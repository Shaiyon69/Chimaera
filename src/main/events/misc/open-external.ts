import { shell } from "electron";
import { logger } from "@main/services";
import { isSafeExternalUrl } from "@main/helpers/ipc-guards-core";
import { registerEvent } from "../register-event";

const openExternal = async (
  _event: Electron.IpcMainInvokeEvent,
  src: string
) => {
  if (!isSafeExternalUrl(src)) {
    logger.warn("Blocked openExternal for unsafe URL", src);
    return;
  }

  return shell.openExternal(src);
};

registerEvent("openExternal", openExternal);
