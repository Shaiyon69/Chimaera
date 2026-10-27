import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  isSteamStoreLoggedInUrl,
  parseSteamProfileXml,
  steamLibraryGameToProfileGame,
  toSteamIntegrationStatus,
} from "./steam-account-core.js";

describe("steam account core", () => {
  it("reports disconnected without an account", () => {
    assert.deepEqual(toSteamIntegrationStatus(null), {
      connected: false,
      snapshotPreserved: false,
    });
  });

  it("reports connected with the account fields", () => {
    const status = toSteamIntegrationStatus({
      steamId64: "7656",
      username: "Ana",
      avatarUrl: null,
      connectedAt: "2026-01-01",
      lastSyncedAt: null,
    });
    assert.equal(status.connected, true);
    assert.equal(status.connected && status.username, "Ana");
  });

  it("maps a library game to a steam-imported profile game", () => {
    const game = steamLibraryGameToProfileGame({
      steamAppId: "620",
      name: "Portal 2",
      playTimeInSeconds: 90,
      lastPlayedAt: "2026-01-02T00:00:00.000Z",
    });
    assert.equal(game.objectId, "620");
    assert.equal(game.hasActiveSteamImport, true);
    assert.equal(game.runtimeByPlatform?.steam, 90);
    assert.equal(
      game.lastTimePlayed?.toISOString(),
      "2026-01-02T00:00:00.000Z"
    );
    assert.equal(game.id, undefined);
  });

  it("parses CDATA and plain profile xml fields", () => {
    const xml =
      "<profile><steamID><![CDATA[Ana]]></steamID><avatarFull>http://a/b.jpg</avatarFull></profile>";
    assert.deepEqual(parseSteamProfileXml(xml), {
      username: "Ana",
      avatarUrl: "http://a/b.jpg",
    });
    assert.deepEqual(parseSteamProfileXml("<profile/>"), {
      username: null,
      avatarUrl: null,
    });
  });

  it("treats leaving /login on the store as logged in", () => {
    assert.equal(
      isSteamStoreLoggedInUrl("https://store.steampowered.com/explore/"),
      true
    );
    assert.equal(
      isSteamStoreLoggedInUrl("https://store.steampowered.com/login/?redir=x"),
      false
    );
    assert.equal(isSteamStoreLoggedInUrl("https://example.com/"), false);
    assert.equal(isSteamStoreLoggedInUrl("nonsense"), false);
  });
});
