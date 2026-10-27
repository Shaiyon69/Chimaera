import { registerEvent } from "../register-event";
import { assertRendererAccess, getSublevelByName } from "./helpers";
import { logger } from "@main/services";

const leveldbValues = async (
  _event: Electron.IpcMainInvokeEvent,
  sublevelName: string
) => {
  try {
    assertRendererAccess(false, sublevelName);
    const sublevel = getSublevelByName(sublevelName);
    return sublevel.values().all();
  } catch (error) {
    logger.error("Error in leveldbValues", error);
    throw error;
  }
};

registerEvent("leveldbValues", leveldbValues);
