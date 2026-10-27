import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { dayDiff, groupLines } from "./message-groups.js";

const at = (day: number, hour: number, minute = 0) =>
  new Date(2026, 9, day, hour, minute).toISOString();

const line = (id: string, mine: boolean, createdAt: string) => ({
  id,
  mine,
  createdAt,
});

describe("message groups", () => {
  it("opens each day with a separator", () => {
    const items = groupLines([
      line("a", true, at(23, 23, 50)),
      line("b", true, at(24, 0, 5)),
    ]);

    assert.deepEqual(
      items.map((i) => i.kind),
      ["day", "line", "day", "line"]
    );
  });

  it("groups one sender's messages within five minutes", () => {
    const items = groupLines([
      line("a", true, at(24, 10, 0)),
      line("b", true, at(24, 10, 4)),
      line("c", true, at(24, 10, 20)),
    ]).filter((i) => i.kind === "line");

    assert.deepEqual(
      items.map((i) => [i.startsGroup, i.endsGroup]),
      [
        [true, false],
        [false, true],
        [true, true],
      ]
    );
  });

  it("splits a group when the sender changes", () => {
    const items = groupLines([
      line("a", true, at(24, 10, 0)),
      line("b", false, at(24, 10, 1)),
    ]).filter((i) => i.kind === "line");

    assert.deepEqual(
      items.map((i) => [i.startsGroup, i.endsGroup]),
      [
        [true, true],
        [true, true],
      ]
    );
  });

  it("splits a group at midnight even inside five minutes", () => {
    const items = groupLines([
      line("a", true, at(23, 23, 59)),
      line("b", true, at(24, 0, 1)),
    ]).filter((i) => i.kind === "line");

    assert.equal(items[0].endsGroup, true);
    assert.equal(items[1].startsGroup, true);
  });

  it("counts calendar days, not 24-hour spans", () => {
    const now = new Date(2026, 9, 24, 0, 30);
    assert.equal(dayDiff(at(24, 0, 1), now), 0);
    assert.equal(dayDiff(at(23, 23, 59), now), 1);
    assert.equal(dayDiff(at(20, 12), now), 4);
  });

  it("handles no messages", () => {
    assert.deepEqual(groupLines([]), []);
  });
});
