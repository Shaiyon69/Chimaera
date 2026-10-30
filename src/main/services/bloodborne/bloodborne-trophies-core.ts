export const trophyAchievementName = (displayName: string) =>
  displayName.toUpperCase().replace(/[^A-Z0-9]+/g, "_");
