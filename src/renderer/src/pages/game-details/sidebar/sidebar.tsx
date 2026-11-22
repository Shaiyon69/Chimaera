import { findBundledGame } from "@shared";
import { lazy, Suspense, useContext, useEffect, useState } from "react";
import type {
  HowLongToBeatCategory,
  SteamAppDetails,
  UserAchievement,
} from "@types";
import { useTranslation } from "react-i18next";
import { Button } from "@renderer/components/button/button";
import { Link } from "@renderer/components/link/link";
import { StarRating } from "@renderer/components/star-rating/star-rating";

import { gameDetailsContext } from "@renderer/context";
import {
  useAppSelector,
  useDate,
  useFormat,
  useProtonDB,
  useUserDetails,
} from "@renderer/hooks";
import {
  DownloadIcon,
  LockIcon,
  PeopleIcon,
  StarIcon,
} from "@primer/octicons-react";
import { HowLongToBeatSection } from "./how-long-to-beat-section";
import { LaunchboxDetailsSection } from "./launchbox-details-section";
import { SidebarSection } from "../sidebar-section/sidebar-section";
import { buildGameAchievementPath } from "@renderer/helpers";
import { RetroAchievementsConnectBanner } from "@renderer/components/retro-achievements-connect-banner/retro-achievements-connect-banner";
import "./sidebar.scss";
import { GameLanguageSection } from "./game-language-section";
import { ControllerSupportSection } from "./controller-support-section";
import { CompanionSection } from "../companion/companion-section";

const ProtonDBSection = lazy(async () => {
  const mod = await import("./protondb-section");
  return { default: mod.ProtonDBSection };
});

const achievementsPlaceholder: UserAchievement[] = [
  {
    displayName: "Timber!!",
    name: "1",
    hidden: false,
    description: "Chop down your first tree.",
    icon: "https://cdn.akamai.steamstatic.com/steamcommunity/public/images/apps/105600/0fbb33098c9da39d1d4771d8209afface9c46e81.jpg",
    icongray:
      "https://cdn.akamai.steamstatic.com/steamcommunity/public/images/apps/105600/0fbb33098c9da39d1d4771d8209afface9c46e81.jpg",
    unlocked: true,
    unlockTime: Date.now(),
  },
  {
    displayName: "Supreme Helper Minion!",
    name: "2",
    hidden: false,
    icon: "https://cdn.akamai.steamstatic.com/steamcommunity/public/images/apps/105600/0a6ff6a36670c96ceb4d30cf6fd69d2fdf55f38e.jpg",
    icongray:
      "https://cdn.akamai.steamstatic.com/steamcommunity/public/images/apps/105600/0a6ff6a36670c96ceb4d30cf6fd69d2fdf55f38e.jpg",
    unlocked: false,
    unlockTime: null,
  },
  {
    displayName: "Feast of Midas",
    name: "3",
    hidden: false,
    icon: "https://cdn.akamai.steamstatic.com/steamcommunity/public/images/apps/105600/2d10311274fe7c92ab25cc29afdca86b019ad472.jpg",
    icongray:
      "https://cdn.akamai.steamstatic.com/steamcommunity/public/images/apps/105600/2d10311274fe7c92ab25cc29afdca86b019ad472.jpg",
    unlocked: false,
    unlockTime: null,
  },
];

