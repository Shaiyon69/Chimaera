import { useEffect, useRef } from "react";

import { useAppSelector } from "@renderer/hooks";
import { logger } from "@renderer/logger";

import { messapp } from "./client";
import { diff, toRows, type LibraryRow } from "./library-sync-core";

const TABLE = "chimaera_library";
const COLUMNS =
  "profile_id, shop, object_id, title, icon_url, playtime_ms, last_played_at, achievements_unlocked, achievements_total";
/** PostgREST answers at most 1000 rows by default. */
const PAGE = 1000;
const UPSERT_BATCH = 500;
/** Ids per delete request, so the query string stays short. */
const DELETE_BATCH = 100;
const FIRST_SYNC_MS = 5_000;
const DEBOUNCE_MS = 30_000;

const fetchServerRows = async (userId: string) => {
  const rows: LibraryRow[] = [];
  for (let from = 0; ; from += PAGE) {
    const { data, error } = await messapp
      .from(TABLE)
      .select(COLUMNS)
      .eq("profile_id", userId)
      .order("shop")
      .order("object_id")
      .range(from, from + PAGE - 1);
    if (error) throw error;
    rows.push(...(data as LibraryRow[]));
    if (data.length < PAGE) return rows;
  }
};

const push = async (
  userId: string,
  server: LibraryRow[],
  local: LibraryRow[]
) => {
  const { upsert, remove } = diff(server, local);
  for (let i = 0; i < upsert.length; i += UPSERT_BATCH) {
    const { error } = await messapp
      .from(TABLE)
      .upsert(upsert.slice(i, i + UPSERT_BATCH), {
        onConflict: "profile_id,shop,object_id",
      });
    if (error) throw error;
  }

  const byShop = new Map<string, string[]>();
  for (const row of remove) {
    byShop.set(row.shop, [...(byShop.get(row.shop) ?? []), row.object_id]);
  }
  for (const [shop, ids] of byShop) {
    for (let i = 0; i < ids.length; i += DELETE_BATCH) {
      const { error } = await messapp
        .from(TABLE)
        .delete()
        .eq("profile_id", userId)
        .eq("shop", shop)
        .in("object_id", ids.slice(i, i + DELETE_BATCH));
      if (error) throw error;
    }
  }
};

/**
 * Keeps MESSAPP's chimaera_library in step with this library, so friends see it
 * (link plan milestone 4). Only games friends may see are sent; mount once, app-wide.
 */
export function useLibrarySync(userId: string | null) {
  const games = useAppSelector((state) => state.library.value);
  const hasLoaded = useAppSelector((state) => state.library.hasLoaded);
  // What the server holds after the last good sync; null means read it again first.
  const synced = useRef<{ userId: string; rows: LibraryRow[] } | null>(null);
  // One sync at a time, so two writes never race.
  const queue = useRef<Promise<void>>(Promise.resolve());

  useEffect(() => {
    if (!userId || !hasLoaded) return;

    const run = async () => {
      try {
        const local = toRows(games, userId);
        const server =
          synced.current?.userId === userId
            ? synced.current.rows
            : await fetchServerRows(userId);
        await push(userId, server, local);
        synced.current = { userId, rows: local };
      } catch (err) {
        // A part-written sync leaves the server unknown; the next one reads it again.
        synced.current = null;
        logger.error("Library sync failed", err);
      }
    };

    const timer = setTimeout(
      () => {
        queue.current = queue.current.then(run);
      },
      synced.current?.userId === userId ? DEBOUNCE_MS : FIRST_SYNC_MS
    );
    return () => clearTimeout(timer);
  }, [userId, hasLoaded, games]);
}
