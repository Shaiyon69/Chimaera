// Builds a release's "What's new" notes from the Conventional Commit subjects since the previous
// release cut. The app's What's new panel reads these from the Skibidevs release body.
//
// Usage: node scripts/release-notes.mjs > notes.md
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const SECTIONS = [
  { type: "feat", title: "New" },
  { type: "fix", title: "Fixed" },
  { type: "perf", title: "Faster" },
];

const SUBJECT = /^(\w+)(?:\([^)]*\))?!?:\s*(.+)$/;

/** Markdown with one `## <section>` per change type; empty string when nothing user-facing changed. */
export function buildReleaseNotes(subjects) {
  const items = new Map(SECTIONS.map(({ type }) => [type, []]));

  for (const subject of subjects) {
    const match = SUBJECT.exec(subject.trim());
    if (!match || !items.has(match[1])) continue;
    const text = match[2].trim();
    const line = text.charAt(0).toUpperCase() + text.slice(1);
    if (!items.get(match[1]).includes(line)) items.get(match[1]).push(line);
  }

  return SECTIONS.filter(({ type }) => items.get(type).length > 0)
    .map(
      ({ type, title }) =>
        `## ${title}\n\n${items
          .get(type)
          .map((line) => `- ${line}`)
          .join("\n")}`
    )
    .join("\n\n");
}

const git = (...args) =>
  execFileSync("git", args, { encoding: "utf8" })
    .trim()
    .split("\n")
    .filter(Boolean);

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  // Newest "cut version" commit = this release, the one before = the last release.
  const cuts = git(
    "log",
    "--format=%H %s",
    "--grep=^chore(release): cut version",
    "-n",
    "2"
  );
  const { version } = JSON.parse(readFileSync("package.json", "utf8"));
  // No previous cut, or HEAD's version was not cut yet: no notes rather than the wrong commits.
  const ready = cuts.length === 2 && cuts[0].endsWith(` ${version}`);
  if (!ready) {
    process.stderr.write(
      `release notes: no cut commit for ${version}, notes left empty\n`
    );
  }
  const range = `${cuts[1]?.split(" ")[0]}..HEAD`;
  process.stdout.write(
    (ready
      ? buildReleaseNotes(git("log", "--no-merges", "--format=%s", range))
      : "") + "\n"
  );
}
