import { shell } from "electron";
import fs from "node:fs";
import path from "node:path";
import { registerEvent } from "../register-event";

const openFolder = async (
  _event: Electron.IpcMainInvokeEvent,
  folderPath: string
) => {
  if (typeof folderPath !== "string" || !path.isAbsolute(folderPath)) {
    throw new Error("openFolder requires an absolute path");
  }

  // shell.openPath runs files, so refuse anything that exists and is not a directory.
  const stat = await fs.promises.stat(folderPath).catch(() => null);
  if (stat && !stat.isDirectory()) {
    throw new Error("openFolder target is not a directory");
  }

  await fs.promises.mkdir(folderPath, { recursive: true });

  return shell.openPath(folderPath);
};

registerEvent("openFolder", openFolder);
