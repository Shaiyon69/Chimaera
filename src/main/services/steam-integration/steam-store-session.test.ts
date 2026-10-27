import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  parseSteamStoreSessionConfig,
  SteamSessionRequiredError,
} from "./steam-store-session-config.ts";

describe("parseSteamStoreSessionConfig", () => {
  it("reads steamid and webapi_token from application_config", () => {
    assert.deepEqual(
      parseSteamStoreSessionConfig({
        currentUrl: "https://store.steampowered.com/explore/",
        userInfoRaw: JSON.stringify({
          logged_in: true,
          steamid: "76561199208012825",
        }),
        storeConfigRaw: JSON.stringify({ webapi_token: "store-token" }),
      }),
      {
        steamId64: "76561199208012825",
        accessToken: "store-token",
      }
    );
  });

  it("rejects a store login redirect", () => {
    assert.throws(
      () =>
        parseSteamStoreSessionConfig({
          currentUrl: "https://store.steampowered.com/login/",
          userInfoRaw: JSON.stringify({
            logged_in: true,
            steamid: "76561199208012825",
          }),
          storeConfigRaw: JSON.stringify({ webapi_token: "store-token" }),
        }),
      SteamSessionRequiredError
    );
  });

  it("rejects a logged-out store page", () => {
    assert.throws(
      () =>
        parseSteamStoreSessionConfig({
          currentUrl: "https://store.steampowered.com/explore/",
          userInfoRaw: JSON.stringify({ logged_in: false }),
          storeConfigRaw: JSON.stringify({ webapi_token: "store-token" }),
        }),
      SteamSessionRequiredError
    );
  });
});
