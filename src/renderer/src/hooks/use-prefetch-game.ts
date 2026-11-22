import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import type { GameShop } from "@types";

// Resting on a card this long counts as interest; sweeping across a row does not.
const HOVER_DELAY_MS = 120;
const warmed = new Set<string>();

/**
 * Hover handlers that warm the game page's caches (store details, artwork) in the Rust store,
 * so a click opens the page from cache instead of waiting on Steam or Hydra. Once per game per run.
 */
export function usePrefetchGame(
  shop: GameShop | undefined,
  objectId: string | undefined
) {
  const { i18n } = useTranslation();
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
    },
    []
  );

  const onMouseEnter = () => {
    if (!shop || !objectId || shop === "custom") return;
    const key = `${shop}:${objectId}:${i18n.language}`;
    if (warmed.has(key)) return;
    timer.current = setTimeout(() => {
      warmed.add(key);
      // Errors only mean the page fetches on open, as it did before.
      window.electron
        .getGameShopDetails(objectId, shop, i18n.language)
        .catch(() => undefined);
      window.electron.getGameAssets(objectId, shop).catch(() => undefined);
    }, HOVER_DELAY_MS);
  };

  const onMouseLeave = () => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = null;
  };

  return { onMouseEnter, onMouseLeave };
}
