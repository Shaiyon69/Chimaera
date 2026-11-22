import { useEffect, useState } from "react";
import type { ProtonDBData } from "@types";
import { limitParallel } from "./protondb-core";

// A failed request (offline, rate limit) is retried after this, not on every remount.
const FAILURE_TTL_MS = 5 * 60 * 1000;

const responseCache = new Map<
  string,
  { data: ProtonDBData | null; expires: number }
>();
const inFlightRequests = new Map<string, Promise<ProtonDBData | null>>();
// Mounted components waiting on each game; a queued request nobody waits for is skipped.
const wanted = new Map<string, number>();

// Library pages can mount dozens of Steam cards at once; keep Hydra's API from a burst.
const withSlot = limitParallel(4);
const SKIPPED = Symbol("skipped");

const readCache = (key: string) => {
  const entry = responseCache.get(key);
  if (!entry) return undefined;
  if (entry.expires > Date.now()) return entry.data;
  responseCache.delete(key);
  return undefined;
};

const want = (key: string, delta: 1 | -1) => {
  const count = (wanted.get(key) ?? 0) + delta;
  if (count > 0) wanted.set(key, count);
  else wanted.delete(key);
};

const loadProtonDB = (key: string, objectId: string) => {
  const inFlight = inFlightRequests.get(key);
  if (inFlight) return inFlight;

  const request = withSlot(() =>
    wanted.has(key)
      ? window.electron.hydraApi.get<ProtonDBData | null>(
          `/games/steam/${objectId}/protondb`,
          { needsAuth: false }
        )
      : Promise.reject(SKIPPED)
  )
    .then((data) => {
      responseCache.set(key, { data, expires: Infinity });
      return data;
    })
    .catch((error) => {
      if (error !== SKIPPED) {
        responseCache.set(key, {
          data: null,
          expires: Date.now() + FAILURE_TTL_MS,
        });
      }
      return null;
    })
    .finally(() => {
      inFlightRequests.delete(key);
    });

  inFlightRequests.set(key, request);
  return request;
};

/** ProtonDB data for a Steam game; always null for other shops (ProtonDB is keyed by Steam app id). */
export function useProtonDB(shop: string | undefined, objectId?: string) {
  const key = shop === "steam" && objectId ? `steam:${objectId}` : null;
  // Only a re-render trigger; the answer itself lives in the shared cache.
  const [, setLoadedKey] = useState<string | null>(null);

  useEffect(() => {
    if (!key || !objectId || readCache(key) !== undefined) return;

    let cancelled = false;
    want(key, 1);
    loadProtonDB(key, objectId).then(() => {
      if (!cancelled) setLoadedKey(key);
    });

    return () => {
      cancelled = true;
      want(key, -1);
    };
  }, [key, objectId]);

  if (!key) return { isLoading: false, data: null };
  const cached = readCache(key);
  return cached === undefined
    ? { isLoading: true, data: null }
    : { isLoading: false, data: cached };
}
