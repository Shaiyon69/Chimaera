import {
  useCallback,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { Tooltip } from "react-tooltip";
import cn from "classnames";

import { useUserDetails } from "@renderer/hooks";
import { useDmInbox } from "@renderer/pages/friends/messapp/use-dm-inbox";
import { useUnreadPeers } from "@renderer/pages/friends/messapp/unread";
import { routes } from "../sidebar/routes";
import { SidebarProfile } from "../sidebar/sidebar-profile";
import { BellIcon, GearIcon } from "@primer/octicons-react";
import { useNotificationCount } from "./use-notification-count";
import "./nav-dock.scss";

// Movement past this many pixels turns a press into a drag; a smaller wobble stays a click.
const DRAG_THRESHOLD_PX = 4;

type Indicator = { top: number; height: number; tint: string } | null;

export function NavDock() {
  const { t } = useTranslation("sidebar");
  const location = useLocation();
  const navigate = useNavigate();
  const tooltipId = useId();
  const { t: tMessapp } = useTranslation("messapp");
  const { userDetails } = useUserDetails();
  const hasUnread = useUnreadPeers().size > 0;
  const notificationCount = useNotificationCount();

  const dockRef = useRef<HTMLElement>(null);
  const indicatorRef = useRef<HTMLSpanElement>(null);
  const frame = useRef(0);
  const drag = useRef<{ startY: number; moved: boolean } | null>(null);
  const suppressClick = useRef(false);
  const [previewPath, setPreviewPath] = useState<string | null>(null);
  const [isDragging, setIsDragging] = useState(false);
  const [indicator, setIndicator] = useState<Indicator>(null);

  useDmInbox();

  // The box sits under the page being dragged over, or else the current page.
  const shownPath = previewPath ?? location.pathname;

  const measure = useCallback(() => {
    const button = dockRef.current?.querySelector<HTMLElement>(
      `[data-dock-path="${CSS.escape(shownPath)}"]`
    );
    setIndicator(
      button
        ? {
            top: button.offsetTop,
            height: button.offsetHeight,
            tint: button.style.getPropertyValue("--tint"),
          }
        : null
    );
  }, [shownPath]);

  useLayoutEffect(() => {
    measure();
    const dock = dockRef.current;
    if (!dock) return;
    // Buttons appear and disappear with sign-in; re-measure when the dock's size changes.
    const observer = new ResizeObserver(measure);
    observer.observe(dock);
    return () => observer.disconnect();
  }, [measure, userDetails]);

  const pathAt = (clientY: number) => {
    const buttons =
      dockRef.current?.querySelectorAll<HTMLElement>("[data-dock-path]");
    let closest: { path: string; distance: number } | null = null;
    buttons?.forEach((button) => {
      const box = button.getBoundingClientRect();
      const distance = Math.abs(clientY - (box.top + box.height / 2));
      if (!closest || distance < closest.distance) {
        closest = { path: button.dataset.dockPath ?? "", distance };
      }
    });
    return (closest as { path: string } | null)?.path ?? null;
  };

  const onPointerDown = (event: ReactPointerEvent<HTMLElement>) => {
    if (event.button !== 0) return;
    if (!(event.target as HTMLElement).closest("[data-dock-path]")) return;
    drag.current = { startY: event.clientY, moved: false };
  };

  const onPointerMove = (event: ReactPointerEvent<HTMLElement>) => {
    const state = drag.current;
    if (!state) return;
    if (
      !state.moved &&
      Math.abs(event.clientY - state.startY) < DRAG_THRESHOLD_PX
    ) {
      return;
    }
    if (!state.moved) {
      state.moved = true;
      setIsDragging(true);
      dockRef.current?.setPointerCapture(event.pointerId);
    }
    setPreviewPath(pathAt(event.clientY));

    // The box follows the pointer 1:1 between the first and last item, written straight to the
    // element once per frame so React never re-renders mid-drag.
    const dock = dockRef.current;
    const box = indicatorRef.current;
    if (!dock || !box) return;
    const buttons = dock.querySelectorAll<HTMLElement>("[data-dock-path]");
    const first = buttons[0];
    const last = buttons[buttons.length - 1];
    if (!first || !last) return;
    const y = Math.min(
      Math.max(
        event.clientY -
          dock.getBoundingClientRect().top -
          first.offsetHeight / 2,
        first.offsetTop
      ),
      last.offsetTop
    );
    cancelAnimationFrame(frame.current);
    frame.current = requestAnimationFrame(() => {
      box.style.transform = `translate3d(0, ${y}px, 0)`;
    });
  };

  const endDrag = (event: ReactPointerEvent<HTMLElement>, commit: boolean) => {
    const state = drag.current;
    drag.current = null;
    if (!state?.moved) return;
    dockRef.current?.releasePointerCapture(event.pointerId);
    const target = pathAt(event.clientY);
    cancelAnimationFrame(frame.current);
    setIsDragging(false);
    setPreviewPath(null);
    // The click that follows a drag must not navigate a second time. It fires in the same
    // task as pointerup (or not at all under pointer capture), so the flag clears right after.
    suppressClick.current = true;
    setTimeout(() => {
      suppressClick.current = false;
    }, 0);
    if (commit && target && target !== location.pathname) navigate(target);
  };

  const dockButton = (
    path: string,
    label: string,
    icon: ReactNode,
    tint: string,
    badge?: ReactNode
  ) => (
    <button
      key={path}
      type="button"
      data-dock-path={path}
      style={{ "--tint": tint } as CSSProperties}
      aria-label={label}
      aria-current={location.pathname === path ? "page" : undefined}
      data-tooltip-id={tooltipId}
      data-tooltip-content={label}
      className={cn("nav-dock__button", {
        "nav-dock__button--active": shownPath === path,
      })}
      onClick={() => {
        if (suppressClick.current) {
          suppressClick.current = false;
          return;
        }
        if (path !== location.pathname) navigate(path);
      }}
    >
      {icon}
      {badge}
    </button>
  );

  return (
    <nav
      ref={dockRef}
      className={cn("nav-dock", { "nav-dock--dragging": isDragging })}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={(event) => endDrag(event, true)}
      onPointerCancel={(event) => endDrag(event, false)}
    >
      <span
        ref={indicatorRef}
        className="nav-dock__indicator"
        aria-hidden="true"
        style={
          indicator
            ? ({
                // While dragging the pointer handler owns the position; React only sets it at rest.
                ...(isDragging
                  ? {}
                  : { transform: `translate3d(0, ${indicator.top}px, 0)` }),
                height: indicator.height,
                "--tint": indicator.tint,
              } as CSSProperties)
            : { opacity: 0 }
        }
      />
      <SidebarProfile />
      {routes
        .filter(({ requiresAuth }) => !requiresAuth || userDetails)
        .map(({ nameKey, path, render, tint }) =>
          dockButton(
            path,
            t(nameKey),
            render(),
            tint,
            path === "/friends" && hasUnread && (
              <span
                className="nav-dock__badge"
                role="img"
                aria-label={tMessapp("unread")}
              />
            )
          )
        )}
      <span className="nav-dock__divider" aria-hidden="true" />
      {dockButton(
        "/notifications",
        t("notifications"),
        <BellIcon />,
        "#e5676f",
        notificationCount > 0 && (
          <span className="nav-dock__count">
            {notificationCount > 99 ? "99+" : notificationCount}
          </span>
        )
      )}
      {dockButton("/settings", t("settings"), <GearIcon />, "#9fb3c8")}
      <Tooltip id={tooltipId} place="right" hidden={isDragging} />
    </nav>
  );
}
