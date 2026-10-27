import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { dmPeerRooms, sortByRecency } from "./recency.js";

describe("recency", () => {
  it("maps each peer to the room shared with only them", () => {
    const peers = dmPeerRooms(
      [
        { dm_room_id: "r1", profile_id: "me" },
        { dm_room_id: "r1", profile_id: "ann" },
        { dm_room_id: "r2", profile_id: "me" },
        { dm_room_id: "r2", profile_id: "bob" },
        { dm_room_id: "r2", profile_id: "cy" },
        { dm_room_id: "r3", profile_id: "ann" },
        { dm_room_id: "r3", profile_id: "dee" },
      ],
      "me"
    );

    assert.deepEqual([...peers], [["ann", "r1"]]);
  });

  it("sorts talked-to friends newest first and leaves the rest in place", () => {
    const rows = ["ann", "bob", "cy", "dee"].map((id) => ({
      profile: { id },
    }));
    const last = new Map([
      ["cy", { createdAt: "2026-10-24T10:00:00Z", mine: true, text: "hi" }],
      ["bob", { createdAt: "2026-10-24T12:00:00Z", mine: false, text: "yo" }],
    ]);

    assert.deepEqual(
      sortByRecency(rows, last).map((r) => r.profile.id),
      ["bob", "cy", "ann", "dee"]
    );
  });
});
