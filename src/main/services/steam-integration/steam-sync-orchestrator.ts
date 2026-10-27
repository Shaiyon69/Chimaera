import { randomUUID } from "node:crypto";
import { db, gamesSublevel, levelKeys } from "@main/level";
import type { SteamAchievement, UserPreferences } from "@types";
import { publishAchievementUnlockNotifications } from "../achievements/merge-achievements";
import { isSteamReconnectRequired } from "@shared";
import { updateGameRecord } from "../game-record-updater";
import {
  hideGamesRemovedFromSteamImport,
  mergeProfileGamesIntoLibrary,
} from "../library-sync";
import { steamSyncLogger } from "../logger";
import { WindowManager } from "../window-manager";
import type {
  SteamGameSyncPayload,
  SteamSourceAchievement,
  SteamSourceLibraryGame,
  SteamSnapshotPayload,
  SteamSyncFinishedPayload,
  SteamSyncOrigin,
  SteamSyncPhase,
  SteamSyncState,
  UnlockedAchievement,
} from "@types";
import {
  isSkippedSteamLibraryTitle,
  parseSteamSourceAchievements,
  parseSteamSourceLibrary,
} from "./steam-source-payload";
import {
  getSteamSourceHttpStatus,
  isSteamPrivateProfilePayload,
  isSteamSourceAchievementSkippable,
  isSteamSourceLibraryFatal,
  isSteamSourceRateLimited,
  withSteamSourceRetry,
} from "./steam-source-retry";
import {
  SteamAccountMismatchError,
  SteamPrivateProfileError,
  SteamRateLimitedError,
  SteamSessionRequiredError,
  SteamSyncAbortedError,
  SteamWebApiHttpError,
  isSteamSyncAbortError,
} from "./steam-sync-errors";
import type { SteamWebApiToken } from "./steam-store-session-config";
import {
  getSteamWebApiToken,
  readSteamCommunitySession,
  type SteamCommunitySession,
} from "./steam-store-session";
import {
  catalogueFromSteamSchema,
  fetchSteamCommunityPlayerAchievements,
  shouldFetchSteamCommunityAchievements,
} from "./steam-community-achievements";
import {
  AchievementMemoryStore,
  mergePersistedAchievementTotals,
  withSteamAchievementCatalogue,
} from "../achievements/achievement-memory-store";
import { mergeUnlockedAchievementLists } from "../achievements/merge-unlocked-achievements";
import {
  fetchSteamFamilyGroupForUser,
  fetchSteamFamilyPlaytimeSummary,
  fetchSteamGameAchievementSchema,
  fetchSteamLastPlayedTimes,
  fetchSteamOwnedGames,
  fetchSteamOwnedGame,
  fetchSteamSharedLibraryApps,
} from "./steam-web-api";
import {
  countSteamFamilyPlaytimeEntries,
  mergeSteamFamilyPlaytimeMaps,
  mergeSteamOwnedAndFamilyGames,
  parseSteamFamilyGroupId,
  parseSteamFamilyPlaytimeByAppId,
  parseSteamLastPlayedTimes,
  parseSteamSharedLibraryApps,
  playtimeMapFromSharedApps,
  type SteamFamilyPlaytime,
  type SteamFamilySharedApp,
} from "./steam-family-library";
import { buildSteamSnapshot } from "./steam-sync-snapshot";
import {
  steamLibraryGameToProfileGame,
  toSteamIntegrationStatus,
} from "./steam-account-core";
import { getSteamAccount, saveSteamAccount } from "./steam-account";
import { linkImportedSteamGameExecutables } from "./link-imported-steam-executables";
import { watchSteamLibraries } from "./steam-install-watcher";
import { AchievementWatcherManager } from "../achievements/achievement-watcher-manager";

const ACHIEVEMENT_FETCH_CONCURRENCY = 8;

const idleState = (): SteamSyncState => ({ status: "idle" });

