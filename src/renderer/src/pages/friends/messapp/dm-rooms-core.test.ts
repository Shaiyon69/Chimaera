import assert from "node:assert/strict";
import { test } from "node:test";
import { pairRooms } from "./dm-rooms-core.js";

const row = (dm_room_id: string, profile_id: string) => ({
  dm_room_id,
  profile_id,
});

test("maps each peer to the one-to-one room shared with me", () => {
  const rooms = pairRooms(
    [row("r1", "me"), row("r1", "ann"), row("r2", "me"), row("r2", "bob")],
    "me"
  );
  assert.deepEqual(
    [...rooms],
    [
      ["ann", "r1"],
      ["bob", "r2"],
    ]
  );
});

test("skips group rooms, solo rooms and rooms I am not in", () => {
  const rooms = pairRooms(
    [
      row("group", "me"),
      row("group", "ann"),
      row("group", "bob"),
      row("solo", "me"),
      row("theirs", "ann"),
      row("theirs", "bob"),
    ],
    "me"
  );
  assert.equal(rooms.size, 0);
});

test("leaves out a peer with two rooms so the RPC picks", () => {
  const rooms = pairRooms(
    [row("a", "me"), row("a", "ann"), row("b", "me"), row("b", "ann")],
    "me"
  );
  assert.equal(rooms.has("ann"), false);
});