export function Sidebar() {
  const [howLongToBeat, setHowLongToBeat] = useState<{
    isLoading: boolean;
    data: HowLongToBeatCategory[] | null;
  }>({ isLoading: true, data: null });

  const { userDetails } = useUserDetails();
  const [activeRequirement, setActiveRequirement] =
    useState<keyof SteamAppDetails["pc_requirements"]>("minimum");

  const { gameTitle, shopDetails, objectId, shop, stats, achievements } =
    useContext(gameDetailsContext);
  const protonDB = useProtonDB(shop, objectId ?? undefined);
  const userPreferences = useAppSelector(
    (state) => state.userPreferences.value
  );

  const { t } = useTranslation("game_details");
  const { formatDateTime } = useDate();
  const { numberFormatter } = useFormat();
  const achievementsCount = achievements?.length ?? 0;
  const localAchievements = !!findBundledGame({ shop, objectId })?.achievements;
  const shouldRenderAchievementsSection =
    ((!!userDetails || localAchievements) && achievementsCount > 0) ||
    (shop === "launchbox" &&
      !!shopDetails?.retroAchievementsGameId &&
      !userPreferences?.retroAchievementsWebApiKey);

  useEffect(() => {
    if (objectId) {
      setHowLongToBeat({ isLoading: true, data: null });

      // Directly fetch from API without checking cache
      window.electron.hydraApi
        .get<HowLongToBeatCategory[] | null>(
          `/games/${shop}/${objectId}/how-long-to-beat`,
          {
            needsAuth: false,
          }
        )
        .then((howLongToBeatData) => {
          setHowLongToBeat({
            isLoading: false,
            data: Array.isArray(howLongToBeatData) ? howLongToBeatData : null,
          });
        })
        .catch(() => {
          setHowLongToBeat({ isLoading: false, data: null });
        });
    }
  }, [objectId, shop]);

  return (
    <aside className="content-sidebar">
      <CompanionSection shop={shop} objectId={objectId} />
      <Suspense fallback={null}>
        <ProtonDBSection
          protonDBData={protonDB.data}
          isLoading={protonDB.isLoading}
          objectId={objectId ?? ""}
        />
      </Suspense>

      {userDetails === null && !shouldRenderAchievementsSection && (
        <SidebarSection title={t("achievements")}>
          <div className="achievements-placeholder">
            <LockIcon size={36} />
            <h3>{t("sign_in_to_see_achievements")}</h3>
          </div>
          <ul className="list achievements-placeholder__blur">
            {achievementsPlaceholder.map((achievement) => (
              <li key={achievement.name}>
                <div className="list__item">
                  <img
                    className={`list__item-image achievements-placeholder__blur ${
                      achievement.unlocked ? "" : "list__item-image--locked"
                    }`}
                    src={achievement.icon}
                    alt={achievement.displayName}
                  />
                  <div>
                    <p>{achievement.displayName}</p>
                    <small>
                      {achievement.unlockTime != null &&
                        formatDateTime(achievement.unlockTime)}
                    </small>
                  </div>
                </div>
              </li>
            ))}
          </ul>
        </SidebarSection>
      )}

      {shouldRenderAchievementsSection && (
        <SidebarSection
          title={
            achievementsCount > 0
              ? t("achievements_count", {
                  unlockedCount:
                    achievements?.filter((a) => a.unlocked).length ?? 0,
                  achievementsCount,
                })
              : t("achievements")
          }
        >
          <ul className="list">
            <RetroAchievementsConnectBanner />

            {(achievements ?? []).slice(0, 4).map((achievement) => (
              <li key={achievement.displayName}>
                <Link
                  to={buildGameAchievementPath({
                    shop: shop,
                    objectId: objectId!,
                    title: gameTitle,
                  })}
                  className="list__item"
                  title={achievement.description}
                >
                  <img
                    className={`list__item-image ${
                      achievement.unlocked ? "" : "list__item-image--locked"
                    }`}
                    src={achievement.icon}
                    alt={achievement.displayName}
                  />
                  <div>
                    <p>{achievement.displayName}</p>
                    <small>
                      {achievement.unlockTime != null &&
                        formatDateTime(achievement.unlockTime)}
                    </small>
                  </div>
                </Link>
              </li>
            ))}

            {achievementsCount > 0 && (
              <Link
                to={buildGameAchievementPath({
                  shop: shop,
                  objectId: objectId!,
                  title: gameTitle,
                })}
              >
                {t("see_all_achievements")}
              </Link>
            )}
          </ul>
        </SidebarSection>
      )}

      {/* Hydra's stats call can fail or return an empty object; "NaN" rows say nothing. */}
      {typeof stats?.downloadCount === "number" &&
        typeof stats?.playerCount === "number" && (
          <SidebarSection title={t("stats")}>
            <div className="stats__section">
              <div className="stats__category">
                <p className="stats__category-title">
                  <DownloadIcon size={18} />
                  {t("download_count")}
                </p>
                <p>{numberFormatter.format(stats?.downloadCount)}</p>
              </div>

              <div className="stats__category">
                <p className="stats__category-title">
                  <PeopleIcon size={18} />
                  {t("player_count")}
                </p>
                <p>{numberFormatter.format(stats?.playerCount)}</p>
              </div>

              <div className="stats__category">
                <p className="stats__category-title">
                  <StarIcon size={18} />
                  {t("rating_count")}
                </p>
                <StarRating
                  rating={
                    stats?.averageScore === 0
                      ? null
                      : (stats?.averageScore ?? null)
                  }
                  size={16}
                />
              </div>
            </div>
          </SidebarSection>
        )}

      {shop === "launchbox" && (
        <LaunchboxDetailsSection
          platform={shopDetails?.platform}
          genres={shopDetails?.genres?.map((g) => g.name)}
          skus={shopDetails?.skus}
        />
      )}

      <HowLongToBeatSection
        howLongToBeatData={howLongToBeat.data}
        isLoading={howLongToBeat.isLoading}
      />

      <ControllerSupportSection />

      {shop !== "launchbox" && (
        <SidebarSection title={t("requirements")}>
          <div className="requirement__button-container">
            <Button
              className="requirement__button"
              onClick={() => setActiveRequirement("minimum")}
              theme={activeRequirement === "minimum" ? "primary" : "outline"}
            >
              {t("minimum")}
            </Button>

            <Button
              className="requirement__button"
              onClick={() => setActiveRequirement("recommended")}
              theme={
                activeRequirement === "recommended" ? "primary" : "outline"
              }
            >
              {t("recommended")}
            </Button>
          </div>

          <div
            className="requirement__details"
            dangerouslySetInnerHTML={{
              __html:
                shopDetails?.pc_requirements?.[activeRequirement] ??
                t(`no_${activeRequirement}_requirements`, {
                  gameTitle,
                }),
            }}
          />
        </SidebarSection>
      )}

      <GameLanguageSection />
    </aside>
  );
}
