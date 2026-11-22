import { test } from "node:test";
import assert from "node:assert/strict";
import { limitParallel } from "./protondb-core.js";

test("runs at most max tasks at once and finishes them all", async () => {
  const run = limitParallel(4);
  let active = 0;
  let peak = 0;

  const results = await Promise.all(
    Array.from({ length: 10 }, (_, i) =>
      run(async () => {
        active += 1;
        peak = Math.max(peak, active);
        await new Promise((resolve) => setTimeout(resolve, 5));
        active -= 1;
        return i;
      })
    )
  );

  assert.equal(peak, 4);
  assert.deepEqual(results, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
});

test("a failing task frees its slot", async () => {
  const run = limitParallel(1);
  await assert.rejects(run(() => Promise.reject(new Error("boom"))));
  assert.equal(await run(async () => "next"), "next");
});

test("a caller arriving during a handoff still waits", async () => {
  const run = limitParallel(1);
  let active = 0;
  let peak = 0;
  const task = async () => {
    active += 1;
    peak = Math.max(peak, active);
    await Promise.resolve();
    active -= 1;
  };

  const first = run(task);
  const second = run(task);
  // Queued while `first` is finishing and handing its slot to `second`.
  const third = first.then(() => run(task));
  await Promise.all([first, second, third]);
  assert.equal(peak, 1);
});
