import { registerEvent } from "../register-event";
import { bundledGameService } from "@main/services/bundled-games";

const service = (objectId: string) => {
  const found = bundledGameService(objectId);
  if (!found) throw new Error(`Unknown bundled game: ${objectId}`);
  return found;
};

registerEvent("getBundledGameSetup", async (_event, objectId: string) => ({
  installed: service(objectId).installed(),
}));
registerEvent("openBundledGameLauncher", async (_event, objectId: string) =>
  service(objectId).openLauncher()
);
registerEvent("downloadBundledGame", async (_event, objectId: string) =>
  service(objectId).download()
);
