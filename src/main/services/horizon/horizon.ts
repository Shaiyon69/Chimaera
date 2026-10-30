import { app } from "electron";
import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import type { GameShop, ShopAssets, ShopDetails } from "@types";
import { gamesSublevel, levelKeys } from "@main/level";
import { logger } from "../logger";
import { WindowManager } from "../window-manager";
import { installGithubZip } from "../github-zip-install";
import {
  LAUNCHER_EXE,
  isLauncherDir,
  pickLauncherRelease,
} from "./horizon-core";

// Horizon (horizon-rust/horizon) is Minecraft: Java 26.3 rewritten in Rust. Chimaera installs
// its launcher once; the launcher then updates itself, installs the game and signs in.
const RELEASES =
  "https://api.github.com/repos/horizon-rust/horizon/releases?per_page=30";

export const HORIZON_OBJECT_ID = "horizon";

const launcherDir = () =>
  path.join(app.getPath("userData"), "horizon-launcher");
const launcherExe = () => path.join(launcherDir(), LAUNCHER_EXE);

export const getHorizonSetup = () => ({
  installed: isLauncherDir(launcherDir()),
});

const fail = (problems: string[]) => {
  logger.error("Horizon launch failed", { problems });
  WindowManager.sendToAppWindows(
    "on-bundled-game-launch-error",
    HORIZON_OBJECT_ID,
    problems
  );
};

const fetchRelease = async () => {
  const response = await fetch(RELEASES, {
    headers: { Accept: "application/vnd.github+json" },
  });
  if (!response.ok) {
    throw new Error(`Horizon release lookup failed: HTTP ${response.status}`);
  }
  const release = pickLauncherRelease(await response.json());
  if (!release)
    throw new Error("No Horizon launcher release has a verifiable zip");
  return release;
};

let installing: Promise<void> | null = null;

export const downloadHorizon = () => {
  installing ??= (async () =>
    installGithubZip({
      release: await fetchRelease(),
      label: "Horizon launcher",
      tempName: "horizon-launcher",
      targetDir: launcherDir(),
      isValidDir: isLauncherDir,
      missingMessage: `Archive is missing ${LAUNCHER_EXE}`,
      onProgress: (phase, percent) =>
        WindowManager.sendToAppWindows("on-bundled-game-progress", {
          id: HORIZON_OBJECT_ID,
          phase,
          percent,
        }),
    }))().finally(() => {
    installing = null;
  });
  return installing;
};

export const openHorizonLauncher = () => {
  const proc = spawn(launcherExe(), [], {
    cwd: launcherDir(),
    detached: true,
    stdio: "ignore",
  });
  proc.once("error", (error) =>
    fail([`could not start the Horizon launcher: ${error.message}`])
  );
  proc.unref();
};

export const launchHorizon = async (shop: GameShop, objectId: string) => {
  if (!getHorizonSetup().installed) {
    WindowManager.sendToAppWindows(
      "on-bundled-game-setup-required",
      HORIZON_OBJECT_ID
    );
    return null;
  }

  const key = levelKeys.game(shop, objectId);
  const record = await gamesSublevel.get(key).catch(() => null);
  // shortcut: playtime counts while the launcher is open; the launcher keeps horizon-client.exe
  // in a folder of its own choosing, so track that exe once its path is known.
  if (record && record.executablePath !== launcherExe()) {
    await gamesSublevel.put(key, {
      ...record,
      executablePath: launcherExe(),
      executablePathUpdatedAt: new Date(),
    });
  }

  openHorizonLauncher();
  return null;
};

/** Art the user drops in resources/horizon; missing files become null. */
const artDir = () =>
  app.isPackaged
    ? path.join(process.resourcesPath, "horizon")
    : path.join(__dirname, "..", "..", "resources", "horizon");

const art = (name: string) => {
  const file = path.join(artDir(), name);
  return fs.existsSync(file) ? `local:${file}` : null;
};

const TITLE = "Minecraft (Horizon)";

// In-game shots from playhorizon.dev: village, survival, night, nether, underwater, launcher, settings.
const SCREENSHOT_FILES = Array.from(
  { length: 7 },
  (_, i) => `screenshot-0${i + 1}.jpg`
);

export const horizonShopAssets = (): ShopAssets => ({
  objectId: HORIZON_OBJECT_ID,
  shop: "custom",
  title: TITLE,
  iconUrl: art("icon.png"),
  libraryHeroImageUrl: art("hero.jpg"),
  libraryImageUrl: art("header.png"),
  logoImageUrl: art("logo.png"),
  logoPosition: null,
  coverImageUrl: art("cover.png"),
  downloadSources: [],
});

const REQUIREMENTS = {
  minimum:
    '<ul class="bb_ul"><li><strong>OS:</strong> Windows 10 64-bit</li>' +
    "<li><strong>Additional Notes:</strong> A Microsoft account that owns Minecraft: Java Edition. The launcher downloads the 26.3 game files from Mojang for that account.</li></ul>",
  recommended: "",
};

export const horizonShopDetails = (): ShopDetails => {
  const about =
    "Minecraft: Java Edition 26.3, rewritten from scratch in Rust. Bring your own Java Edition account.";
  const description =
    "<p>Horizon is a from-scratch Rust rewrite of Minecraft: Java Edition 26.3, checked against vanilla behaviour. Play singleplayer worlds, join servers, or host one from the launcher.</p>" +
    '<p><strong>PC notes:</strong> runs through the <a href="https://playhorizon.dev">Horizon launcher</a>, which updates itself and installs the game. It needs a Microsoft account that owns Minecraft: Java Edition; Horizon ships no Minecraft code or assets.</p>' +
    "<p>Not an official Minecraft product. Not approved by or associated with Mojang or Microsoft.</p>";
  return {
    objectId: HORIZON_OBJECT_ID,
    name: TITLE,
    steam_appid: 0,
    detailed_description: description,
    about_the_game: description,
    short_description: about,
    screenshots: SCREENSHOT_FILES.flatMap((name, id) => {
      const url = art(name);
      return url ? [{ id, path_thumbnail: url, path_full: url }] : [];
    }),
    developers: ["Horizon contributors"],
    publishers: ["Horizon contributors"],
    genres: [
      { id: "1", name: "Sandbox" },
      { id: "2", name: "Survival" },
    ],
    supported_languages: "English",
    pc_requirements: REQUIREMENTS,
    mac_requirements: { minimum: "", recommended: "" },
    linux_requirements: { minimum: "", recommended: "" },
    release_date: { coming_soon: false, date: "" },
    content_descriptors: { ids: [] },
  } as ShopDetails;
};
