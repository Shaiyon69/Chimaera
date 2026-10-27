import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { toFriendPresence, toPayload } from "./presence-core.js";

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
