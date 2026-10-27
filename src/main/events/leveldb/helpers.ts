import { db, levelKeys } from "@main/level";

const sublevelCache = new Map<
  string,
  ReturnType<typeof db.sublevel<string, unknown>>
>();

// Only what the renderer actually uses. Anything else (steamAccount, auth,
// user details) stays reachable from the main process only.
const READABLE_ROOT_KEYS = new Set<string>([
  levelKeys.userPreferences,
  levelKeys.downloadSourcesSinceValue,
  "searchHistory",
]);
const WRITABLE_ROOT_KEYS = new Set<string>(["searchHistory"]);
const READABLE_SUBLEVELS = new Set<string>([
  levelKeys.themes,
  levelKeys.games,
  levelKeys.downloadSources,
]);
const WRITABLE_SUBLEVELS = new Set<string>([levelKeys.themes, levelKeys.games]);

/**
 * Throws unless the renderer may touch this key or sublevel.
 * @param write - true for put/del/clear
 * @param sublevelName - sublevel, or null/undefined for a root key
 * @param key - root key, checked only when there is no sublevel
 */
export const assertRendererAccess = (
  write: boolean,
  sublevelName?: string | null,
  key?: string
) => {
  const allowed = sublevelName
    ? (write ? WRITABLE_SUBLEVELS : READABLE_SUBLEVELS).has(sublevelName)
    : key !== undefined &&
      (write ? WRITABLE_ROOT_KEYS : READABLE_ROOT_KEYS).has(key);

  if (!allowed) {
    throw new Error(
      `LevelDB access denied: ${sublevelName ?? "root"}/${sublevelName ? "*" : key}`
    );
  }
};

/**
 * Gets a sublevel by name, creating it if it doesn't exist.
 * All sublevels use "json" encoding by default.
 * @param sublevelName - The name of the sublevel to get or create
 * @returns The sublevel instance
 */
export const getSublevelByName = (
  sublevelName: string
): ReturnType<typeof db.sublevel<string, unknown>> => {
  if (sublevelCache.has(sublevelName)) {
    return sublevelCache.get(sublevelName)!;
  }

  // All sublevels use "json" encoding - this cannot be changed per sublevel
  const sublevel = db.sublevel<string, unknown>(sublevelName, {
    valueEncoding: "json",
  });
  sublevelCache.set(sublevelName, sublevel);
  return sublevel;
};