const getApiErrorMessage = (error: unknown): string | null => {
  if (typeof error === "object" && error !== null) {
    const response = (error as { response?: { data?: { message?: unknown } } })
      .response;
    const responseMessage = response?.data?.message;

    if (typeof responseMessage === "string") {
      return responseMessage;
    }
  }

  if (error instanceof Error && error.message) {
    return error.message;
  }

  return null;
};

const steamWebApiErrorBody = (error: unknown): string | null => {
  let body: unknown = null;

  if (error instanceof SteamWebApiHttpError) {
    body = error.body;
  } else if (typeof error === "object" && error !== null && "body" in error) {
    body = (error as { body: unknown }).body;
  }

  if (body == null) return null;

  return typeof body === "string" ? body : JSON.stringify(body);
};

const formatAchievementHttpDetail = (error: unknown, status: number | null) => {
  const message = getApiErrorMessage(error);
  const body = steamWebApiErrorBody(error);
  const extras = [message, body].filter(Boolean);

  if (extras.length === 0) {
    return `HTTP ${status}`;
  }

  return `HTTP ${status}, ${extras.join(", ")}`;
};

const steamUnlocksToLocal = (
  achievements: SteamSourceAchievement[]
): UnlockedAchievement[] =>
  achievements.flatMap((achievement) => {
    if (!achievement.unlocked || !achievement.unlockTime) {
      return [];
    }

    return [
      {
        name: achievement.name,
        unlockTime: Date.parse(achievement.unlockTime),
      },
    ];
  });

const getSteamSyncFailureMessage = (error: unknown): string => {
  if (
    error instanceof SteamPrivateProfileError ||
    error instanceof SteamRateLimitedError ||
    error instanceof SteamSessionRequiredError ||
    error instanceof SteamAccountMismatchError
  ) {
    return error.message;
  }

  if (isSteamSourceRateLimited(error)) {
    return "profile/steam-rate-limited";
  }

  return getApiErrorMessage(error) ?? "steam-sync-failed";
};

const throwIfAborted = (signal: AbortSignal) => {
  if (signal.aborted) {
    throw new SteamSyncAbortedError();
  }
};

const fetchWithRetry = <T>(
  label: string,
  fn: () => Promise<T>,
  signal: AbortSignal
) =>
  withSteamSourceRetry(fn, {
    signal,
    onRetry: (status, delayMs, failedAttempt) => {
      steamSyncLogger.log(
        `${label} retry ${failedAttempt} after HTTP ${status}, waiting ${delayMs}ms`
      );
    },
  });

const mapPool = async <T, R>(
  items: T[],
  concurrency: number,
  signal: AbortSignal,
  fn: (item: T, index: number) => Promise<R>
): Promise<R[]> => {
  const results: R[] = new Array(items.length);
  let nextIndex = 0;

  const worker = async () => {
    for (;;) {
      throwIfAborted(signal);

      const index = nextIndex;
      nextIndex += 1;
      if (index >= items.length) return;

      results[index] = await fn(items[index], index);
    }
  };

  const workerCount = Math.min(concurrency, items.length);
  if (workerCount === 0) return results;

  await Promise.all(Array.from({ length: workerCount }, () => worker()));
  return results;
};

class SteamSyncOrchestrator {
  private state: SteamSyncState = idleState();
  private abortController: AbortController | null = null;
  private runPromise: Promise<void> | null = null;
  private hasEmittedFinished = false;
  private origin: SteamSyncOrigin = "manual";

  getState() {
    return this.state;
  }

  clearReconnectRequired() {
    if (this.state.status === "idle" && this.state.requiresReconnect) {
      this.setState(idleState());
    }
  }

  async start(origin: SteamSyncOrigin = "manual") {
    if (this.state.status === "running" || this.state.status === "cancelling") {
      steamSyncLogger.log(
        "Start ignored, sync already",
        this.state.status,
        this.state.syncRunId
      );
      return this.state;
    }

    this.hasEmittedFinished = false;
    this.origin = origin;
    this.abortController = new AbortController();
    this.setState({
      status: "running",
      syncRunId: "",
      phase: "starting",
      gamesFound: 0,
      gamesProcessed: 0,
    });

    const { signal } = this.abortController;
    this.runPromise = this.run(signal).finally(() => {
      this.runPromise = null;
      this.abortController = null;
    });

    return this.state;
  }

