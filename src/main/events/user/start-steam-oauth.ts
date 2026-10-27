import { logger } from "@main/services";
import {
  getSteamAccount,
  saveSteamAccount,
} from "@main/services/steam-integration/steam-account";
import { parseSteamProfileXml } from "@main/services/steam-integration/steam-account-core";
import {
  getSteamWebApiToken,
  notifySteamConnectError,
  notifySteamConnected,
  openSteamLoginWindow,
  readSteamCommunitySession,
} from "@main/services/steam-integration/steam-store-session";
import { steamSyncOrchestrator } from "@main/services/steam-integration/steam-sync-orchestrator";
import { registerEvent } from "../register-event";

const readProfile = async (steamId64: string) => {
  try {
    const { fetch } = await readSteamCommunitySession();
    const response = await fetch(
      `https://steamcommunity.com/profiles/${steamId64}/?xml=1`
    );
    return parseSteamProfileXml(await response.text());
  } catch (error) {
    // The name and avatar are cosmetic; the link itself must still succeed.
    logger.error("Failed to read Steam profile", error);
    return { username: null, avatarUrl: null };
  }
};

const saveConnectedAccount = async () => {
  const { steamId64 } = await getSteamWebApiToken();
  const [{ username, avatarUrl }, existing] = await Promise.all([
    readProfile(steamId64),
    getSteamAccount(),
  ]);

  await saveSteamAccount({
    steamId64,
    username: username ?? steamId64,
    avatarUrl,
    connectedAt: existing?.connectedAt ?? new Date().toISOString(),
    lastSyncedAt:
      existing?.steamId64 === steamId64 ? existing.lastSyncedAt : null,
  });

  steamSyncOrchestrator.clearReconnectRequired();
  notifySteamConnected();
};

const startSteamOAuth = async () => {
  logger.log("Opening Steam login window");
  openSteamLoginWindow(saveConnectedAccount, (error) => {
    logger.error("Failed to connect Steam", error);
    notifySteamConnectError("generic");
  });
};

registerEvent("startSteamOAuth", startSteamOAuth);
