// Lists preload IPC channels the Tauri shell does not handle yet.
// Usage: node scripts/tauri-parity.mjs [--list] [--strict]
//   --list    print every missing channel
//   --strict  exit 1 while any channel is missing (cutover gate)
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

// Channels the preload invokes: literal ipcRenderer.invoke/send calls, plus the
// literal union passed through helpers such as invokeCloudSaveOperation.
export const preloadChannels = (source) => {
  const channels = new Set();
  for (const m of source.matchAll(
    /ipcRenderer\.(?:invoke|send)\(\s*"([^"]+)"/g
  ))
    channels.add(m[1]);
  for (const m of source.matchAll(/channel:\s*((?:\|?\s*"[^"]+"\s*)+),/g))
    for (const c of m[1].matchAll(/"([^"]+)"/g)) channels.add(c[1]);
  return channels;
};

// Handled channels: `"name" =>` match arms in the dispatcher, src-tauri/src/ipc.rs.
export const rustChannels = (sources) => {
  const channels = new Set();
  for (const source of sources)
    for (const m of source.matchAll(/^\s*"([A-Za-z0-9_-]+)"\s*=>/gm))
      channels.add(m[1]);
  return channels;
};

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const wanted = preloadChannels(
    readFileSync(join(root, "src/preload/index.ts"), "utf8")
  );
  const handled = rustChannels([
    readFileSync(join(root, "src-tauri/src/ipc.rs"), "utf8"),
  ]);
  const missing = [...wanted].filter((c) => !handled.has(c)).sort();
  const extra = [...handled].filter((c) => !wanted.has(c)).sort();

  console.log(
    `Tauri parity: ${wanted.size - missing.length}/${wanted.size} preload channels ported, ${missing.length} missing.`
  );
  if (extra.length)
    console.log(`Not called by the preload: ${extra.join(", ")}`);
  if (process.argv.includes("--list"))
    for (const c of missing) console.log(`  ${c}`);
  if (process.argv.includes("--strict") && missing.length) process.exit(1);
}
