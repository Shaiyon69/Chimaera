import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { trophyAchievementName } from "./bloodborne-trophies-core.js";

describe("bloodborne trophies", () => {
  it("slugs names like the catalogue", () => {
    assert.equal(trophyAchievementName("Yharnam Sunrise"), "YHARNAM_SUNRISE");
  });
});
