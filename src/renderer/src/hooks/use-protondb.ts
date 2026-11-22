import { useEffect, useState } from "react";
import type { ProtonDBData } from "@types";
import { limitParallel } from "./protondb-core";

const responseCache = new Map<string, ProtonDBData | null>();
const inFlightRequests = new Map<string, Promise<ProtonDBData | null>>();

// Library pages can mount dozens of Steam cards at once; keep Hydra's API from a burst.
const withSlot = limitParallel(4);

/** ProtonDB summary for a game, cached for the session. Null when ProtonDB has none. */
export const getProtonDBData = (shop: string, objectId: string) => {
  const cacheKey = `${shop}:${objectId}`;

  if (responseCache.has(cacheKey)) {
    return Promise.resolve(responseCache.get(cacheKey) ?? null);
  }

  const inFlight = inFlightRequests.get(cacheKey);
  if (inFlight) return inFlight;

  const request = withSlot(() =>
    window.electron.hydraApi.get<ProtonDBData | null>(
      `/games/${shop}/${objectId}/protondb`,
      { needsAuth: false }
    )
  )
    .then((data) => {
      responseCache.set(cacheKey, data);
      return data;
    })
    // Errors are cached as "no data" too, so scrolling a library never retries a failing API.
    .catch(() => {
      responseCache.set(cacheKey, null);
      return null;
    })
    .finally(() => {
      inFlightRequests.delete(cacheKey);
    });

  inFlightRequests.set(cacheKey, request);
  return request;
};

/** ProtonDB data for a Steam game; always null for other shops (ProtonDB is keyed by Steam app id). */
export function useProtonDB(shop: string | undefined, objectId?: string) {
  const enabled = shop === "steam" && Boolean(objectId);
  const [state, setState] = useState<{
    isLoading: boolean;
    data: ProtonDBData | null;
  }>({ isLoading: enabled, data: null });

  useEffect(() => {
    if (!enabled || !objectId) {
      setState({ isLoading: false, data: null });
      return;
    }

    const cacheKey = `steam:${objectId}`;
    if (responseCache.has(cacheKey)) {
      setState({ isLoading: false, data: responseCache.get(cacheKey) ?? null });
      return;
    }

    let cancelled = false;
    setState({ isLoading: true, data: null });
    getProtonDBData("steam", objectId).then((data) => {
      if (!cancelled) setState({ isLoading: false, data });
    });

    return () => {
      cancelled = true;
    };
  }, [enabled, objectId]);

  return state;
}
