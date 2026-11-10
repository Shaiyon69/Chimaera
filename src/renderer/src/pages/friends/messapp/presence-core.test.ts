import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { playtimeLabel, toFriendPresence, toPayload } from "./presence-core.js";

const game = {
  title: "Hades",
  objectId: "1145360",
  shop: "steam",
  iconUrl: null,
};

describe("toFriendPresence", () => {
  it("is offline with no entries", () => {
    assert.deepEqual(toFriendPresence([]), { online: false, playing: null });
  });

  it("is online and not playing when the entry has no game", () => {
    assert.deepEqual(toFriendPresence([{ playing: null, since: 1 }]), {
      online: true,
      playing: null,
    });
  });

  it("takes the newest entry across several windows", () => {
    const result = toFriendPresence([
      { playing: game, since: 5 },
      { playing: null, since: 9 },
    ]);
    assert.equal(result.playing, null);
  });

  it("drops a malformed game", () => {
    const result = toFriendPresence([
      { playing: { title: 3 } as never, since: 1 },
    ]);
    assert.deepEqual(result, { online: true, playing: null });
  });
});

describe("toPayload", () => {
  it("hides the game when sharing is off", () => {
    assert.equal(toPayload(game, false, 1).playing, null);
    assert.deepEqual(toPayload(game, true, 1), { playing: game, since: 1 });
  });
});

describe("playtime", () => {
  it("keeps a valid start time and drops junk", () => {
    const at = (startedAt: unknown) =>
      toFriendPresence(
        [{ playing: { ...game, startedAt } as never, since: 1 }],
        1_000_000
      ).playing?.startedAt;
    assert.equal(at(500_000), 500_000);
    assert.equal(at("500000"), null);
    assert.equal(at(-1), null);
    assert.equal(at(9_000_000_000), null);
  });

  it("formats elapsed time", () => {
    assert.equal(playtimeLabel(null, 0), null);
    assert.equal(playtimeLabel(0, 30_000), null);
    assert.equal(playtimeLabel(1, 12 * 60_000 + 1), "12m");
    assert.equal(playtimeLabel(1, 65 * 60_000 + 1), "1h 05m");
  });
});
