import fs from "node:fs";
import { app } from "electron";
import { logger } from "@main/services";
import { isOwnTempFile } from "@main/helpers/ipc-guards-core";
import { registerEvent } from "../register-event";

const deleteTempFile = async (
  _event: Electron.IpcMainInvokeEvent,
  filePath: string
): Promise<void> => {
  // The renderer must not be able to delete arbitrary files.
  if (!isOwnTempFile(filePath, app.getPath("temp"))) {
    logger.warn("Blocked deleteTempFile outside temp files", filePath);
    return;
  }

  try {
    if (fs.existsSync(filePath)) {
      fs.unlinkSync(filePath);
    }
  } catch (error) {
    // Silently fail - temp files will be cleaned up by OS eventually
    logger.warn(`Failed to delete temp file: ${error}`);
  }
};

registerEvent("deleteTempFile", deleteTempFile);
