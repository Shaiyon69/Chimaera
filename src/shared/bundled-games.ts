/** Games Chimaera installs and launches itself, as `custom` shop entries. */
export interface BundledGame {
  objectId: string;
  title: string;
  /** Also claims records by title; Bloodborne records can predate its fixed id. */
  titlePattern?: RegExp;
  achievements: boolean;
  /** False where OptiScaler can't apply (Vulkan engine with its own upscalers, or not a D3D game). */
  optiscaler: boolean;
  /** Shows a Launcher button beside Play; off where Play already opens the launcher. */
  launcherButton: boolean;
  /** What the catalogue card shows. */
  card: {
    genres: string[];
    releaseYear: number | null;
    developers: string[];
    publishers: string[];
  };
}

export const BUNDLED_GAMES: BundledGame[] = [
  {
    objectId: "bloodborne",
    title: "Bloodborne",
    titlePattern: /^bloodborne\b/i,
    achievements: true,
    optiscaler: false,
    launcherButton: true,
    card: {
      genres: ["Action", "RPG"],
      releaseYear: 2015,
      developers: ["FromSoftware"],
      publishers: ["Sony Interactive Entertainment"],
    },
  },
  {
    objectId: "horizon",
    title: "Minecraft (Horizon)",
    achievements: false,
    optiscaler: false,
    launcherButton: false,
    card: {
      genres: ["Sandbox", "Survival"],
      releaseYear: null,
      developers: ["Horizon contributors"],
      publishers: ["Horizon contributors"],
    },
  },
];

export const findBundledGame = (
  game?: {
    shop?: string | null;
    objectId?: string | null;
    title?: string | null;
  } | null
): BundledGame | null =>
  BUNDLED_GAMES.find(
    (g) =>
      (game?.shop === "custom" && game.objectId === g.objectId) ||
      g.titlePattern?.test(game?.title?.trim() ?? "")
  ) ?? null;
