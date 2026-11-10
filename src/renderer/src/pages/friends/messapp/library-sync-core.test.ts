import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { diff, toRows, type SyncableGame } from "./library-sync-core.js";

const game = (over: Partial<SyncableGame> = {}): SyncableGame => ({
  shop: "steam",
  objectId: "620",
  title: "Portal 2",
  iconUrl: "https://cdn.example/620.png",
  playTimeInMilliseconds: 1000,
  lastTimePlayed: "2026-11-10T10:00:00.000Z",
  achievementCount: 51,
  unlockedAchievementCount: 12,
  ...over,
});

describe("toRows", () => {
  it("maps a game to a row", () => {
    assert.deepEqual(toRows([game()], "u1"), [
      {
        profile_id: "u1",
        shop: "steam",
        object_id: "620",
        title: "Portal 2",
        icon_url: "https://cdn.example/620.png",
        playtime_ms: 1000,
        last_played_at: "2026-11-10T10:00:00.000Z",
        achievements_unlocked: 12,
        achievements_total: 51,
      },
    ]);
  });

  it("skips deleted, hidden and untitled games", () => {
    const rows = toRows(
      [
        game({ objectId: "1", isDeleted: true }),
        game({ objectId: "2", isHiddenFromOthers: true }),
        game({ objectId: "3", title: "   " }),
        game({ objectId: "4" }),
      ],
      "u1"
    );
    assert.deepEqual(
      rows.map((r) => r.object_id),
      ["4"]
    );
  });

  it("keeps only https icons", () => {
    for (const iconUrl of [
      "local:C:/x.png",
      "cached-image://x",
      "http://x",
      null,
    ]) {
      assert.equal(
        toRows([game({ iconUrl })], "u1")[0].icon_url,
        null,
        String(iconUrl)
      );
    }
  });

  it("adds Steam and GOG playtime, caps titles, cleans counts and dates", () => {
    const [row] = toRows(
      [
        game({
          title: "x".repeat(300),
          playTimeInMilliseconds: 1000.4,
          steamPlayTimeInMilliseconds: 2000,
          gogPlayTimeInMilliseconds: 3000,
          lastTimePlayed: "not a date",
          achievementCount: undefined,
          unlockedAchievementCount: -1,
        }),
      ],
      "u1"
    );
    assert.equal(row.title.length, 200);
    assert.equal(row.playtime_ms, 6000);
    assert.equal(row.last_played_at, null);
    assert.equal(row.achievements_total, null);
    assert.equal(row.achievements_unlocked, null);
  });

  it("keeps the first copy of a duplicate key", () => {
    const rows = toRows([game({ title: "A" }), game({ title: "B" })], "u1");
    assert.deepEqual(
      rows.map((r) => r.title),
      ["A"]
    );
  });
});

describe("diff", () => {
  const [base] = toRows([game()], "u1");

  it("sends nothing when nothing synced changed", () => {
    // Postgres answers timestamps as "+00:00", not "Z".
    const server = [{ ...base, last_played_at: "2026-11-10T10:00:00+00:00" }];
    assert.deepEqual(diff(server, [base]), { upsert: [], remove: [] });
  });

  it("upserts new and changed rows and removes gone ones", () => {
    const changed = { ...base, playtime_ms: 5000 };
    const added = { ...base, object_id: "400", title: "Portal" };
    const gone = { ...base, object_id: "70", title: "Half-Life" };
    assert.deepEqual(diff([base, gone], [changed, added]), {
      upsert: [changed, added],
      remove: [gone],
    });
  });
});