  async waitForCurrentRun() {
    for (;;) {
      const currentRun = this.runPromise;
      if (currentRun === null) return;
      await currentRun;
    }
  }

  private async fetchFamilyGame(
    token: SteamWebApiToken,
    steamAppId: string,
    signal: AbortSignal
  ) {
    const groupPayload = await fetchWithRetry(
      "family group",
      () => fetchSteamFamilyGroupForUser(token, signal),
      signal
    );
    const familyGroupId = parseSteamFamilyGroupId(groupPayload);
    if (!familyGroupId) {
      return {
        apps: [] as SteamFamilySharedApp[],
        playtimeSources: [] as Map<string, SteamFamilyPlaytime>[],
      };
    }

    const sharedPayload = await fetchWithRetry(
      `family game ${steamAppId}`,
      () => fetchSteamSharedLibraryApps(token, familyGroupId, signal),
      signal
    );
    const apps = parseSteamSharedLibraryApps(sharedPayload).filter(
      (game) => game.steamAppId === steamAppId
    );
    const playtimeSources = [playtimeMapFromSharedApps(apps)];

    try {
      const playtimePayload = await fetchWithRetry(
        `family playtime ${steamAppId}`,
        () => fetchSteamFamilyPlaytimeSummary(token, familyGroupId, signal),
        signal
      );
      playtimeSources.push(
        parseSteamFamilyPlaytimeByAppId(playtimePayload, token.steamId64)
      );
    } catch (error) {
      if (isSteamSyncAbortError(error)) throw error;
      steamSyncLogger.log("Steam family playtime unavailable", error);
    }

    return { apps, playtimeSources };
  }

  async collectGameSyncPayload(
    steamAppId: string,
    signal: AbortSignal
  ): Promise<SteamGameSyncPayload> {
    const token = await this.resolveSteamSession(signal);
    throwIfAborted(signal);

    const ownedPayload = await fetchWithRetry(
      `game ${steamAppId}`,
      () => fetchSteamOwnedGame(token, steamAppId, signal),
      signal
    );
    const ownedGames = parseSteamSourceLibrary(ownedPayload).filter(
      (game) => game.steamAppId === steamAppId
    );
    const playtimeSources = [
      await this.fetchLastPlayedPlaytimeMap(token, signal),
    ];
    const family =
      ownedGames.length === 0
        ? await this.fetchFamilyGame(token, steamAppId, signal)
        : { apps: [], playtimeSources: [] };
    playtimeSources.push(...family.playtimeSources);

    const game = mergeSteamOwnedAndFamilyGames(
      ownedGames,
      family.apps,
      mergeSteamFamilyPlaytimeMaps(...playtimeSources)
    ).find((candidate) => candidate.steamAppId === steamAppId);

    if (!game) {
      throw new Error(`steam-game-not-found:${steamAppId}`);
    }

    let achievements:
      | SteamSnapshotPayload["games"][number]["achievements"]
      | undefined;
    try {
      const communitySession = await readSteamCommunitySession();
      if (communitySession.hasLoginCookie) {
        const achievementsByAppId = await this.fetchAchievements(
          token,
          communitySession,
          [game],
          signal,
          false
        );
        achievements = buildSteamSnapshot([game], achievementsByAppId).games[0]
          ?.achievements;
      }
    } catch (error) {
      if (isSteamSyncAbortError(error)) throw error;
      steamSyncLogger.log(
        `Steam achievements unavailable for ${steamAppId}`,
        error
      );
    }

    return {
      playTimeInSeconds: game.playTimeInSeconds,
      lastPlayedAt: game.lastPlayedAt,
      ...(achievements ? { achievements } : {}),
    };
  }

