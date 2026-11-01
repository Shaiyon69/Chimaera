import { useContext, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import Skeleton from "react-loading-skeleton";
import {
  DeviceDesktopIcon,
  StackIcon,
  TelescopeIcon,
  TrophyIcon,
} from "@primer/octicons-react";
import { userProfileContext } from "@renderer/context";
import { Link } from "@renderer/components";
import { useFillScrollTarget, useFormat } from "@renderer/hooks";
import { buildGameAchievementPath, isGameCompleted } from "@renderer/helpers";
import { ClassicsIcon } from "@renderer/pages/library/category-filter";
import {
  hasSteamLibraryGames,
  isSteamLibraryGame,
} from "@renderer/pages/library/library-category";
import { cachedImage } from "@shared";
import type { UserGame } from "@types";
import { FilterDropdown, type FilterDropdownOption } from "./filter-dropdown";
import type { ProfilePlatform } from "./library-tab";
import { UserStatsBox, uniqueGames } from "./user-stats-box";
import "./achievements-tab.scss";

type CompletionFilter = "all" | "in_progress" | "completed";

const FILTERS: { id: CompletionFilter; labelKey: string }[] = [
  { id: "all", labelKey: "achievements_filter_all" },
  { id: "in_progress", labelKey: "achievements_filter_in_progress" },
  { id: "completed", labelKey: "achievements_filter_completed" },
];

interface AchievementsTabProps {
  platform: ProfilePlatform;
  onPlatformChange: (platform: ProfilePlatform) => void;
  pinnedGames: UserGame[];
  libraryGames: UserGame[];
  hasMoreLibraryGames: boolean;
  isAwaitingInitialLibrary: boolean;
  onLoadMore: () => void;
}

export function AchievementsTab({
  platform,
  onPlatformChange,
  pinnedGames,
  libraryGames,
  hasMoreLibraryGames,
  isAwaitingInitialLibrary,
  onLoadMore,
}: Readonly<AchievementsTabProps>) {
  const { t } = useTranslation("user_profile");
  const { numberFormatter } = useFormat();
  const { userProfile } = useContext(userProfileContext);
  const [filter, setFilter] = useState<CompletionFilter>("all");
  const [steamOnly, setSteamOnly] = useState(false);

  useFillScrollTarget(
    "scrollableDiv",
    libraryGames.length,
    hasMoreLibraryGames,
    onLoadMore
  );

  const withAchievements = useMemo(
    () =>
      uniqueGames([...pinnedGames, ...libraryGames]).filter(
        (game) => game.achievementCount > 0
      ),
    [pinnedGames, libraryGames]
  );
  const hasSteamGames = hasSteamLibraryGames(withAchievements);
  const isSteamOnly = steamOnly && hasSteamGames;

  // Most complete first, so the list reads as a progress board, not a library.
  const games = useMemo(
    () =>
      withAchievements
        .filter((game) => !isSteamOnly || isSteamLibraryGame(game))
        .map((game) => ({
          game,
          ratio: Math.min(
            game.unlockedAchievementCount / game.achievementCount,
            1
          ),
          completed: isGameCompleted(
            game.achievementCount,
            game.unlockedAchievementCount
          ),
        }))
        .sort(
          (a, b) =>
            b.ratio - a.ratio ||
            b.game.unlockedAchievementCount - a.game.unlockedAchievementCount
        ),
    [withAchievements, isSteamOnly]
  );

  const completedCount = games.filter((entry) => entry.completed).length;
  const counts: Record<CompletionFilter, number> = {
    all: games.length,
    in_progress: games.length - completedCount,
    completed: completedCount,
  };
  const visible = games.filter(
    (entry) => filter === "all" || (filter === "completed") === entry.completed
  );

  const platformOptions: FilterDropdownOption<ProfilePlatform>[] = [
    { value: "all", label: t("platform_all"), icon: StackIcon },
    { value: "pc", label: t("platform_pc"), icon: DeviceDesktopIcon },
    { value: "classics", label: t("platform_classics"), icon: ClassicsIcon },
  ];

  return (
    <div key="achievements" className="profile-content__tab-panel">
      <UserStatsBox />

      <div className="achievements-tab__toolbar">
        <div className="achievements-tab__segments" role="group">
          {FILTERS.map(({ id, labelKey }) => (
            <button
              key={id}
              type="button"
              aria-pressed={filter === id}
              className={`achievements-tab__segment${filter === id ? " achievements-tab__segment--active" : ""}`}
              onClick={() => setFilter(id)}
            >
              {t(labelKey)}
              <span className="achievements-tab__segment-count">
                {numberFormatter.format(counts[id])}
              </span>
            </button>
          ))}
        </div>
        <div className="achievements-tab__toolbar-end">
          {hasSteamGames && (
            <div className="achievements-tab__segments">
              <button
                type="button"
                aria-pressed={isSteamOnly}
                className={`achievements-tab__segment${isSteamOnly ? " achievements-tab__segment--active" : ""}`}
                onClick={() => setSteamOnly((value) => !value)}
              >
                {t("achievements_filter_steam")}
              </button>
            </div>
          )}
          <FilterDropdown
            placeholder={t("platform")}
            value={platform}
            options={platformOptions}
            onChange={onPlatformChange}
          />
        </div>
      </div>

      {isAwaitingInitialLibrary && (
        <ul className="achievements-tab__list">
          {Array.from({ length: 6 }).map((_, index) => (
            <li key={index}>
              <Skeleton className="achievements-tab__skeleton" />
            </li>
          ))}
        </ul>
      )}

      {!isAwaitingInitialLibrary && visible.length === 0 && (
        <div className="profile-content__no-games">
          <div className="profile-content__telescope-icon">
            <TelescopeIcon size={24} />
          </div>
          <h2>{t("no_recent_activity_title")}</h2>
          <p>
            {filter === "completed"
              ? t("no_completed_games_description")
              : t("no_achievement_games_description")}
          </p>
        </div>
      )}

      {!isAwaitingInitialLibrary && visible.length > 0 && (
        <ul className="achievements-tab__list">
          {visible.map(({ game, ratio, completed }) => {
            const art =
              game.customLibraryHeroImageUrl ??
              game.libraryHeroImageUrl ??
              game.customLibraryImageUrl ??
              game.coverImageUrl;
            return (
              <li key={`${game.shop}-${game.objectId}`}>
                <Link
                  to={buildGameAchievementPath(
                    game,
                    userProfile ? { userId: userProfile.id } : undefined
                  )}
                  className={`achievements-tab__row${completed ? " achievements-tab__row--completed" : ""}`}
                >
                  {art && (
                    <img
                      className="achievements-tab__row-art"
                      src={cachedImage(art)}
                      alt=""
                      loading="lazy"
                    />
                  )}
                  <span className="achievements-tab__row-body">
                    <span className="achievements-tab__row-head">
                      <span className="achievements-tab__row-title">
                        {game.title}
                      </span>
                      {completed ? (
                        <span className="achievements-tab__badge">
                          <TrophyIcon size={12} />
                          {t("completed_badge")}
                        </span>
                      ) : (
                        <span className="achievements-tab__percent">
                          {Math.floor(ratio * 100)}%
                        </span>
                      )}
                    </span>
                    <span className="achievements-tab__row-count">
                      {t("achievement_count", {
                        unlocked: numberFormatter.format(
                          game.unlockedAchievementCount
                        ),
                        total: numberFormatter.format(game.achievementCount),
                      })}
                    </span>
                    <span className="achievements-tab__bar">
                      <span
                        className="achievements-tab__bar-fill"
                        style={{ width: `${Math.floor(ratio * 100)}%` }}
                      />
                    </span>
                  </span>
                </Link>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
