/** Opens the setup modal (Install button, or a Play before setup); detail is the objectId. */
export const BUNDLED_GAME_SETUP_EVENT = "chimaera:bundled-game-setup";
/** Fired after setup is saved so buttons can switch from Install to Play. */
export const BUNDLED_GAME_CONFIGURED_EVENT = "chimaera:bundled-game-configured";

export const openBundledGameSetup = (objectId: string) =>
  window.dispatchEvent(
    new CustomEvent<string>(BUNDLED_GAME_SETUP_EVENT, { detail: objectId })
  );
