import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { isUnread, seedReadMarks, unreadPeers } from "./unread-core.js";

describe("unread", () => {
  it("flags only their newer messages", () => {
    const theirs = { createdAt: "2026-01-02T00:00:00+00:00", mine: false };

    assert.equal(isUnread(theirs, undefined), true);
    assert.equal(isUnread(theirs, "2026-01-01T00:00:00+00:00"), true);
    assert.equal(isUnread(theirs, theirs.createdAt), false);
    assert.equal(isUnread({ ...theirs, mine: true }, undefined), false);
    assert.equal(isUnread(undefined, undefined), false);
  });

  it("collects unread peers", () => {
    const latest = new Map([
      ["ann", { createdAt: "2026-01-02T00:00:00+00:00", mine: false }],
      ["bob", { createdAt: "2026-01-02T00:00:00+00:00", mine: true }],
      ["cy", { createdAt: "2026-01-02T00:00:00+00:00", mine: false }],
    ]);

    const unread = unreadPeers(latest, { cy: "2026-01-02T00:00:00+00:00" });

    assert.deepEqual([...unread], ["ann"]);
  });

  it("seeds every conversation as read", () => {
    const latest = new Map([
      ["ann", { createdAt: "2026-01-02T00:00:00+00:00", mine: false }],
    ]);

    assert.equal(unreadPeers(latest, seedReadMarks(latest)).size, 0);
  });
});
