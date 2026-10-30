// Stands in for the `electron` module when the preload runs inside the Tauri
// webview (vite.tauri.config.ts aliases it). The preload stays byte-for-byte the
// Electron one: every ipcRenderer.invoke goes to the Rust `ipc` dispatcher and
// every ipcRenderer.on becomes a Tauri event listener, so the renderer and Big
// Picture do not change while the main process is ported.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { logger } from "@renderer/logger";

type Listener = (event: unknown, ...args: unknown[]) => void;

// Main pushes with app.emit(channel, [arg1, arg2, ...]), the same argument list
// webContents.send(channel, ...args) delivered.
const unlisteners = new Map<Listener, Map<string, Promise<UnlistenFn>>>();

const on = (channel: string, listener: Listener) => {
  const pending = listen<unknown[]>(channel, (event) =>
    listener({ sender: null }, ...(event.payload ?? []))
  );
  const byChannel = unlisteners.get(listener) ?? new Map();
  byChannel.set(channel, pending);
  unlisteners.set(listener, byChannel);
  return ipcRenderer;
};

const removeListener = (channel: string, listener: Listener) => {
  const byChannel = unlisteners.get(listener);
  const pending = byChannel?.get(channel);
  if (!byChannel || !pending) return ipcRenderer;
  byChannel.delete(channel);
  if (byChannel.size === 0) unlisteners.delete(listener);
  void pending.then((unlisten) => unlisten());
  return ipcRenderer;
};

// One warning per channel the Rust side has not ported, so a blank window
// points straight at the call that broke it (open devtools to see it).
const reportedMissing = new Set<string>();

const invokeChannel = async (channel: string, args: unknown[]) => {
  try {
    return await invoke("ipc", { channel, args });
  } catch (error) {
    if (
      String(error).startsWith("not ported") &&
      !reportedMissing.has(channel)
    ) {
      reportedMissing.add(channel);
      logger.warn(`Tauri: IPC channel not ported yet: ${channel}`);
    }
    throw error;
  }
};

export const ipcRenderer = {
  invoke: (channel: string, ...args: unknown[]) => invokeChannel(channel, args),
  send: (channel: string, ...args: unknown[]) => {
    invokeChannel(channel, args).catch(() => undefined);
  },
  on,
  off: removeListener,
  removeListener,
};

export const contextBridge = {
  exposeInMainWorld: (key: string, api: unknown) => {
    (globalThis as Record<string, unknown>)[key] = api;
  },
};
