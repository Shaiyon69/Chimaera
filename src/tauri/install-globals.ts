// The preload reads process.platform and process.env at load time. The Rust shell
// injects the real values as window.__CHIMAERA_PROCESS__ before any script runs.
const injected = (globalThis as { __CHIMAERA_PROCESS__?: unknown })
  .__CHIMAERA_PROCESS__ as
  | { platform: string; env: Record<string, string> }
  | undefined;

(globalThis as Record<string, unknown>).process ??= injected ?? {
  platform: "win32",
  env: {},
};
