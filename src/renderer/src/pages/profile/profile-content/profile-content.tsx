import { userProfileContext } from "@renderer/context";
import {
  getProfileLibraryFilter,
  readStoredProfilePlatform,
  readStoredProfileSort,
  readStoredSouvenirSort,
} from "@renderer/helpers";
import {
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { ProfileHero } from "../profile-hero/profile-hero";
import {
  useAppDispatch,
  useAppSelector,
  useUserDetails,
} from "@renderer/hooks";
import { setHeaderTitle } from "@renderer/features";
import { useTranslation } from "react-i18next";
import {
  findSouvenirByNotificationTarget,
  isAchievementSouvenirsEnabled,
  useSouvenirContentWarning,
} from "@shared";
import { LockedProfile } from "./locked-profile";
import { ReportProfile } from "../report-profile/report-profile";
import { BadgesBox } from "./badges-box";
import { RecentGamesBox } from "./recent-games-box";
import { UserStatsBox } from "./user-stats-box";
import { ProfileSection } from "../profile-section/profile-section";
import { ProfileTabs, type ProfileTabType } from "./profile-tabs";
import { LibraryTab } from "./library-tab";
import { SouvenirsTab } from "./souvenirs-tab";
import { SouvenirLightbox } from "./souvenir-lightbox";
import { useSouvenirActions } from "./use-souvenir-actions";
import type { ProfilePlatform } from "./library-tab";
import { AnimatePresence } from "framer-motion";
import { useNavigate, useSearchParams } from "react-router-dom";
import { ConfirmationModal } from "@renderer/components";
import "./profile-content.scss";

type SortOption = "playtime" | "achievementCount" | "playedRecently";

export function ProfileContent() {
  const {
    userProfile,
    isMe,
    userStats,
    libraryGames,
    pinnedGames,
    getUserStats,
    getUserProfile,
    getUserLibraryGames,
    loadMoreLibraryGames,
    hasMoreLibraryGames,
    isLoadingLibraryGames,
    souvenirs,
    souvenirsTotal,
    hasReachedSouvenirLimit,
    souvenirsHiddenReason,
    hasMoreSouvenirs,
    isLoadingSouvenirs,
    getUserSouvenirs,
    loadMoreSouvenirs,
    updateSouvenir,
    removeSouvenir,
    loadedLibrarySortBy,
  } = useContext(userProfileContext);
  const { userDetails } = useUserDetails();
  const navigate = useNavigate();
  const [searchParams, setSearchParams] = useSearchParams();
  const requestedTab = searchParams.get("tab");
  const requestedSouvenir = searchParams.get("souvenir");
  const attemptedDeepLinkPagesRef = useRef(new Set<string>());
  const souvenirsEnabled = useAppSelector((state) =>
    isAchievementSouvenirsEnabled(
      state.userPreferences.value?.enableAchievementSouvenirs,
      window.electron.platform
    )
  );
  const disableNsfwAlert = useAppSelector(
    (state) => state.userPreferences.value?.disableNsfwAlert === true
  );
  const [sortBy, setSortBy] = useState<SortOption>(readStoredProfileSort);

  const prefetchedSortBy = useRef(readStoredProfileSort()).current;

  const handleSortChange = useCallback((nextSortBy: SortOption) => {
    setSortBy(nextSortBy);
    localStorage.setItem("profile-sort-by", nextSortBy);
  }, []);
  const [platform, setPlatform] = useState<ProfilePlatform>(
    readStoredProfilePlatform
  );

  const handlePlatformChange = useCallback((nextPlatform: ProfilePlatform) => {
    setPlatform(nextPlatform);
    localStorage.setItem("profile-platform", nextPlatform);
  }, []);
  const effectiveSortBy =
    !userProfile?.hasActiveSubscription && sortBy === "achievementCount"
      ? "playedRecently"
      : sortBy;

  const isCorrectingPrefetchedSort =
    Boolean(userProfile) &&
    sortBy === prefetchedSortBy &&
    effectiveSortBy !== prefetchedSortBy &&
    loadedLibrarySortBy !== effectiveSortBy;

  const libraryFilter = useMemo(
    () => getProfileLibraryFilter(platform),
    [platform]
  );

  const [activeTab, setActiveTab] = useState<ProfileTabType>(
    requestedTab === "souvenirs" ? "souvenirs" : "library"
  );

  const dispatch = useAppDispatch();

  const { t } = useTranslation("user_profile");
  const {
    likingKeys,
    visibilityKeys,
    deletingKeys,
    reportingKeys,
    reportedKeys,
    likeSouvenir,
    changeSouvenirVisibility,
    deleteSouvenir,
    reportSouvenir,
  } = useSouvenirActions({
    ownerUserId: userProfile?.id,
    canLike: Boolean(userDetails),
    canReport: Boolean(userDetails) && !isMe,
    updateSouvenir,
    removeSouvenir,
  });
  const {
    openSouvenirKey,
    openSouvenirIndex,
    openSouvenir: souvenir,
    pendingSouvenir,
    requestOpenSouvenir,
    confirmContentWarning,
    dismissContentWarning,
    closeSouvenir,
  } = useSouvenirContentWarning({
    souvenirs,
    disableNsfwAlert,
    ownerUserId: userProfile?.id,
  });

  const clearRequestedSouvenir = useCallback(() => {
    const nextParams = new URLSearchParams(searchParams);
    nextParams.delete("souvenir");
    setSearchParams(nextParams, { replace: true });
  }, [searchParams, setSearchParams]);

  useEffect(() => {
    attemptedDeepLinkPagesRef.current.clear();
  }, [requestedSouvenir, userProfile?.id]);

  useEffect(() => {
    if (!requestedSouvenir || !userProfile?.id) return;

    setActiveTab("souvenirs");
    const normalizedTarget = requestedSouvenir.toLowerCase();
    const match = findSouvenirByNotificationTarget(
      souvenirs,
      requestedSouvenir
    );

    if (match) {
      requestOpenSouvenir(match);
      clearRequestedSouvenir();
      return;
    }

    if (isLoadingSouvenirs) return;
    const pageKey = `${normalizedTarget}:${souvenirs.length}`;
    if (hasMoreSouvenirs && !attemptedDeepLinkPagesRef.current.has(pageKey)) {
      attemptedDeepLinkPagesRef.current.add(pageKey);
      void loadMoreSouvenirs(readStoredSouvenirSort());
      return;
    }

    clearRequestedSouvenir();
  }, [
    clearRequestedSouvenir,
    hasMoreSouvenirs,
    isLoadingSouvenirs,
    loadMoreSouvenirs,
    requestOpenSouvenir,
    requestedSouvenir,
    souvenirs,
    userProfile?.id,
  ]);

  useEffect(() => {
    dispatch(setHeaderTitle(""));

    if (userProfile) {
      dispatch(setHeaderTitle(userProfile.displayName));
    }
  }, [userProfile, dispatch]);

  useEffect(() => {
    if (userProfile) {
      getUserLibraryGames(effectiveSortBy, true, libraryFilter);
    }
  }, [effectiveSortBy, libraryFilter, getUserLibraryGames, userProfile]);

  useEffect(() => {
    if (userProfile) {
      getUserStats(libraryFilter);
    }
  }, [libraryFilter, getUserStats, userProfile]);

  const handleLoadMore = useCallback(() => {
    if (
      activeTab === "library" &&
      hasMoreLibraryGames &&
      !isLoadingLibraryGames
    ) {
      loadMoreLibraryGames(effectiveSortBy, libraryFilter);
    }
  }, [
    activeTab,
    hasMoreLibraryGames,
    isLoadingLibraryGames,
    loadMoreLibraryGames,
    effectiveSortBy,
    libraryFilter,
  ]);

  useEffect(() => {
    const handlePinToggled = () => {
      if (userProfile) {
        getUserLibraryGames(effectiveSortBy, true, libraryFilter);
      }
    };

    window.addEventListener("hydra:game-pin-toggled", handlePinToggled);
    return () => {
      window.removeEventListener("hydra:game-pin-toggled", handlePinToggled);
    };
  }, [getUserLibraryGames, effectiveSortBy, libraryFilter, userProfile]);

  useEffect(() => {
    const refresh = () => {
      if (isMe) void getUserProfile();
    };
    window.addEventListener("hydra:game-visibility-updated", refresh);
    return () =>
      window.removeEventListener("hydra:game-visibility-updated", refresh);
  }, [getUserProfile, isMe]);

  // Reset tab when switching users
  useEffect(() => {
    setActiveTab(requestedTab === "souvenirs" ? "souvenirs" : "library");
    setPlatform(readStoredProfilePlatform());
  }, [requestedTab, userProfile?.id]);

  const usersAreFriends = useMemo(() => {
    return userProfile?.relation?.status === "ACCEPTED";
  }, [userProfile]);

  const content = (() => {
    if (!userProfile) return null;

    const shouldLockProfile =
      userProfile.profileVisibility === "PRIVATE" ||
      (userProfile.profileVisibility === "FRIENDS" && !usersAreFriends);

    if (!isMe && shouldLockProfile) {
      return <LockedProfile />;
    }

    const hasGames = libraryGames.length > 0;
    const hasPinnedGames = pinnedGames.length > 0;
    const hasAnyGames = hasGames || hasPinnedGames;

    const shouldShowRightContent =
      hasAnyGames || userProfile.friends.length > 0 || isMe;

    return (
      <section className="profile-content__section">
        <div className="profile-content__main">
          <ProfileTabs
            activeTab={activeTab}
            souvenirsCount={souvenirsTotal}
            onTabChange={setActiveTab}
          />

          <div className="profile-content__tab-panels">
            <AnimatePresence mode="wait">
              {activeTab === "library" && (
                <LibraryTab
                  sortBy={effectiveSortBy}
                  onSortChange={handleSortChange}
                  platform={platform}
                  onPlatformChange={handlePlatformChange}
                  pinnedGames={pinnedGames}
                  libraryGames={libraryGames}
                  hasMoreLibraryGames={hasMoreLibraryGames}
                  isAwaitingInitialLibrary={isCorrectingPrefetchedSort}
                  userStats={userStats}
                  onLoadMore={handleLoadMore}
                  isMe={isMe}
                  hasActiveSubscription={Boolean(
                    userProfile.hasActiveSubscription
                  )}
                />
              )}

              {activeTab === "souvenirs" && (
                <SouvenirsTab
                  achievements={souvenirs}
                  hasReachedLimit={hasReachedSouvenirLimit}
                  hiddenReason={souvenirsHiddenReason}
                  canLike={Boolean(userDetails)}
                  hasMore={hasMoreSouvenirs}
                  isLoading={isLoadingSouvenirs}
                  isEnabled={souvenirsEnabled}
                  isMe={isMe}
                  userId={userProfile.id}
                  visibility={userProfile.souvenirsVisibility}
                  hasActiveSubscription={Boolean(
                    userProfile.hasActiveSubscription
                  )}
                  disableNsfwAlert={disableNsfwAlert}
                  likingKeys={likingKeys}
                  onSouvenirClick={requestOpenSouvenir}
                  onLikeClick={(item) => void likeSouvenir(item)}
                  onReload={getUserSouvenirs}
                  onLoadMore={loadMoreSouvenirs}
                  onOpenSettings={() =>
                    navigate(
                      "/settings?tab=content_gameplay#achievement-souvenirs"
                    )
                  }
                />
              )}
            </AnimatePresence>
          </div>
        </div>

        {shouldShowRightContent && (
          <div className="profile-content__right-content">
            {userStats && (
              <ProfileSection title={t("stats")} defaultOpen={true}>
                <UserStatsBox />
              </ProfileSection>
            )}
            {userProfile?.badges.length > 0 && (
              <ProfileSection
                title={t("badges")}
                count={userProfile.badges.length}
                defaultOpen={true}
              >
                <BadgesBox />
              </ProfileSection>
            )}
            {userProfile?.recentGames.length > 0 && (
              <ProfileSection title={t("activity")} defaultOpen={true}>
                <RecentGamesBox />
              </ProfileSection>
            )}
            <ReportProfile />
          </div>
        )}
      </section>
    );
  })();

  return (
    <div>
      <ProfileHero />

      {content}

      <SouvenirLightbox
        souvenir={souvenir}
        items={souvenirs}
        index={openSouvenirIndex}
        isOwner={isMe}
        canLike={Boolean(userDetails)}
        isLiking={Boolean(openSouvenirKey && likingKeys.has(openSouvenirKey))}
        isUpdatingVisibility={Boolean(
          openSouvenirKey && visibilityKeys.has(openSouvenirKey)
        )}
        isDeleting={Boolean(
          openSouvenirKey && deletingKeys.has(openSouvenirKey)
        )}
        isReporting={Boolean(
          openSouvenirKey && reportingKeys.has(openSouvenirKey)
        )}
        isReported={Boolean(
          openSouvenirKey && reportedKeys.has(openSouvenirKey)
        )}
        isContentWarningVisible={Boolean(pendingSouvenir)}
        onClose={closeSouvenir}
        onNavigate={(index) => {
          const nextSouvenir = souvenirs[index];
          return nextSouvenir ? requestOpenSouvenir(nextSouvenir) : false;
        }}
        onLike={(item) => void likeSouvenir(item)}
        onVisibilityChange={(item) => void changeSouvenirVisibility(item)}
        onDelete={deleteSouvenir}
        onReport={reportSouvenir}
      />

      <ConfirmationModal
        visible={Boolean(pendingSouvenir)}
        title={t("souvenir_content_warning_title")}
        descriptionText={t("souvenir_content_warning_description", {
          title: pendingSouvenir?.gameTitle ?? t("unknown_game"),
        })}
        confirmButtonLabel={t("allow_nsfw_content", {
          ns: "game_details",
        })}
        cancelButtonLabel={t("refuse_nsfw_content", {
          ns: "game_details",
        })}
        onConfirm={confirmContentWarning}
        onClose={dismissContentWarning}
        clickOutsideToClose={false}
      />
    </div>
  );
}
