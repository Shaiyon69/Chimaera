import { app } from "electron";
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { Readable, Transform } from "node:stream";
import { pipeline } from "node:stream/promises";
import type { ReadableStream as WebReadableStream } from "node:stream/web";
import { logger } from "../logger";
import { SevenZip } from "../7zip";
import { WindowManager } from "../window-manager";
import { isPortDir, pickPortRelease } from "./bloodborne-core";

// Installs Lantern Engine (by Awaken) from its latest stable release; the zip is verified
// against the sha256 digest GitHub publishes for the asset. Lantern's own launcher updates
// the package after that, keeping the player's files.
const LATEST_RELEASE =
  "https://api.github.com/repos/Awakenchan/LanternEngine/releases/latest";

/** The NSIS installer writes this when "Include Bloodborne" is ticked. */
const INSTALL_REQUESTED_MARKER = "bloodborne-install-requested";

export const downloadedPortDir = () =>
  path.join(app.getPath("userData"), "lantern-engine");

/** Lantern's launcher settings (game folder, saves folder, profile), shared by every package. */
export const lanternSettingsFile = () =>
  path.join(app.getPath("appData"), "Lantern Engine", "settings.json");

export const readLanternSettings = (): Record<string, unknown> => {
  try {
    const data = JSON.parse(fs.readFileSync(lanternSettingsFile(), "utf-8"));
    return data && typeof data === "object" ? data : {};
  } catch {
    return {};
  }
};

/** The player's files in a package, as Lantern's own updater keeps them (Updater.Kept). */
const USER_DATA = [
  "lantern.ini",
  "bbport.ini",
  "mods.json",
  "patches.json",
  "user",
  "mods",
  "logs",
  "network",
  "engine-assets",
];

/** Copies the player's files from a replaced package over the fresh one. */
const carryUserData = async (from: string, to: string) => {
  for (const name of USER_DATA) {
    const source = path.join(from, name);
    if (!fs.existsSync(source)) continue;
    await fs.promises.rm(path.join(to, name), { recursive: true, force: true });
    await fs.promises.cp(source, path.join(to, name), { recursive: true });
  }
};

/** Copies what the new package lacks yet; never overwrites the player's Lantern files. */
const copyMissing = async (from: string, to: string, names: string[]) => {
  for (const name of names) {
    const source = path.join(from, name);
    const target = path.join(to, name);
    if (!fs.existsSync(source) || fs.existsSync(target)) continue;
    await fs.promises.cp(source, target, { recursive: true });
  }
};

/** Takes saves, mods and the game folder over from the ports Chimaera used before Lantern. */
const migrateLegacyData = async (port: string) => {
  const root = app.getPath("userData");
  // The Mrsuss60 port, then Chimaera's own launcher before it.
  for (const [legacy, settingsName] of [
    [path.join(root, "bloodborne-port"), "bblauncher_settings.json"],
    [path.join(root, "bloodborne"), "launcher_settings.json"],
  ]) {
    if (!fs.existsSync(legacy)) continue;
    // Not bbport.ini: its old FSR and effect settings crashed the engine before.
    await copyMissing(legacy, port, ["user", "mods"]);
    try {
      const { eboot } = JSON.parse(
        fs.readFileSync(path.join(legacy, settingsName), "utf-8")
      );
      const settings = readLanternSettings();
      if (typeof eboot === "string" && !settings.game_dir) {
        settings.game_dir = eboot.replace(/[\\/]+eboot\.bin$/i, "");
        await fs.promises.mkdir(path.dirname(lanternSettingsFile()), {
          recursive: true,
        });
        await fs.promises.writeFile(
          lanternSettingsFile(),
          JSON.stringify(settings, null, 2)
        );
      }
    } catch {
      // no game folder to carry over
    }
    await fs.promises.rename(legacy, `${legacy}.migrated`);
  }
};

let installing: Promise<void> | null = null;

