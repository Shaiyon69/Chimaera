import { registerEvent } from "../register-event";
import { db } from "@main/level";
import { assertRendererAccess, getSublevelByName } from "./helpers";
import { logger } from "@main/services";

const leveldbDel = async (
  _event: Electron.IpcMainInvokeEvent,
  key: string,
  sublevelName?: string | null
) => {
  try {
    assertRendererAccess(true, sublevelName, key);
    if (sublevelName) {
      const sublevel = getSublevelByName(sublevelName);
      await sublevel.del(key);
    } else {
      await db.del(key);
    }
  } catch (error) {
    if (error instanceof Error && error.name === "NotFoundError") {
      // NotFoundError on delete is not an error, just return
      return;
    }
    logger.error("Error in leveldbDel", error);
    throw error;
  }
};

registerEvent("leveldbDel", leveldbDel);
