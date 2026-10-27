import { steamSyncLogger } from "../logger";
import { getSteamAccount } from "./steam-account";
import { createSteamStartupSync } from "./steam-startup-sync-core";
import { steamSyncOrchestrator } from "./steam-sync-orchestrator";

// Launching shouldn't open a Steam session every time; a recent sync is fresh enough.
const STARTUP_SYNC_MIN_AGE_MS = 6 * 60 * 60 * 1000;

const isStale = (lastSyncedAt: string | null) =>
  !lastSyncedAt ||
  Date.now() - Date.parse(lastSyncedAt) > STARTUP_SYNC_MIN_AGE_MS;

const startupSync = createSteamStartupSync({
  isLoggedIn: () => true,
  getStatus: async () => {
    const account = await getSteamAccount();
    return { connected: account !== null && isStale(account.lastSyncedAt) };
  },
  startSync: (origin) => steamSyncOrchestrator.start(origin),
  logError: (message, error) => steamSyncLogger.error(message, error),
});

export const startSteamSyncOnStartup = () => startupSync.run();