const sendProgress = (phase: "downloading" | "extracting", percent: number) =>
  WindowManager.sendToAppWindows("on-bloodborne-port-progress", {
    phase,
    percent,
  });

const fetchLatestRelease = async () => {
  const response = await fetch(LATEST_RELEASE, {
    headers: { Accept: "application/vnd.github+json" },
  });
  if (!response.ok) {
    throw new Error(
      `Lantern Engine release lookup failed: HTTP ${response.status}`
    );
  }
  const release = pickPortRelease(await response.json());
  if (!release)
    throw new Error("Latest Lantern Engine release has no verifiable zip");
  return release;
};

const install = async () => {
  const release = await fetchLatestRelease();
  const root = app.getPath("userData");
  const zipPath = path.join(root, "lantern-engine.zip.part");
  const extractDir = path.join(root, "lantern-engine.tmp");

  try {
    const response = await fetch(release.url);
    if (!response.ok || !response.body) {
      throw new Error(
        `Lantern Engine download failed: HTTP ${response.status}`
      );
    }

    const total = Number(response.headers.get("content-length")) || 0;
    const hash = createHash("sha256");
    let received = 0;
    let lastPercent = -1;

    await pipeline(
      Readable.fromWeb(response.body as WebReadableStream),
      new Transform({
        transform(chunk: Buffer, _encoding, callback) {
          hash.update(chunk);
          received += chunk.length;
          const percent = total ? Math.floor((received / total) * 100) : 0;
          if (percent !== lastPercent) {
            lastPercent = percent;
            sendProgress("downloading", percent);
          }
          callback(null, chunk);
        },
      }),
      fs.createWriteStream(zipPath)
    );

    if (hash.digest("hex") !== release.sha256) {
      throw new Error(
        "Lantern Engine download is corrupted (checksum mismatch)"
      );
    }

    sendProgress("extracting", 0);
    await fs.promises.rm(extractDir, { recursive: true, force: true });
    await SevenZip.extractFile(
      { filePath: zipPath, outputPath: extractDir },
      ({ percent }) => sendProgress("extracting", Math.floor(percent))
    );

    // The release zip wraps everything in one folder.
    const inner = fs
      .readdirSync(extractDir, { withFileTypes: true })
      .filter((e) => e.isDirectory());
    const portRoot =
      !isPortDir(extractDir) && inner.length === 1
        ? path.join(extractDir, inner[0].name)
        : extractDir;

    if (!isPortDir(portRoot)) {
      throw new Error("Archive is missing Lantern Engine.exe or its engine");
    }

    // Swap in last so a failed attempt never leaves a half-installed package. Windows refuses
    // the first rename while the game runs, which aborts and leaves the old one untouched.
    const dir = downloadedPortDir();
    const old = `${dir}.old`;
    await fs.promises.rm(old, { recursive: true, force: true });
    if (fs.existsSync(dir)) await fs.promises.rename(dir, old);
    try {
      await fs.promises.rename(portRoot, dir);
    } catch (error) {
      if (fs.existsSync(old)) await fs.promises.rename(old, dir);
      throw error;
    }
    if (fs.existsSync(old)) await carryUserData(old, dir);
    await migrateLegacyData(dir);
    await fs.promises.rm(old, { recursive: true, force: true });
  } finally {
    await fs.promises.rm(zipPath, { force: true });
    await fs.promises.rm(extractDir, { recursive: true, force: true });
  }
};

export const downloadBloodbornePort = () => {
  installing ??= install().finally(() => {
    installing = null;
  });
  return installing;
};

/** Installs when the installer box was ticked; Lantern's launcher keeps an installed package current. */
export const syncBloodbornePort = async () => {
  const marker = path.join(app.getPath("userData"), INSTALL_REQUESTED_MARKER);
  const requested = fs.existsSync(marker);
  fs.rmSync(marker, { force: true });
  if (!requested || isPortDir(downloadedPortDir())) return;

  try {
    await downloadBloodbornePort();
  } catch (error) {
    logger.error("Lantern Engine install failed", error);
  }
};
