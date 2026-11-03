import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { parseMessageInsert } from "./realtime-core.js";

const insert = {
  type: "INSERT",
  id: "m1",
  profile_id: "u2",
  created_at: "2026-11-03T12:00:00Z",
  dm_room_id: "r1",
  channel_id: null,
  thread_root_id: null,
};

describe("parseMessageInsert", () => {
  it("reads a DM insert", () => {
    assert.deepEqual(parseMessageInsert(insert), { id: "m1", roomId: "r1" });
  });

  it("ignores updates, deletes and channel messages", () => {
    assert.equal(parseMessageInsert({ ...insert, type: "UPDATE" }), null);
    assert.equal(parseMessageInsert({ ...insert, type: "DELETE" }), null);
    assert.equal(
      parseMessageInsert({ ...insert, dm_room_id: null, channel_id: "c1" }),
      null
    );
  });

  it("ignores junk", () => {
    assert.equal(parseMessageInsert(null), null);
    assert.equal(parseMessageInsert("x"), null);
    assert.equal(parseMessageInsert({ type: "INSERT" }), null);
  });
});
