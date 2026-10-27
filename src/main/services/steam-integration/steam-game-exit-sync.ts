import { levelKeys } from "@main/level";
import type { Game, SteamGameSyncPayload } from "@types";

import { updateGameRecord } from "../game-record-updater";
import { mergeImportedProfileGame } from "../library-sync/merge-imported-profile-game";
import { steamSyncLogger } from "../logger";
import { WindowManager } from "../window-manager";
import { createSteamGameExitSyncScheduler } from "./steam-game-exit-sync-scheduler";
import { createSteamGamePageSync } from "./steam-game-page-sync-core";
import { steamSyncOrchestrator } from "./steam-sync-orchestrator";

const publishGame = async (
  steamAppId: string,
  payload: SteamGameSyncPayload,
  signal: AbortSignal
) => {
  if (signal.aborted) return;

  const updatedGame = await updateGameRecord(
    levelKeys.game("steam", steamAppId),
    (localGame) =>
      mergeImportedProfileGame(localGame, {
        objectId: steamAppId,
        shop: "steam",
        lastTimePlayed: payload.lastPlayedAt,
        runtimeByPlatform: { steam: payload.playTimeInSeconds },
        hasActiveSteamImport: true,
      })
  );
  if (!updatedGame || signal.aborted) return;

  WindowManager.sendToAppWindows("on-library-batch-complete");
};

const scheduler = createSteamGameExitSyncScheduler({
  waitForFullSync: () => steamSyncOrchestrator.waitForCurrentRun(),
  collect: (steamAppId, signal) =>
    steamSyncOrchestrator.collectGameSyncPayload(steamAppId, signal),
  publish: publishGame,
  scheduleTimer: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimer: (timer) => clearTimeout(timer),
  log: (message, ...args) => steamSyncLogger.log(message, ...args),
  logError: (message, ...args) => steamSyncLogger.error(message, ...args),
});

const pageSync = createSteamGamePageSync({
  waitForFullSync: () => steamSyncOrchestrator.waitForCurrentRun(),
  collect: (steamAppId, signal) =>
    steamSyncOrchestrator.collectGameSyncPayload(steamAppId, signal),
  publish: publishGame,
  log: (message, ...args) => steamSyncLogger.log(message, ...args),
  logError: (message, ...args) => steamSyncLogger.error(message, ...args),
});

export const scheduleSteamGameExitSync = (game: Game) => {
  const gameKey = levelKeys.game(game.shop, game.objectId);
  scheduler.schedule(gameKey, game.objectId);
};

export const cancelSteamGameExitSync = (game: Game) => {
  scheduler.cancel(levelKeys.game(game.shop, game.objectId));
};

export const syncSteamGameOnGamePage = (steamAppId: string) =>
  pageSync.sync(steamAppId);

export { shouldScheduleSteamGameExitSync } from "./steam-game-exit-sync-scheduler";
