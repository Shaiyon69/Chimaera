import { registerEvent } from "../register-event";
import { WindowManager } from "@main/services";
import { steamSyncLogger } from "@main/services/logger";
import {
  collectSteamOnlyObjectIds,
  getSteamImportedDataCleanupPlan,
  hasImportedSteamData,
} from "@main/services/steam-integration/steam-imported-games";
import { clearSteamAccount } from "@main/services/steam-integration/steam-account";
import { STEAM_SESSION_PARTITION } from "@main/services/steam-integration/steam-store-session";
import { clearImportedSteamGames } from "@main/services/steam-integration/clear-imported-steam-games";
import { AchievementMemoryStore } from "@main/services/achievements/achievement-memory-store";
import { gamesSublevel, batchWithGames } from "@main/level";
import type { Game } from "@types";
import { session } from "electron";
import { chunk } from "lodash-es";

const DISCONNECT_WRITE_CHUNK_SIZE = 250;

const writeGames = async (updates: [string, Game][]) => {
  for (const updatesChunk of chunk(updates, DISCONNECT_WRITE_CHUNK_SIZE)) {
    const batch = batchWithGames();

    for (const [key, game] of updatesChunk) {
      batch.put(key, game, { sublevel: gamesSublevel });
    }

    await batch.write();
  }
};

const restoreLastTimePlayed = async (
  lastTimePlayedByGameKey: Map<string, Date | null>
) => {
  const entries = [...lastTimePlayedByGameKey];
  const games = await gamesSublevel.getMany(entries.map(([key]) => key));

  await writeGames(
    entries.flatMap(([key, lastTimePlayed], index) => {
      const game = games[index];
      return game && !game.isDeleted
        ? [[key, { ...game, lastTimePlayed }] as [string, Game]]
        : [];
    })
  );
};

// The cleanup keeps this flag until the end so nothing else treats the game as non-Steam mid-way.
const clearActiveSteamImport = async () =>
  writeGames(
    (await gamesSublevel.iterator().all()).flatMap(([key, game]) =>
      game.hasActiveSteamImport
        ? [[key, { ...game, hasActiveSteamImport: false }] as [string, Game]]
        : []
    )
  );

const disconnectSteam = async (
  _event: Electron.IpcMainInvokeEvent,
  deleteImportedData: boolean
) => {
  const allGames = await gamesSublevel.iterator().all();
  const steamOnlyObjectIdSet = new Set(
    deleteImportedData
      ? collectSteamOnlyObjectIds(allGames.map(([, game]) => game))
      : []
  );
  const lastTimePlayedByGameKey = new Map<string, Date | null>();
  const localUpdates: [string, Game][] = [];

  for (const [key, game] of allGames) {
    if (!deleteImportedData && game.hasActiveSteamImport) {
      localUpdates.push([key, { ...game, hasActiveSteamImport: false }]);
      continue;
    }

    if (
      deleteImportedData &&
      hasImportedSteamData(game, steamOnlyObjectIdSet)
    ) {
      const cleanupPlan = getSteamImportedDataCleanupPlan(game);
      lastTimePlayedByGameKey.set(key, cleanupPlan.lastTimePlayedFallback);
      AchievementMemoryStore.delete(game.shop, game.objectId);
      localUpdates.push([key, { ...game, ...cleanupPlan.cleanup }]);
    }
  }

  await writeGames(localUpdates);

  if (deleteImportedData) {
    await clearImportedSteamGames([...steamOnlyObjectIdSet]);
    await restoreLastTimePlayed(lastTimePlayedByGameKey);
    await clearActiveSteamImport();
  }

  await clearSteamAccount();
  await session.fromPartition(STEAM_SESSION_PARTITION).clearStorageData();

  WindowManager.sendToAppWindows("on-library-batch-complete");
  steamSyncLogger.log("Steam disconnected", { deleteImportedData });
};

registerEvent("disconnectSteam", disconnectSteam);
