import type {
  SteamSourceAchievement,
  SteamSourceLibraryGame,
  SteamSnapshotAchievement,
  SteamSnapshotPayload,
} from "@types";

export const buildSteamSnapshotAchievements = (
  sourceAchievements: SteamSourceAchievement[]
): SteamSnapshotAchievement[] =>
  sourceAchievements.flatMap((achievement) => {
    if (!achievement.unlocked || !achievement.unlockTime) {
      return [];
    }

    return [
      {
        name: achievement.name,
        unlockTime: achievement.unlockTime,
      },
    ];
  });

export const buildSteamSnapshot = (
  games: SteamSourceLibraryGame[],
  achievementsByAppId: Map<string, SteamSourceAchievement[] | undefined>
): SteamSnapshotPayload => ({
  games: games.map((game) => {
    const steamAppId = String(game.steamAppId);
    const sourceAchievements = achievementsByAppId.get(steamAppId);
    const achievements = sourceAchievements
      ? buildSteamSnapshotAchievements(sourceAchievements)
      : undefined;

    return {
      steamAppId,
      name: game.name,
      playTimeInSeconds: game.playTimeInSeconds,
      lastPlayedAt: game.lastPlayedAt,
      ...(achievements !== undefined ? { achievements } : {}),
    };
  }),
});
