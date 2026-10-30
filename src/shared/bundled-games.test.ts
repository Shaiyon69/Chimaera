import { test } from "node:test";
import assert from "node:assert/strict";
import { findBundledGame } from "./bundled-games.js";

test("matches bundled games by custom shop id", () => {
  assert.equal(
    findBundledGame({ shop: "custom", objectId: "horizon" })?.objectId,
    "horizon"
  );
  assert.equal(findBundledGame({ shop: "steam", objectId: "horizon" }), null);
});

test("only Bloodborne claims records by title", () => {
  assert.equal(
    findBundledGame({
      shop: "custom",
      objectId: "x1",
      title: "Bloodborne GOTY",
    })?.objectId,
    "bloodborne"
  );
  assert.equal(
    findBundledGame({
      shop: "custom",
      objectId: "x2",
      title: "Minecraft (Horizon)",
    }),
    null
  );
});
