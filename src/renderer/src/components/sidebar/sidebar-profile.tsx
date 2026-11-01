import { useNavigate } from "react-router-dom";
import { useAppSelector, useUserDetails } from "@renderer/hooks";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import SteamLogo from "@renderer/assets/steam-logo.svg?react";
import { Avatar } from "../avatar/avatar";

import { platformToSystem } from "@renderer/helpers";

import "./sidebar-profile.scss";

const CLASSIC_DISC_OVERLAY = (
  <svg
    className="sidebar-profile__classic-disc-overlay"
    viewBox="0 0 36 36"
    aria-hidden="true"
  >
    <circle
      cx="18"
      cy="18"
      r="17.4"
      fill="none"
      stroke="rgba(255, 255, 255, 0.58)"
      strokeWidth="0.8"
    />
    <circle
      cx="18"
      cy="18"
      r="12.8"
      fill="none"
      stroke="rgba(255, 255, 255, 0.14)"
      strokeWidth="0.55"
    />
    <circle
      cx="18"
      cy="18"
      r="9.8"
      fill="none"
      stroke="rgba(255, 255, 255, 0.1)"
      strokeWidth="0.45"
    />
    <path
      d="M18 1a17 17 0 0 1 12.5 5.5l-8 7.4A6 6 0 0 0 18 12Z"
      fill="rgba(255, 255, 255, 0.15)"
    />
    <path
      d="M18 35a17 17 0 0 1-12.5-5.5l8-7.4A6 6 0 0 0 18 24Z"
      fill="rgba(255, 255, 255, 0.08)"
    />
    <circle
      cx="18"
      cy="18"
      r="3.2"
      fill="rgba(8, 9, 12, 0.58)"
      stroke="rgba(255, 255, 255, 0.48)"
      strokeWidth="0.65"
    />
    <circle
      cx="18"
      cy="18"
      r="1.35"
      fill="rgba(8, 9, 12, 0.92)"
      stroke="rgba(255, 255, 255, 0.34)"
      strokeWidth="0.4"
    />
  </svg>
);

interface ClassicGameDiscProps {
  iconUrl: string | null;
}

function ClassicGameDisc({ iconUrl }: Readonly<ClassicGameDiscProps>) {
  const [hasArtworkError, setHasArtworkError] = useState(false);

  return (
    <span className="sidebar-profile__classic-disc" aria-hidden="true">
      {iconUrl && !hasArtworkError ? (
        <img
          className="sidebar-profile__classic-disc-artwork"
          src={iconUrl}
          alt=""
          onError={() => setHasArtworkError(true)}
        />
      ) : null}
      {CLASSIC_DISC_OVERLAY}
    </span>
  );
}

export function SidebarProfile() {
  const navigate = useNavigate();

  const { t } = useTranslation(["sidebar", "user_profile"]);

  const { userDetails } = useUserDetails();

  const { gameRunning } = useAppSelector((state) => state.gameRunning);
  const library = useAppSelector((state) => state.library.value);

  const isPlayStationGameRunning = useMemo(() => {
    if (gameRunning?.shop !== "launchbox") return false;

    const runningGame = library.find(
      (game) =>
        game.shop === gameRunning.shop && game.objectId === gameRunning.objectId
    );

    return platformToSystem(runningGame?.platform) !== null;
  }, [gameRunning, library]);

  // Signed out, the Friends page holds the sign-in form.
  const handleProfileClick = () => {
    navigate(userDetails ? `/profile/${userDetails.id}` : "/friends");
  };

  const gameRunningDetails = () => {
    if (!userDetails || !gameRunning) return null;

    if (isPlayStationGameRunning) {
      return (
        <ClassicGameDisc
          key={`${gameRunning.shop}:${gameRunning.objectId}`}
          iconUrl={gameRunning.customIconUrl ?? gameRunning.iconUrl}
        />
      );
    }

    if (gameRunning.iconUrl) {
      return (
        <img
          className="sidebar-profile__game-running-icon"
          alt={gameRunning.title}
          width={24}
          src={gameRunning.iconUrl}
        />
      );
    }

    return <SteamLogo />;
  };

  return (
    <div className="sidebar-profile">
      <button
        type="button"
        className="sidebar-profile__button"
        onClick={handleProfileClick}
        aria-label={userDetails ? t("user_profile:see_profile") : t("sign_in")}
      >
        <div className="sidebar-profile__button-content">
          <Avatar
            size={35}
            src={userDetails?.profileImageUrl}
            alt={userDetails?.displayName}
          />

          <div className="sidebar-profile__button-information">
            <p className="sidebar-profile__button-title">
              {userDetails ? userDetails.displayName : t("sign_in")}
            </p>

            {userDetails && gameRunning && (
              <div className="sidebar-profile__button-game-running-title">
                <small>{gameRunning.title}</small>
              </div>
            )}
          </div>

          {gameRunningDetails()}
        </div>
      </button>
    </div>
  );
}
