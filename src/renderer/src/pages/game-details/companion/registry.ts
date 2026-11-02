/** Scheme served by src-tauri/src/companion.rs from the bundled companion builds. */
export const COMPANION_SCHEME = "companion";

export interface Companion {
  /** Folder under resources/companions, also the web build's base path. */
  id: string;
  shop: string;
  objectId: string;
  /** Key in the game_details namespace. */
  titleKey: string;
}

const COMPANIONS: Companion[] = [
  {
    id: "terraria",
    shop: "steam",
    objectId: "105600",
    titleKey: "companion_terraria",
  },
];

export const findCompanion = (
  shop: string,
  objectId: string | undefined
): Companion | null =>
  COMPANIONS.find((c) => c.shop === shop && c.objectId === objectId) ?? null;

/** URL of a companion's page, or null outside Tauri (no scheme to serve it). */
export const companionUrl = (id: string): string | null => {
  const tauri = (
    globalThis as {
      __TAURI_INTERNALS__?: {
        convertFileSrc: (path: string, protocol: string) => string;
      };
    }
  ).__TAURI_INTERNALS__;
  // Appended after conversion so the slash stays a path separator, not %2F.
  return tauri
    ? `${tauri.convertFileSrc(id, COMPANION_SCHEME)}/index.html`
    : null;
};