  async cancel() {
    if (this.state.status !== "running") {
      steamSyncLogger.log("Cancel ignored, sync is", this.state.status);
      return;
    }

    const syncRunId = this.state.syncRunId;
    steamSyncLogger.log("Cancel requested", syncRunId || "(no run id yet)");
    this.setState({ status: "cancelling", syncRunId });
    this.abortController?.abort();
    await this.runPromise?.catch(() => {});
  }

  private setState(state: SteamSyncState) {
    this.state = state;
    WindowManager.sendToAppWindows("on-steam-sync-progress", state);
  }

  private emitFinished(payload: SteamSyncFinishedPayload) {
    if (this.hasEmittedFinished) return;

    this.hasEmittedFinished = true;
    WindowManager.sendToAppWindows("on-steam-sync-finished", payload);
  }

  private async resolveSteamSession(signal: AbortSignal) {
    const account = await getSteamAccount();

    if (!account) {
      throw new SteamSessionRequiredError();
    }

    const token = await getSteamWebApiToken(signal);
    if (token.steamId64 !== account.steamId64) {
      steamSyncLogger.error(
        "Steam store session does not match linked SteamID64",
        token.steamId64,
        account.steamId64
      );
      throw new SteamAccountMismatchError();
    }

    return token;
  }

  private async fetchLibrary(token: SteamWebApiToken, signal: AbortSignal) {
    steamSyncLogger.log("GET Steam GetOwnedGames");

    try {
      const response = await fetchWithRetry(
        "library",
        () => fetchSteamOwnedGames(token, signal),
        signal
      );

      if (response != null && typeof response !== "object") {
        steamSyncLogger.log(
          "Unexpected Steam library payload",
          typeof response
        );
      }

      if (isSteamPrivateProfilePayload(response)) {
        throw new SteamPrivateProfileError();
      }

      const ownedGames = parseSteamSourceLibrary(response);
      const mergedGames = await this.mergeFamilyLibrary(
        ownedGames,
        token,
        signal
      );
      const games = mergedGames.filter(
        (game) => !isSkippedSteamLibraryTitle(game.name)
      );
      steamSyncLogger.log(
        "Library games found",
        games.length,
        "skipped titles",
        mergedGames.length - games.length
      );
      return games;
    } catch (error) {
      if (
        error instanceof SteamWebApiHttpError &&
        (error.status === 401 || error.status === 403)
      ) {
        throw new SteamSessionRequiredError();
      }

      if (isSteamSourceLibraryFatal(error)) {
        steamSyncLogger.error("Steam library is private");
        throw new SteamPrivateProfileError();
      }

      throw error;
    }
  }

  private async fetchLastPlayedPlaytimeMap(
    token: SteamWebApiToken,
    signal: AbortSignal
  ) {
    try {
      steamSyncLogger.log("GET Steam ClientGetLastPlayedTimes");
      const lastPlayedPayload = await fetchWithRetry(
        "last played times",
        () => fetchSteamLastPlayedTimes(token, signal),
        signal
      );
      return parseSteamLastPlayedTimes(lastPlayedPayload);
    } catch (error) {
      if (isSteamSyncAbortError(error)) {
        throw error;
      }

      steamSyncLogger.log("Steam last played times unavailable", error);
      return new Map<string, SteamFamilyPlaytime>();
    }
  }

