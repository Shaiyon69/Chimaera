import { test } from "node:test";
import assert from "node:assert/strict";
import { buildReleaseNotes } from "./release-notes.mjs";

test("groups user-facing commits and drops the rest", () => {
  const notes = buildReleaseNotes([
    "feat(protondb): show tier badges on every platform",
    "fix(linux): game shortcuts point at the AppImage file",
    "chore(release): cut version 2.2.0",
    "ci: install libxdo for the Linux build",
    "perf: cache cover art",
    "Merge branch 'feat/noctalia-theme'",
    "feat!: drop the old theme format",
  ]);

  assert.equal(
    notes,
    [
      "## New",
      "",
      "- Show tier badges on every platform",
      "- Drop the old theme format",
      "",
      "## Fixed",
      "",
      "- Game shortcuts point at the AppImage file",
      "",
      "## Faster",
      "",
      "- Cache cover art",
    ].join("\n")
  );
});

test("repeated subjects appear once and an empty range gives no notes", () => {
  assert.equal(
    buildReleaseNotes(["fix: crash on start", "fix(ui): crash on start"]),
    "## Fixed\n\n- Crash on start"
  );
  assert.equal(buildReleaseNotes(["chore: tidy", "docs: readme"]), "");
});
