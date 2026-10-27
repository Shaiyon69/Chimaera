import type { SteamIntegrationStatus, SteamSourceLibraryGame } from "@types";

import type { ProfileGame } from "../library-sync/merge-with-remote-games.js";

export interface SteamAccount {
  steamId64: string;
  username: string;
  avatarUrl: string | null;
  connectedAt: string;
  lastSyncedAt: string | null;
}

const STEAM_ASSETS =
  "https://shared.steamstatic.com/store_item_assets/steam/apps";

export const toSteamIntegrationStatus = (
  account: SteamAccount | null
): SteamIntegrationStatus =>
  account
    ? {
        connected: true,
        snapshotPreserved: false,
        ...account,
        disconnectedAt: null,
        latestSyncRun: null,
      }
    : { connected: false, snapshotPreserved: false };

/** Fields left undefined here keep whatever the local record already has. */
export const steamLibraryGameToProfileGame = (
  game: SteamSourceLibraryGame
): ProfileGame => ({
  shop: "steam",
  objectId: game.steamAppId,
  title: game.name,
  iconUrl: `${STEAM_ASSETS}/${game.steamAppId}/library_600x900_2x.jpg`,
  libraryHeroImageUrl: `${STEAM_ASSETS}/${game.steamAppId}/library_hero.jpg`,
  logoImageUrl: `${STEAM_ASSETS}/${game.steamAppId}/logo.png`,
  lastTimePlayed: game.lastPlayedAt ? new Date(game.lastPlayedAt) : null,
  playTimeInMilliseconds: 0,
  runtimeByPlatform: { steam: game.playTimeInSeconds },
  hasManuallyUpdatedPlaytime: false,
  achievementCount: 0,
  unlockedAchievementCount: 0,
  source: "steam",
  hasActiveSteamImport: true,
});

const xmlField = (xml: string, tag: string) => {
  const match = new RegExp(
    String.raw`<${tag}>\s*(?:<!\[CDATA\[([\s\S]*?)\]\]>|([^<]*))\s*</${tag}>`
  ).exec(xml);

  return (match?.[1] ?? match?.[2])?.trim() || null;
};

/** Reads the public `?xml=1` profile; a private profile still exposes name and avatar. */
export const parseSteamProfileXml = (xml: string) => ({
  username: xmlField(xml, "steamID"),
  avatarUrl: xmlField(xml, "avatarFull"),
});

/** The login page sends a signed-in user on to `redir`, so leaving /login means success. */
export const isSteamStoreLoggedInUrl = (href: string) => {
  try {
    const url = new URL(href);
    return (
      url.hostname === "store.steampowered.com" &&
      !url.pathname.startsWith("/login")
    );
  } catch {
    return false;
  }
};
