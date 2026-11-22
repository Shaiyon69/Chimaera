import { test } from "node:test";
import assert from "node:assert/strict";
import Color from "color";
import { pickAccent } from "./hero-accent.js";

const pixels = (...runs: [number, [number, number, number]][]) => {
  const out: number[] = [];
  for (const [count, [r, g, b]] of runs)
    for (let i = 0; i < count; i++) out.push(r, g, b, 255);
  return new Uint8ClampedArray(out);
};

test("a small vivid area beats a large dull one", () => {
  // Mostly murky blue-grey night with a bright orange fire.
  const accent = pickAccent(pixels([500, [30, 38, 44]], [76, [240, 120, 20]]));
  assert.ok(accent);
  const hue = Color(accent).hue();
  assert.ok(hue > 15 && hue < 45, `hue ${hue}`);
});

test("dark teal art yields a readable teal", () => {
  const accent = pickAccent(pixels([576, [10, 60, 70]]));
  assert.ok(accent);
  const color = Color(accent);
  assert.ok(color.hue() > 170 && color.hue() < 200, `hue ${color.hue()}`);
  assert.equal(Math.round(color.lightness()), 70);
});

test("grey art has no accent", () => {
  assert.equal(pickAccent(pixels([576, [120, 120, 122]])), null);
});
