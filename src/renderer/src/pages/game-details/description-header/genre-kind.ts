export type GenreKind =
  | "action"
  | "strategy"
  | "rpg"
  | "casual"
  | "racing"
  | "sports"
  | "indie"
  | "adventure"
  | "simulation"
  | "multiplayer"
  | "free"
  | "early_access"
  | "violent"
  | "nudity"
  | "horror"
  | "puzzle"
  | "platformer"
  | "shooter"
  | "fighting"
  | "sandbox"
  | "survival"
  | "music"
  | "tools"
  | "creative"
  | "video"
  | "other";

// Steam's fixed genre ids; its names are localized, so match on id first.
const STEAM_IDS: Record<string, GenreKind> = {
  "1": "action",
  "2": "strategy",
  "3": "rpg",
  "4": "casual",
  "9": "racing",
  "18": "sports",
  "23": "indie",
  "25": "adventure",
  "28": "simulation",
  "29": "multiplayer",
  "37": "free",
  "51": "creative",
  "53": "creative",
  "57": "tools",
  "58": "video",
  "70": "early_access",
  "72": "nudity",
  "73": "violent",
  "74": "violent",
};

// Checked in order: "Action RPG" is an RPG, "Survival Horror" is horror.
const KEYWORDS: [RegExp, GenreKind][] = [
  [/horror/, "horror"],
  [/rpg|role.?playing/, "rpg"],
  [/shoot|fps/, "shooter"],
  [/fight|beat/, "fighting"],
  [/platform/, "platformer"],
  [/puzzle/, "puzzle"],
  [/racing|driving/, "racing"],
  [/sport/, "sports"],
  [/strateg|tactic/, "strategy"],
  [/simulat/, "simulation"],
  [/sandbox/, "sandbox"],
  [/survival/, "survival"],
  [/multiplayer|mmo/, "multiplayer"],
  [/music|rhythm/, "music"],
  [/adventure/, "adventure"],
  [/action/, "action"],
  [/casual|party/, "casual"],
  [/indie/, "indie"],
  [/free/, "free"],
  [/early access/, "early_access"],
];

export function genreKind(
  genre: { id: string; name?: string; description?: string },
  fromSteam: boolean
): GenreKind {
  if (fromSteam && STEAM_IDS[genre.id]) return STEAM_IDS[genre.id];
  const label = (genre.name || genre.description || "").toLowerCase();
  return KEYWORDS.find(([pattern]) => pattern.test(label))?.[1] ?? "other";
}
