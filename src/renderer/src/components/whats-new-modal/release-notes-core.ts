export interface ReleaseNotesSection {
  title: string | null;
  items: string[];
}

// Inline markdown the notes may carry, reduced to plain text: links, code, bold, italics.
const plain = (text: string) =>
  text
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/[`*_]+/g, "")
    .trim();

/**
 * Release markdown as sections of plain-text items. Only headings and list lines carry meaning;
 * nothing is rendered as HTML. The old placeholder body ("Chimaera v2.1.2") gives no sections.
 */
export function parseReleaseNotes(markdown: string): ReleaseNotesSection[] {
  const sections: ReleaseNotesSection[] = [];
  let current: ReleaseNotesSection | null = null;

  for (const raw of markdown.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || /^chimaera v[\d.]+\S*$/i.test(line)) continue;

    const heading = /^#{1,6}\s+(.+)$/.exec(line);
    if (heading) {
      current = { title: plain(heading[1]), items: [] };
      sections.push(current);
      continue;
    }

    const item = plain(line.replace(/^([-*+]|\d+\.)\s+/, ""));
    if (!item) continue;
    if (!current) {
      current = { title: null, items: [] };
      sections.push(current);
    }
    current.items.push(item);
  }

  return sections.filter((section) => section.items.length > 0);
}
