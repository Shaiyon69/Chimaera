import { cachedImage } from "@shared";
import { buildGameDetailsPath } from "@renderer/helpers";
import { Link } from "@renderer/components";
import { ArtFallback } from "@renderer/components/art-fallback/art-fallback";
import { useCallback, useContext, useMemo, useState } from "react";
import { userProfileContext } from "@renderer/context";
import { useTranslation } from "react-i18next";
import { ClockIcon, TrophyIcon } from "@primer/octicons-react";
import { useDate, useFormat } from "@renderer/hooks";
import type { UserGame } from "@types";
import { MAX_MINUTES_TO_SHOW_IN_PLAYTIME } from "@renderer/constants";
import { GameVisibilityBadge } from "@renderer/components/game-visibility-badge/game-visibility-badge";
import "./recent-games-box.scss";

const playedAt = (game: UserGame) =>
  game.lastTimePlayed ? new Date(game.lastTimePlayed).getTime() : 0;

function ActivityArt({ game }: Readonly<{ game: UserGame }>) {
  const [hasError, setHasError] = useState(false);
  const src =
    game.customLibraryHeroImageUrl ??
    game.libraryHeroImageUrl ??
    game.customLibraryImageUrl ??
    game.coverImageUrl;

  if (!src || hasError) {
    return (
      <ArtFallback
        title={game.title}
        variant="banner"
        className="recent-games__art"
      />
    );
  }

  return (
    <img
      className="recent-games__art"
      src={cachedImage(src)}
      alt=""
      onError={() => setHasError(true)}
    />
  );
}

export function RecentGamesBox() {
  const { userProfile } = useContext(userProfileContext);
  const { t } = useTranslation("user_profile");
  const { numberFormatter } = useFormat();
  const { formatDistance } = useDate();

  const formatPlayTime = useCallback(
    (game: UserGame) => {
      const minutes = (game?.playTimeInSeconds || 0) / 60;

      if (minutes < MAX_MINUTES_TO_SHOW_IN_PLAYTIME) {
        return t("amount_minutes", {
          amount: minutes.toFixed(0),
        });
      }

      const hours = minutes / 60;
      return t("amount_hours", { amount: numberFormatter.format(hours) });
    },
    [numberFormatter, t]
  );

  const games = useMemo(
    () =>
      [...(userProfile?.recentGames ?? [])].sort(
        (a, b) => playedAt(b) - playedAt(a)
      ),
    [userProfile?.recentGames]
  );

  if (!games.length) return null;

  const [featured, ...rest] = games;

  const meta = (game: UserGame) => (
    <span className="recent-games__meta">
      <GameVisibilityBadge
        isHiddenFromOthers={game.isHiddenFromOthers}
        isConcealed={game.isConcealed}
      />
      <span className="recent-games__meta-item">
        <ClockIcon size={16} />
        {formatPlayTime(game)}
      </span>
      {game.achievementCount > 0 && (
        <span className="recent-games__meta-item">
          <TrophyIcon size={16} />
          {game.unlockedAchievementCount} / {game.achievementCount}
        </span>
      )}
    </span>
  );

  const lastPlayed = (game: UserGame) =>
    game.lastTimePlayed
      ? t("last_time_played", {
          period: formatDistance(game.lastTimePlayed, new Date(), {
            addSuffix: true,
          }),
        })
      : t("played_recently");

  const featuredLogo = featured.customLogoImageUrl ?? featured.logoImageUrl;
  const featuredProgress =
    featured.achievementCount > 0
      ? featured.unlockedAchievementCount / featured.achievementCount
      : null;

  return (
    <div className="recent-games">
      <Link
        to={buildGameDetailsPath(featured)}
        className="recent-games__featured"
      >
        <ActivityArt game={featured} />
        <span className="recent-games__featured-shade" aria-hidden="true" />
        <span className="recent-games__featured-content">
          <span className="recent-games__eyebrow">{lastPlayed(featured)}</span>
          {featuredLogo ? (
            <img
              className="recent-games__featured-logo"
              src={cachedImage(featuredLogo)}
              alt={featured.title}
            />
          ) : (
            <span className="recent-games__featured-title">
              {featured.title}
            </span>
          )}
          {meta(featured)}
          {featuredProgress !== null && (
            <span className="recent-games__bar">
              <span
                className="recent-games__bar-fill"
                style={{ width: `${Math.round(featuredProgress * 100)}%` }}
              />
            </span>
          )}
        </span>
      </Link>

      {rest.length > 0 && (
        <ul className="recent-games__grid">
          {rest.map((game) => (
            <li key={`${game.shop}-${game.objectId}`}>
              <Link
                to={buildGameDetailsPath(game)}
                className="recent-games__card"
              >
                <ActivityArt game={game} />
                <span className="recent-games__card-body">
                  <span className="recent-games__card-title">{game.title}</span>
                  <span className="recent-games__card-when">
                    {lastPlayed(game)}
                  </span>
                  {meta(game)}
                </span>
              </Link>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
