import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { resolveSteamSessionPlaytimePolicy } from "./steam-playtime.js";

describe("Steam session playtime policy", () => {
  it("deduplicates imported Steam library sessions and syncs on exit", () => {
    assert.deepEqual(
      resolveSteamSessionPlaytimePolicy({
        hasActiveSteamImport: true,
        isSteamLibraryPath: true,
        enableHydraPlaytimeTracking: false,
      }),
      { countHydraPlaytime: false, syncSteamOnExit: true }
    );
  });

  it("counts imported games launched from outside Steam libraries", () => {
    assert.deepEqual(
      resolveSteamSessionPlaytimePolicy({
        hasActiveSteamImport: true,
        isSteamLibraryPath: false,
        enableHydraPlaytimeTracking: false,
      }),
      { countHydraPlaytime: true, syncSteamOnExit: false }
    );
  });

  it("allows an imported game to opt in without changing exit sync eligibility", () => {
    assert.deepEqual(
      resolveSteamSessionPlaytimePolicy({
        hasActiveSteamImport: true,
        isSteamLibraryPath: true,
        enableHydraPlaytimeTracking: true,
      }),
      { countHydraPlaytime: true, syncSteamOnExit: true }
    );

    assert.deepEqual(
      resolveSteamSessionPlaytimePolicy({
        hasActiveSteamImport: true,
        isSteamLibraryPath: false,
        enableHydraPlaytimeTracking: true,
      }),
      { countHydraPlaytime: true, syncSteamOnExit: false }
    );
  });

  it("counts Hydra playtime after the Steam import is disconnected", () => {
    assert.deepEqual(
      resolveSteamSessionPlaytimePolicy({
        hasActiveSteamImport: false,
        isSteamLibraryPath: false,
        enableHydraPlaytimeTracking: false,
      }),
      { countHydraPlaytime: true, syncSteamOnExit: false }
    );
  });
});
