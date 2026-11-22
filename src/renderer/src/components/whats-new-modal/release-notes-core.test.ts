import { test } from "node:test";
import assert from "node:assert/strict";
import { parseReleaseNotes } from "./release-notes-core.js";

test("reads sections and items as plain text", () => {
  const notes = parseReleaseNotes(
    "## New\n\n- Show **tier** badges in `library`\n- See [ProtonDB](https://protondb.com)\n\n## Fixed\n\n* Crash on start\n<script>alert(1)</script>"
  );
  assert.deepEqual(notes, [
    {
      title: "New",
      items: ["Show tier badges in library", "See ProtonDB"],
    },
    {
      title: "Fixed",
      items: ["Crash on start", "<script>alert(1)</script>"],
    },
  ]);
});

test("old placeholder bodies and empty headings give no notes", () => {
  assert.deepEqual(parseReleaseNotes("Chimaera v2.1.2"), []);
  assert.deepEqual(parseReleaseNotes("## New\n\n## Fixed\n"), []);
});

test("text before any heading lands in an untitled section", () => {
  assert.deepEqual(parseReleaseNotes("Hotfix for the updater.\n"), [
    { title: null, items: ["Hotfix for the updater."] },
  ]);
});
