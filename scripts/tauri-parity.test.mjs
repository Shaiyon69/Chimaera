import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { preloadChannels, rustChannels } from "./tauri-parity.mjs";

describe("tauri parity", () => {
  it("collects literal invokes and helper channel unions", () => {
    const preload = `
      ping: () => ipcRenderer.invoke("ping"),
      report: () => ipcRenderer.send("updateNetworkStatus", 1),
      ipcRenderer.on("on-download-progress", listener);
      const op = async (
        channel:
          | "syncGameCloudSave"
          | "resolveCloudSaveConflict",
        args: unknown[],
      ) => {};`;
    assert.deepEqual([...preloadChannels(preload)].sort(), [
      "ping",
      "resolveCloudSaveConflict",
      "syncGameCloudSave",
      "updateNetworkStatus",
    ]);
  });

  it("reads match arms, not other strings", () => {
    const rust = `match channel {\n  "ping" => Ok(json!("pong")),\n  "getVersion" => x,\n  _ => Err(format!("not ported")),\n}`;
    assert.deepEqual([...rustChannels([rust])].sort(), ["getVersion", "ping"]);
  });
});
