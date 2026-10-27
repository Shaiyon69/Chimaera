export const resolveSteamSessionPlaytimePolicy = ({
  hasActiveSteamImport,
  isSteamLibraryPath,
  enableHydraPlaytimeTracking,
}: {
  hasActiveSteamImport: boolean;
  isSteamLibraryPath: boolean;
  enableHydraPlaytimeTracking: boolean;
}) => ({
  countHydraPlaytime:
    !hasActiveSteamImport || !isSteamLibraryPath || enableHydraPlaytimeTracking,
  syncSteamOnExit: hasActiveSteamImport && isSteamLibraryPath,
});
