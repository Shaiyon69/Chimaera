// The few Electron types the preload and renderer still name. `electron` itself
// resolves to src/tauri/electron-shim.ts, so the 300 MB package is not needed.
declare namespace Electron {
  // Listener cleanups return it straight out of useEffect, so it must read as void.
  type IpcRenderer = void;
  type IpcRendererEvent = unknown;

  interface FileFilter {
    name: string;
    extensions: readonly string[];
  }

  interface OpenDialogOptions {
    title?: string;
    defaultPath?: string;
    buttonLabel?: string;
    filters?: FileFilter[];
    properties?: readonly (
      | "openFile"
      | "openDirectory"
      | "multiSelections"
      | "showHiddenFiles"
      | "createDirectory"
      | "promptToCreate"
      | "noResolveAliases"
      | "treatPackageAsDirectory"
      | "dontAddToRecent"
    )[];
  }

  interface OpenDialogReturnValue {
    canceled: boolean;
    filePaths: string[];
  }
}
