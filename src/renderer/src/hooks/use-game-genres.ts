import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { GameShop } from "@types";
import { logger } from "@renderer/logger";

export const gameGenreKey = (game: { shop: GameShop; objectId: string }) =>
  `${game.shop}:${game.objectId}`;

/** Store genres per game, keyed by `gameGenreKey`, plus every genre sorted by name. */
export function useGameGenres(games: { shop: GameShop; objectId: string }[]) {
  const { i18n } = useTranslation();
  const [genresByGame, setGenresByGame] = useState<Record<string, string[]>>(
    {}
  );

  // A stable key so re-renders with the same games do not refetch.
  const gamesKey = useMemo(
    () => [...new Set(games.map(gameGenreKey))].sort().join("|"),
    [games]
  );

  useEffect(() => {
    if (!gamesKey) return;
    let isStale = false;
    const list = gamesKey.split("|").map((key) => {
      const index = key.indexOf(":");
      return {
        shop: key.slice(0, index) as GameShop,
        objectId: key.slice(index + 1),
      };
    });
    window.electron
      .getGameGenres(list, i18n.language)
      .then((result) => {
        if (!isStale) setGenresByGame((current) => ({ ...current, ...result }));
      })
      .catch((error) => logger.error("Failed to load game genres", error));
    return () => {
      isStale = true;
    };
  }, [gamesKey, i18n.language]);

  const genres = useMemo(
    () =>
      [...new Set(Object.values(genresByGame).flat())].sort((a, b) =>
        a.localeCompare(b)
      ),
    [genresByGame]
  );

  return { genresByGame, genres };
}
