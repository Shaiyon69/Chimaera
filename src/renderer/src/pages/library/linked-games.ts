import type { Game, LinkedGame } from "@types";

type GroupedGame = Pick<Game, "shop" | "launcher"> & {
  executablePath?: string | null;
  linkedGames?: LinkedGame[];
};

export interface LinkedExecutable {
  launcher: LinkedGame["launcher"];
  executablePath: string;
}

const ownLauncher = (game: GroupedGame): LinkedGame["launcher"] =>
  game.shop === "steam" ? "steam" : (game.launcher ?? null);

/** Launchers that own a grouped library entry, the winner's first. Empty when it has no copies. */
export const linkedLauncherIds = (game: GroupedGame): string[] => {
  if (!game.linkedGames?.length) return [];
  const ids = [ownLauncher(game), ...game.linkedGames.map((l) => l.launcher)];
  return [...new Set(ids.filter((id): id is NonNullable<typeof id> => !!id))];
};

/** One entry per distinct exe across the winner and its copies (Windows paths ignore case). */
export const linkedExecutables = (game: GroupedGame): LinkedExecutable[] => {
  const seen = new Set<string>();
  const copies = [
    { launcher: ownLauncher(game), executablePath: game.executablePath },
    ...(game.linkedGames ?? []),
  ];
  const result: LinkedExecutable[] = [];
  for (const { launcher, executablePath } of copies) {
    const key = executablePath?.replaceAll("/", "\\").toLowerCase();
    if (!executablePath || !key || seen.has(key)) continue;
    seen.add(key);
    result.push({ launcher, executablePath });
  }
  return result;
};
