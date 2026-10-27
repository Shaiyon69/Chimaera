import { useId } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { useTranslation } from "react-i18next";
import { Tooltip } from "react-tooltip";
import cn from "classnames";

import { useUserDetails } from "@renderer/hooks";
import { useDmInbox } from "@renderer/pages/friends/messapp/use-dm-inbox";
import { useUnreadPeers } from "@renderer/pages/friends/messapp/unread";
import { routes } from "../sidebar/routes";
import { SidebarProfile } from "../sidebar/sidebar-profile";
import "./nav-dock.scss";

export function NavDock() {
  const { t } = useTranslation("sidebar");
  const location = useLocation();
  const navigate = useNavigate();
  const tooltipId = useId();
  const { t: tMessapp } = useTranslation("messapp");
  const { userDetails } = useUserDetails();
  const hasUnread = useUnreadPeers().size > 0;

  useDmInbox();

  return (
    <nav className="nav-dock">
      <SidebarProfile />
      {routes
        .filter(({ requiresAuth }) => !requiresAuth || userDetails)
        .map(({ nameKey, path, render }) => (
          <button
            key={nameKey}
            type="button"
            aria-label={t(nameKey)}
            data-tooltip-id={tooltipId}
            data-tooltip-content={t(nameKey)}
            className={cn("nav-dock__button", {
              "nav-dock__button--active": location.pathname === path,
            })}
            onClick={() => {
              if (path !== location.pathname) navigate(path);
            }}
          >
            {render()}
            {path === "/friends" && hasUnread && (
              <span
                className="nav-dock__badge"
                role="img"
                aria-label={tMessapp("unread")}
              />
            )}
          </button>
        ))}
      <Tooltip id={tooltipId} place="right" />
    </nav>
  );
}
