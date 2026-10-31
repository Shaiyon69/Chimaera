import assert from "node:assert/strict";
import { describe, it } from "node:test";

// @ts-ignore The Node ESM test runner requires the source extension.
import * as linkedGames from "./linked-games.ts";

const { linkedLauncherIds, linkedExecutables } = linkedGames;

const steamWinner = {
  shop: "steam" as const,
  launcher: undefined,
  executablePath: "C:\\Steam\\LN2.exe",
  linkedGames: [
    {
      id: "custom:gog-1",
      shop: "custom" as const,
      objectId: "gog-1",
      launcher: "gog" as const,
      executablePath: "C:\\GOG\\LN2.exe",
    },
    {
      id: "custom:epic-1",
      shop: "custom" as const,
      objectId: "epic-1",
      launcher: null,
      executablePath: "c:\\steam\\ln2.EXE",
    },
  ],
};

describe("linkedLauncherIds", () => {
  it("lists the winner first, without repeats or unknown launchers", () => {
    assert.deepEqual(linkedLauncherIds(steamWinner), ["steam", "gog"]);
  });

  it("is empty for a game with no linked copies", () => {
    assert.deepEqual(
      linkedLauncherIds({ shop: "steam", launcher: undefined }),
      []
    );
  });
});

describe("linkedExecutables", () => {
  it("keeps one entry per exe, compared case-insensitively", () => {
    assert.deepEqual(linkedExecutables(steamWinner), [
      { launcher: "steam", executablePath: "C:\\Steam\\LN2.exe" },
      { launcher: "gog", executablePath: "C:\\GOG\\LN2.exe" },
    ]);
  });

  it("skips copies without an exe", () => {
    assert.deepEqual(
      linkedExecutables({
        shop: "custom",
        launcher: "gog",
        executablePath: null,
        linkedGames: [
          {
            id: "steam:1",
            shop: "steam",
            objectId: "1",
            launcher: "steam",
            executablePath: null,
          },
        ],
      }),
      []
    );
  });
});
