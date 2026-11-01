import { useId, type ReactNode } from "react";
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

export function NavDock() {
  const { t } = useTranslation("sidebar");
  const location = useLocation();
  const navigate = useNavigate();
  const tooltipId = useId();
  const { t: tMessapp } = useTranslation("messapp");
  const { userDetails } = useUserDetails();
  const hasUnread = useUnreadPeers().size > 0;
  const notificationCount = useNotificationCount();

  useDmInbox();

  const dockButton = (
    path: string,
    label: string,
    icon: ReactNode,
    badge?: ReactNode
  ) => (
    <button
      key={path}
      type="button"
      aria-label={label}
      data-tooltip-id={tooltipId}
      data-tooltip-content={label}
      className={cn("nav-dock__button", {
        "nav-dock__button--active": location.pathname === path,
      })}
      onClick={() => {
        if (path !== location.pathname) navigate(path);
      }}
    >
      {icon}
      {badge}
    </button>
  );

  return (
    <nav className="nav-dock">
      <SidebarProfile />
      {routes
        .filter(({ requiresAuth }) => !requiresAuth || userDetails)
        .map(({ nameKey, path, render }) =>
          dockButton(
            path,
            t(nameKey),
            render(),
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
        notificationCount > 0 && (
          <span className="nav-dock__count">
            {notificationCount > 99 ? "99+" : notificationCount}
          </span>
        )
      )}
      {dockButton("/settings", t("settings"), <GearIcon />)}
      <Tooltip id={tooltipId} place="right" />
    </nav>
  );
}