  private async mergeFamilyLibrary(
    ownedGames: SteamSourceLibraryGame[],
    token: SteamWebApiToken,
    signal: AbortSignal
  ) {
    const playtimeSources = [
      await this.fetchLastPlayedPlaytimeMap(token, signal),
    ];
    let familyApps: SteamFamilySharedApp[] = [];

    try {
      steamSyncLogger.log("GET Steam GetFamilyGroupForUser");
      const groupPayload = await fetchWithRetry(
        "family group",
        () => fetchSteamFamilyGroupForUser(token, signal),
        signal
      );
      const familyGroupId = parseSteamFamilyGroupId(groupPayload);
      if (!familyGroupId) {
        steamSyncLogger.log("Steam family group not found");
      } else {
        steamSyncLogger.log("GET Steam GetSharedLibraryApps");
        const sharedPayload = await fetchWithRetry(
          "family library",
          () => fetchSteamSharedLibraryApps(token, familyGroupId, signal),
          signal
        );
        familyApps = parseSteamSharedLibraryApps(sharedPayload);
        playtimeSources.push(playtimeMapFromSharedApps(familyApps));

        try {
          steamSyncLogger.log("GET Steam GetPlaytimeSummary");
          const playtimePayload = await fetchWithRetry(
            "family playtime",
            () => fetchSteamFamilyPlaytimeSummary(token, familyGroupId, signal),
            signal
          );
          playtimeSources.push(
            parseSteamFamilyPlaytimeByAppId(playtimePayload, token.steamId64)
          );
        } catch (error) {
          if (isSteamSyncAbortError(error)) {
            throw error;
          }

          steamSyncLogger.log("Steam family playtime unavailable", error);
        }
      }
    } catch (error) {
      if (isSteamSyncAbortError(error)) {
        throw error;
      }

      steamSyncLogger.log("Steam family library unavailable", error);
    }

    const playtimeByAppId = mergeSteamFamilyPlaytimeMaps(...playtimeSources);
    const games = mergeSteamOwnedAndFamilyGames(
      ownedGames,
      familyApps,
      playtimeByAppId
    );
    steamSyncLogger.log(
      "Family library games found",
      familyApps.length,
      "merged total",
      games.length,
      "with playtime",
      countSteamFamilyPlaytimeEntries(playtimeByAppId)
    );
    return games;
  }

  private async persistLocalAchievementCounts(
    steamAppId: string,
    schemaCount: number,
    unlockedCount: number
  ) {
    const gameKey = levelKeys.game("steam", steamAppId);
    await updateGameRecord(gameKey, (localGame) =>
      mergePersistedAchievementTotals("steam", steamAppId, localGame, {
        achievementCount: schemaCount,
        unlockedAchievementCount: unlockedCount,
      })
    );
  }

  private async notifyNewUnlocks(
    steamAppId: string,
    newAchievements: UnlockedAchievement[],
    previous: UnlockedAchievement[],
    merged: UnlockedAchievement[],
    achievementsData: SteamAchievement[]
  ) {
    if (!newAchievements.length || !achievementsData.length) return;

    try {
      const localGame = await gamesSublevel.get(
        levelKeys.game("steam", steamAppId)
      );
      const userPreferences = await db.get<string, UserPreferences | null>(
        levelKeys.userPreferences,
        { valueEncoding: "json" }
      );
      if (!localGame || localGame.isDeleted) return;

      publishAchievementUnlockNotifications({
        game: localGame,
        newAchievements,
        unlockedAchievements: previous,
        achievementsData,
        mergedLocalAchievements: merged,
        userPreferences: userPreferences ?? ({} as UserPreferences),
      });
    } catch (err) {
      steamSyncLogger.log("Failed to notify Steam unlocks", steamAppId, err);
    }
  }

