import { getDisplayedPlayTimeInMilliseconds } from "../../../../../shared/playtime.js";

/** The fields of a library game the sync reads (a structural slice of LibraryGame). */
export interface SyncableGame {
  shop: string;
  objectId: string;
  title: string;
  iconUrl?: string | null;
  playTimeInMilliseconds?: number | null;
  steamPlayTimeInMilliseconds?: number | null;
  gogPlayTimeInMilliseconds?: number | null;
  epicPlayTimeInMilliseconds?: number | null;
  lastTimePlayed?: Date | string | null;
  achievementCount?: number;
  unlockedAchievementCount?: number;
  isDeleted?: boolean;
  isHiddenFromOthers?: boolean;
}

/** One row of MESSAPP's public.chimaera_library (migration 20261110120000). */
export interface LibraryRow {
  profile_id: string;
  shop: string;
  object_id: string;
  title: string;
  icon_url: string | null;
  playtime_ms: number;
  last_played_at: string | null;
  achievements_unlocked: number | null;
  achievements_total: number | null;
}

const TITLE_MAX = 200;
const ICON_MAX = 1000;

const count = (n: number | undefined) =>
  typeof n === "number" && Number.isFinite(n) && n >= 0 ? Math.floor(n) : null;

const isoDate = (value: Date | string | null | undefined) => {
  if (!value) return null;
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? null : date.toISOString();
};

/** Rows for the games friends may see; local-only icon URLs (local:, cached-image:) are dropped. */
export const toRows = (games: SyncableGame[], userId: string): LibraryRow[] => {
  const rows = new Map<string, LibraryRow>();
  for (const game of games) {
    if (game.isDeleted || game.isHiddenFromOthers) continue;
    const title = game.title?.trim().slice(0, TITLE_MAX);
    if (!title || !game.shop || !game.objectId) continue;
    const icon = game.iconUrl ?? null;
    const row: LibraryRow = {
      profile_id: userId,
      shop: game.shop,
      object_id: game.objectId,
      title,
      icon_url:
        icon && icon.startsWith("https://") && icon.length <= ICON_MAX
          ? icon
          : null,
      playtime_ms: Math.round(getDisplayedPlayTimeInMilliseconds(game)),
      last_played_at: isoDate(game.lastTimePlayed),
      achievements_unlocked: count(game.unlockedAchievementCount),
      achievements_total: count(game.achievementCount),
    };
    // The primary key is (profile_id, shop, object_id); the first copy wins.
    const key = rowKey(row);
    if (!rows.has(key)) rows.set(key, row);
  }
  return [...rows.values()];
};

export const rowKey = (row: Pick<LibraryRow, "shop" | "object_id">) =>
  `${row.shop}\u0000${row.object_id}`;

const same = (a: LibraryRow, b: LibraryRow) =>
  a.title === b.title &&
  a.icon_url === b.icon_url &&
  a.playtime_ms === b.playtime_ms &&
  // Postgres returns timestamps in its own format, so compare instants.
  isoDate(a.last_played_at) === isoDate(b.last_played_at) &&
  a.achievements_unlocked === b.achievements_unlocked &&
  a.achievements_total === b.achievements_total;

/** What to write so the server matches `local`. */
export const diff = (server: LibraryRow[], local: LibraryRow[]) => {
  const before = new Map(server.map((row) => [rowKey(row), row]));
  const upsert = local.filter((row) => {
    const old = before.get(rowKey(row));
    return !old || !same(old, row);
  });
  const keep = new Set(local.map(rowKey));
  const remove = server.filter((row) => !keep.has(rowKey(row)));
  return { upsert, remove };
};
