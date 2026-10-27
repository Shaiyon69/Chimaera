import { registerEvent } from "../register-event";
import { assertRendererAccess, getSublevelByName } from "./helpers";
import { logger } from "@main/services";

const leveldbClear = async (
  _event: Electron.IpcMainInvokeEvent,
  sublevelName: string
) => {
  try {
    assertRendererAccess(true, sublevelName);
    const sublevel = getSublevelByName(sublevelName);
    await sublevel.clear();
  } catch (error) {
    logger.error("Error in leveldbClear", error);
    throw error;
  }
};

registerEvent("leveldbClear", leveldbClear);
