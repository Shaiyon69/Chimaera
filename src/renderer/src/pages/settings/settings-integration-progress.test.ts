import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { getSteamProgressPresentation } from "./settings-integration-progress.js";

const running = (
  phase: "library" | "achievements" | "merging" | "executables" | "finishing",
  gamesFound: number,
  gamesProcessed: number
) =>
  getSteamProgressPresentation({
    status: "running",
    syncRunId: "run",
    phase,
    gamesFound,
    gamesProcessed,
  });

describe("getSteamProgressPresentation", () => {
  it("uses indeterminate progress before the game count is known", () => {
    assert.deepEqual(running("library", 0, 0), {
      mode: "indeterminate",
      percentage: null,
      showCount: false,
      labelKey: "steam_syncing",
    });
    assert.equal(running("achievements", 0, 0)?.mode, "indeterminate");
  });

  it("maps achievements onto the middle of the bar", () => {
    assert.deepEqual(running("achievements", 4, 0), {
      mode: "determinate",
      percentage: 20,
      showCount: true,
      labelKey: "steam_syncing",
    });
    assert.equal(running("achievements", 4, 2)?.percentage, 55);
    assert.equal(running("achievements", 4, 4)?.percentage, 90);
  });

  it("tracks the local library update with a game count", () => {
    assert.deepEqual(running("merging", 0, 0), {
      mode: "determinate",
      percentage: 0,
      showCount: false,
      labelKey: "steam_sync_updating_library",
    });
    assert.deepEqual(running("merging", 8000, 4000), {
      mode: "determinate",
      percentage: 10,
      showCount: true,
      labelKey: "steam_sync_updating_library",
    });
  });

  it("counts the installed game scan and reaches the end before finishing", () => {
    assert.deepEqual(running("executables", 10, 5), {
      mode: "determinate",
      percentage: 95,
      showCount: true,
      labelKey: "steam_sync_scanning_games",
    });
    assert.deepEqual(running("finishing", 0, 0), {
      mode: "determinate",
      percentage: 100,
      showCount: false,
      labelKey: "steam_sync_finishing",
    });
  });

  it("clamps invalid processed counts", () => {
    assert.equal(running("achievements", 4, -1)?.percentage, 20);
    assert.equal(running("achievements", 4, 5)?.percentage, 90);
  });
});
