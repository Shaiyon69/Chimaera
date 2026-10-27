import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { createThrottle } from "./typing.js";

describe("typing throttle", () => {
  it("lets the first signal through, then blocks until the interval passes", () => {
    let time = 10_000;
    const allow = createThrottle(3000, () => time);

    assert.equal(allow(), true);
    time += 2999;
    assert.equal(allow(), false);
    time += 1;
    assert.equal(allow(), true);
  });

  it("measures from the last signal that went out, not the last attempt", () => {
    let time = 0;
    const allow = createThrottle(3000, () => time);

    allow();
    time = 1000;
    allow();
    time = 3000;
    assert.equal(allow(), true);
  });
});
