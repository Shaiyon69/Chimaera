import { useCallback, useContext, useMemo } from "react";
import { userProfileContext } from "@renderer/context";
import { useTranslation } from "react-i18next";
import { useFormat } from "@renderer/hooks";
import { MAX_MINUTES_TO_SHOW_IN_PLAYTIME } from "@renderer/constants";
import {
  buildGameAchievementPath,
  buildGameDetailsPath,
  isGameCompleted,
} from "@renderer/helpers";
import { Link } from "@renderer/components";
import { cachedImage } from "@shared";
import {
  ClockIcon,
  GraphIcon,
  PeopleIcon,
  StackIcon,
  TrophyIcon,
  VerifiedIcon,
} from "@primer/octicons-react";
import type { UserGame } from "@types";
import { BadgesBox } from "./badges-box";
import "./user-stats-box.scss";

const TOP_COUNT = 6;

// Pinned games also appear in the library list; keep each once.
export const uniqueGames = (games: UserGame[]) => {
  const seen = new Set<string>();
  return games.filter((game) => {
    const key = `${game.shop}:${game.objectId}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
};

interface StatRow {
  game: UserGame;
  ratio: number;
  value: string;
}

export function UserStatsBox() {
  const { userStats, userProfile, libraryGames, pinnedGames } =
    useContext(userProfileContext);
  const { t } = useTranslation("user_profile");
  const { numberFormatter } = useFormat();

  const formatPlayTime = useCallback(
    (playTimeInSeconds: number) => {
      const minutes = playTimeInSeconds / 60;

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
    () => uniqueGames([...pinnedGames, ...libraryGames]),
    [pinnedGames, libraryGames]
  );

  const completedGames = useMemo(
    () =>
      games
        .filter((game) =>
          isGameCompleted(game.achievementCount, game.unlockedAchievementCount)
        )
        .sort((a, b) => b.achievementCount - a.achievementCount),
    [games]
  );

  const mostPlayed = useMemo<StatRow[]>(() => {
    const top = games
      .filter((game) => game.playTimeInSeconds > 0)
      .sort((a, b) => b.playTimeInSeconds - a.playTimeInSeconds)
      .slice(0, TOP_COUNT);
    const max = top[0]?.playTimeInSeconds ?? 1;
    return top.map((game) => ({
      game,
      ratio: game.playTimeInSeconds / max,
      value: formatPlayTime(game.playTimeInSeconds),
    }));
  }, [games, formatPlayTime]);

  const completion = useMemo<StatRow[]>(
    () =>
      games
        .filter(
          (game) =>
            game.achievementCount > 0 &&
            !isGameCompleted(
              game.achievementCount,
              game.unlockedAchievementCount
            )
        )
        .map((game) => ({
          game,
          ratio: game.unlockedAchievementCount / game.achievementCount,
          value: `${game.unlockedAchievementCount} / ${game.achievementCount}`,
        }))
        .sort(
          (a, b) =>
            b.ratio - a.ratio ||
            b.game.unlockedAchievementCount - a.game.unlockedAchievementCount
        )
        .slice(0, TOP_COUNT),
    [games]
  );

  // shortcut: covers loaded library pages only; switch to a server stat if one appears.
  const averageCompletion = useMemo(() => {
    const tracked = games.filter((game) => game.achievementCount > 0);
    if (tracked.length === 0) return 0;
    const sum = tracked.reduce(
      (total, game) =>
        total +
        Math.min(game.unlockedAchievementCount / game.achievementCount, 1),
      0
    );
    return sum / tracked.length;
  }, [games]);

  if (!userStats) return null;

  const playtime = userStats.totalPlayTimeInSeconds;
  const tiles = [
    {
      key: "games",
      icon: <StackIcon size={22} />,
      label: t("games"),
      value: numberFormatter.format(userStats.libraryCount),
    },
    {
      key: "playtime",
      icon: <ClockIcon size={22} />,
      label: t("total_play_time"),
      value: formatPlayTime(playtime.value),
      chip:
        playtime.topPercentile > 0
          ? t("top_percentile", { percentile: playtime.topPercentile })
          : null,
    },
    {
      key: "achievements",
      icon: <TrophyIcon size={22} />,
      label: t("achievements_unlocked"),
      value: numberFormatter.format(userStats.unlockedAchievementSum ?? 0),
    },
    {
      key: "completed",
      icon: <VerifiedIcon size={22} />,
      label: t("games_completed"),
      value: numberFormatter.format(completedGames.length),
    },
    {
      key: "average",
      icon: <GraphIcon size={22} />,
      label: t("average_completion"),
      value: `${Math.floor(averageCompletion * 100)}%`,
    },
    {
      key: "friends",
      icon: <PeopleIcon size={22} />,
      label: t("friends"),
      value: numberFormatter.format(userStats.friendsCount),
    },
  ];

  const panels = [
    { key: "most_played", title: t("most_played"), rows: mostPlayed },
    {
      key: "achievement_progress",
      title: t("closest_to_completion"),
      rows: completion,
    },
  ].filter((panel) => panel.rows.length > 0);

  return (
    <div className="user-stats">
      <ul className="user-stats__tiles">
        {tiles.map((tile) => (
          <li
            key={tile.key}
            className={`user-stats__tile user-stats__tile--${tile.key}`}
          >
            <span className="user-stats__tile-icon">{tile.icon}</span>
            <span className="user-stats__tile-value">{tile.value}</span>
            <span className="user-stats__tile-label">
              {tile.label}
              {tile.chip && (
                <span className="user-stats__chip">{tile.chip}</span>
              )}
            </span>
          </li>
        ))}
      </ul>

      {panels.length > 0 && (
        <div className="user-stats__panels">
          {panels.map((panel) => (
            <section key={panel.key} className="user-stats__panel">
              <h3 className="user-stats__panel-title">{panel.title}</h3>
              <ol className="user-stats__rows">
                {panel.rows.map(({ game, ratio, value }, index) => {
                  const icon = game.customIconUrl ?? game.iconUrl;
                  return (
                    <li key={`${game.shop}-${game.objectId}`}>
                      <Link
                        to={buildGameDetailsPath(game)}
                        className="user-stats__row"
                      >
                        <span className="user-stats__rank">{index + 1}</span>
                        {icon ? (
                          <img
                            className="user-stats__row-icon"
                            src={cachedImage(icon)}
                            alt=""
                          />
                        ) : (
                          <span
                            className="user-stats__row-icon user-stats__row-icon--empty"
                            aria-hidden="true"
                          >
                            {game.title.charAt(0)}
                          </span>
                        )}
                        <span className="user-stats__row-body">
                          <span className="user-stats__row-head">
                            <span className="user-stats__row-title">
                              {game.title}
                            </span>
                            <span className="user-stats__row-value">
                              {value}
                            </span>
                          </span>
                          <span className="user-stats__bar">
                            <span
                              className="user-stats__bar-fill"
                              style={{ width: `${Math.round(ratio * 100)}%` }}
                            />
                          </span>
                        </span>
                      </Link>
                    </li>
                  );
                })}
              </ol>
            </section>
          ))}
        </div>
      )}

      {completedGames.length > 0 && (
        <section className="user-stats__panel user-stats__panel--wide">
          <h3 className="user-stats__panel-title">
            {t("completed_games")}
            <span className="user-stats__count">
              {numberFormatter.format(completedGames.length)}
            </span>
          </h3>
          <ul className="user-stats__covers">
            {completedGames.map((game) => {
              const cover = game.customLibraryImageUrl ?? game.coverImageUrl;
              return (
                <li key={`${game.shop}-${game.objectId}`}>
                  <Link
                    to={buildGameAchievementPath(
                      game,
                      userProfile ? { userId: userProfile.id } : undefined
                    )}
                    className="user-stats__cover"
                    title={game.title}
                  >
                    {cover ? (
                      <img src={cachedImage(cover)} alt={game.title} />
                    ) : (
                      <span className="user-stats__cover-fallback">
                        {game.title}
                      </span>
                    )}
                    <span className="user-stats__cover-badge">
                      <TrophyIcon size={12} />
                      {numberFormatter.format(game.achievementCount)}
                    </span>
                  </Link>
                </li>
              );
            })}
          </ul>
        </section>
      )}

      {Boolean(userProfile?.badges.length) && (
        <section className="user-stats__panel user-stats__panel--badges">
          <h3 className="user-stats__panel-title">
            {t("badges")}
            <span className="user-stats__count">
              {userProfile?.badges.length}
            </span>
          </h3>
          <BadgesBox />
        </section>
      )}
    </div>
  );
}