  private async fetchAchievements(
    token: SteamWebApiToken,
    communitySession: SteamCommunitySession,
    games: SteamSourceLibraryGame[],
    signal: AbortSignal,
    trackProgress = true
  ) {
    const achievementsByAppId = new Map<
      string,
      SteamSourceAchievement[] | undefined
    >();
    let gamesProcessed = 0;
    let rateLimited = false;

    await mapPool(
      games,
      ACHIEVEMENT_FETCH_CONCURRENCY,
      signal,
      async (game, index) => {
        throwIfAborted(signal);

        try {
          if (rateLimited) {
            steamSyncLogger.log(
              `Skipping achievements for ${game.steamAppId} ${game.name} (HTTP 429)`
            );
            achievementsByAppId.set(game.steamAppId, undefined);
            return;
          }

          const scrapeCommunity = shouldFetchSteamCommunityAchievements(
            game.playTimeInSeconds
          );

          steamSyncLogger.log(
            `GET Steam achievements ${index + 1}/${games.length}`,
            game.steamAppId,
            game.name,
            scrapeCommunity ? "community" : "schema only"
          );

          let schemaPayload: unknown;
          const loadSchema = async (steamAppId: string) => {
            schemaPayload = await fetchSteamGameAchievementSchema(
              token,
              steamAppId,
              signal
            );
            return schemaPayload;
          };

          const response = scrapeCommunity
            ? await fetchWithRetry(
                `achievements ${game.steamAppId}`,
                () =>
                  fetchSteamCommunityPlayerAchievements({
                    steamId64: token.steamId64,
                    steamAppId: game.steamAppId,
                    signal,
                    communityFetch: communitySession.fetch,
                    timeZoneOffsetSeconds:
                      communitySession.timeZoneOffsetSeconds,
                    loadSchema,
                  }),
                signal
              )
            : { achievements: [] };

          if (schemaPayload == null) {
            try {
              schemaPayload = await loadSchema(game.steamAppId);
            } catch (schemaError) {
              steamSyncLogger.log(
                `Schema unavailable for ${game.steamAppId} ${game.name}`,
                schemaError
              );
            }
          }

          if (response != null && typeof response !== "object") {
            steamSyncLogger.log(
              "Unexpected Steam achievements payload",
              game.steamAppId,
              typeof response
            );
          }

          const achievements = parseSteamSourceAchievements(response);
          const catalogue =
            schemaPayload != null
              ? catalogueFromSteamSchema(game.steamAppId, schemaPayload)
              : [];
          const schemaCount =
            catalogue.length > 0 ? catalogue.length : achievements.length;

          const current = AchievementMemoryStore.get("steam", game.steamAppId);
          const unlockedAchievements = mergeUnlockedAchievementLists(
            steamUnlocksToLocal(achievements),
            current?.unlockedAchievements ?? []
          );

          steamSyncLogger.log(
            `Achievements for ${game.steamAppId} ${game.name}: ${unlockedAchievements.length} unlocked / ${schemaCount}`
          );

          // A first sync has no baseline; toasting then would announce the whole history.
          const newUnlocks = current
            ? unlockedAchievements.filter(
                (u) =>
                  !current.unlockedAchievements.some(
                    (c) => c.name.toUpperCase() === u.name.toUpperCase()
                  )
              )
            : [];

          AchievementMemoryStore.set(
            "steam",
            game.steamAppId,
            withSteamAchievementCatalogue(
              current,
              catalogue,
              unlockedAchievements
            )
          );

          await this.notifyNewUnlocks(
            game.steamAppId,
            newUnlocks,
            current?.unlockedAchievements ?? [],
            unlockedAchievements,
            catalogue
          );

          await this.persistLocalAchievementCounts(
            game.steamAppId,
            schemaCount,
            unlockedAchievements.length
          );

          achievementsByAppId.set(game.steamAppId, achievements);
        } catch (error) {
          if (isSteamSyncAbortError(error)) throw error;

          if (
            error instanceof SteamSessionRequiredError ||
            (error instanceof Error &&
              error.name === "SteamSessionRequiredError")
          ) {
            throw error instanceof SteamSessionRequiredError
              ? error
              : new SteamSessionRequiredError();
          }

          const status = getSteamSourceHttpStatus(error);
          const detail = formatAchievementHttpDetail(error, status);

          if (status === 401) {
            throw new SteamSessionRequiredError();
          }

          if (isSteamSourceAchievementSkippable(error)) {
            steamSyncLogger.log(
              `Skipping achievements for ${game.steamAppId} ${game.name} (${detail})`
            );
            achievementsByAppId.set(game.steamAppId, undefined);

            if (status === 429) {
              rateLimited = true;
              if (trackProgress) {
                this.setState(idleState());
                this.emitFinished({
                  ok: false,
                  message: "profile/steam-rate-limited",
                  origin: this.origin,
                });
              }
            }
          } else {
            steamSyncLogger.error(
              `Failed achievements for ${game.steamAppId} ${game.name} (${detail})`
            );
            throw error;
          }
        } finally {
          gamesProcessed += 1;

          if (trackProgress && this.state.status === "running") {
            this.setState({
              ...this.state,
              phase: "achievements",
              gamesFound: games.length,
              gamesProcessed,
            });
          }
        }
      }
    );

    if (rateLimited) {
      throw new SteamRateLimitedError();
    }

    return achievementsByAppId;
  }

  private setProgress(
    syncRunId: string,
    phase: SteamSyncPhase,
    gamesFound = 0,
    gamesProcessed = 0
  ) {
    this.setState({
      status: "running",
      syncRunId,
      phase,
      gamesFound,
      gamesProcessed,
    });
  }

  private async run(signal: AbortSignal) {
    const syncRunId = randomUUID();

    steamSyncLogger.log("Steam sync started");

    try {
      this.setProgress(syncRunId, "library");

      const token = await this.resolveSteamSession(signal);
      throwIfAborted(signal);

      const communitySession = await readSteamCommunitySession();
      if (!communitySession.hasLoginCookie) {
        throw new SteamSessionRequiredError();
      }

      const games = await this.fetchLibrary(token, signal);
      throwIfAborted(signal);

      // Merged before achievements so their counts land on records that exist.
      this.setProgress(syncRunId, "merging");
      await AchievementWatcherManager.runBatch(async () => {
        await mergeProfileGamesIntoLibrary(
          games.map(steamLibraryGameToProfileGame),
          false,
          (processed, total) => {
            if (signal.aborted) return;
            this.setProgress(syncRunId, "merging", total, processed);
          }
        );

        // An empty library is far likelier a bad response than a cleared account.
        if (games.length > 0) {
          await hideGamesRemovedFromSteamImport(
            new Set(
              games.map((game) => levelKeys.game("steam", game.steamAppId))
            )
          );
        }
      });
      WindowManager.sendToAppWindows("on-library-batch-complete");

      this.setProgress(syncRunId, "achievements", games.length);
      await this.fetchAchievements(token, communitySession, games, signal);
      throwIfAborted(signal);

      this.setProgress(syncRunId, "executables");
      const linkedExecutableCount = await AchievementWatcherManager.runBatch(
        () =>
          linkImportedSteamGameExecutables((processed, total) => {
            if (signal.aborted) return;
            this.setProgress(syncRunId, "executables", total, processed);
          })
      );
      steamSyncLogger.log("Library merge finished", { linkedExecutableCount });
      void watchSteamLibraries();
      WindowManager.sendToAppWindows("on-library-batch-complete");

      this.setProgress(syncRunId, "finishing");
      const account = await getSteamAccount();
      const synced = account && {
        ...account,
        lastSyncedAt: new Date().toISOString(),
      };
      if (synced) await saveSteamAccount(synced);

      steamSyncLogger.log("Steam sync succeeded", synced?.lastSyncedAt);
      this.setState(idleState());
      this.emitFinished({
        ok: true,
        status: toSteamIntegrationStatus(synced),
        origin: this.origin,
      });
    } catch (error) {
      if (isSteamSyncAbortError(error)) {
        steamSyncLogger.log("Steam sync aborted", syncRunId);
        this.setState(idleState());
        this.emitFinished({
          ok: false,
          message: "steam-sync-aborted",
          origin: this.origin,
        });
        return;
      }

      const message = getSteamSyncFailureMessage(error);

      steamSyncLogger.error("Steam sync failed", message);
      this.setState(
        isSteamReconnectRequired(message)
          ? { status: "idle", requiresReconnect: true }
          : idleState()
      );
      this.emitFinished({ ok: false, message, origin: this.origin });
    }
  }
}

export const steamSyncOrchestrator = new SteamSyncOrchestrator();
