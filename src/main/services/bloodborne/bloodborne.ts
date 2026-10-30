import { app } from "electron";
import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import type {
  Game,
  GameShop,
  ShopAssets,
  ShopDetails,
  SteamAchievement,
} from "@types";
import { gamesSublevel, levelKeys } from "@main/level";
import { logger } from "../logger";
import {
  downloadedPortDir,
  readLanternSettings,
} from "./bloodborne-port-installer";
import { WindowManager } from "../window-manager";
import {
  LAUNCHER_EXE,
  engineDirOf,
  gameDir,
  isPortDir,
  parseTrophyLog,
  savesDir,
} from "./bloodborne-core";
import { trophyAchievementName } from "./bloodborne-trophies-core";
import { mergeAchievements } from "../achievements/merge-achievements";

/** Lantern Engine's launcher: it holds the game folder, graphics, keybinds and mods. */
const launcherExe = () => path.join(downloadedPortDir(), LAUNCHER_EXE);

/** Starts the game with the launcher's saved settings and waits for it to end. */
const playExe = () => path.join(downloadedPortDir(), "Bloodborne.exe");

export const getBloodborneSetup = () => ({
  portInstalled: isPortDir(downloadedPortDir()),
});

const fail = (problems: string[]) => {
  logger.error("Bloodborne launch failed", { problems });
  WindowManager.sendToAppWindows("on-bloodborne-launch-error", problems);
  return null;
};

/** Art the user drops in resources/bloodborne; missing files become null. */
const artDir = () =>
  app.isPackaged
    ? path.join(process.resourcesPath, "bloodborne")
    : path.join(__dirname, "..", "..", "resources", "bloodborne");

const art = (name: string) => {
  const file = path.join(artDir(), name);
  return fs.existsSync(file) ? `local:${file}` : null;
};

export const BLOODBORNE_OBJECT_ID = "bloodborne";

export const bloodborneShopAssets = (): ShopAssets => ({
  objectId: BLOODBORNE_OBJECT_ID,
  shop: "custom",
  title: "Bloodborne",
  iconUrl: art("icon.png"),
  libraryHeroImageUrl: art("hero.jpg"),
  libraryImageUrl: art("header.png"),
  logoImageUrl: art("logo.png"),
  logoPosition: null,
  coverImageUrl: art("cover.png"),
  downloadSources: [],
});

const SCREENSHOT_FILES = [
  "001",
  "008",
  "015",
  "046",
  "054",
  "062",
  "070",
  "078",
  "086",
].map((n) => `screenshot-${n}.jpg`);

// Figures from earlier bbport-based builds and player reports
// (RX 6650 XT + Ryzen 5 5600X: 89-100 FPS at 1080p).
const REQUIREMENTS = {
  minimum:
    '<ul class="bb_ul"><li><strong>OS:</strong> Windows 10 (1903+) 64-bit</li>' +
    "<li><strong>Processor:</strong> Intel Core 4th gen (Haswell) or AMD Ryzen, with AVX2</li>" +
    "<li><strong>Memory:</strong> 6 GB free (RAM + page file)</li>" +
    "<li><strong>Graphics:</strong> Vulkan 1.3 GPU: GeForce GTX 1060, Radeon RX 480 or Intel Arc A380</li>" +
    "<li><strong>Additional Notes:</strong> Your own decrypted CUSA03173 v1.09 dump (eboot.bin). Target: 30-60 FPS at 720p-1080p with an upscaler.</li></ul>",
  recommended:
    '<ul class="bb_ul"><li><strong>OS:</strong> Windows 11 64-bit</li>' +
    "<li><strong>Processor:</strong> AMD Ryzen 5 5600X or better</li>" +
    "<li><strong>Memory:</strong> 10 GB free (RAM + page file) for 1440p/4K output</li>" +
    "<li><strong>Graphics:</strong> Radeon RX 6650 XT / GeForce RTX 3060 or better (RTX 20-series+ for DLSS)</li>" +
    "<li><strong>Additional Notes:</strong> Current vendor drivers. Reported: 89-100 FPS at 1080p on an RX 6650 XT; about 90 FPS at 4K with FSR 4 Balanced on an RX 7800 XT.</li></ul>",
};

export const bloodborneShopDetails = (): ShopDetails => {
  const about =
    "Hunt your nightmares in the gothic city of Yharnam. Runs on Lantern Engine by Awaken: bring your own CUSA03173 dump.";
  const description =
    "<p>Face your fears as you search for answers in the ancient city of Yharnam, now cursed with a strange endemic illness spreading through the streets like wildfire. Danger, death and madness lurk around every corner of this dark and horrifying world, and you must discover its darkest secrets in order to survive.</p>" +
    "<ul><li>Fast, aggressive combat built around trick weapons and the regain system.</li>" +
    "<li>Interconnected gothic world, optional chalice dungeons and three endings.</li>" +
    "<li>The Old Hunters expansion adds the Hunter's Nightmare and new bosses.</li></ul>" +
    '<p><strong>PC notes:</strong> runs on <a href="https://github.com/Awakenchan/LanternEngine">Lantern Engine by Awaken</a>, where the game\'s own code runs natively and its graphics are translated to Vulkan. It needs your own decrypted CUSA03173 v1.09 dump and supports FSR, DLSS and TAA upscaling plus FPS and resolution patches.</p>';
  return {
    objectId: BLOODBORNE_OBJECT_ID,
    name: "Bloodborne",
    steam_appid: 0,
    detailed_description: description,
    about_the_game: description,
    short_description: about,
    screenshots: SCREENSHOT_FILES.flatMap((name, id) => {
      const url = art(name);
      return url ? [{ id, path_thumbnail: url, path_full: url }] : [];
    }),
    developers: ["FromSoftware"],
    publishers: ["Sony Interactive Entertainment"],
    genres: [
      { id: "1", name: "Action" },
      { id: "3", name: "RPG" },
    ],
    supported_languages: "English",
    pc_requirements: REQUIREMENTS,
    mac_requirements: { minimum: "", recommended: "" },
    linux_requirements: { minimum: "", recommended: "" },
    release_date: { coming_soon: false, date: "24 Mar, 2015" },
    content_descriptors: { ids: [] },
  } as ShopDetails;
};

// [displayName, description, hidden], indexed by PS4 trophy id (Lantern's Trophies.cs order).
const TROPHIES: [string, string, boolean][] = [
  ["Bloodborne", "All trophies acquired. Hats off!", false],
  ["Yharnam Sunrise", "You lived through the hunt, and saw another day.", true],
  [
    "Honouring Wishes",
    "Captivated by the moon presence, you pledge to watch over the hunter's dream.",
    true,
  ],
  [
    "Childhood's Beginning",
    "You became an infant Great One, lifting humanity into its next childhood.",
    true,
  ],
  [
    "Yharnam, Pthumerian Queen",
    "Defeat Yharnam, Blood Queen of the Old Labyrinth.",
    false,
  ],
  ["Hunter's Essence", "Acquire all hunter weapons.", false],
  ["Hunter's Craft", "Acquire all special hunter tools.", false],
  ["Weapon Master", "Acquire a weapon of the highest level.", false],
  ["Blood Gem Master", "Acquire an extremely precious blood gem.", false],
  ["Rune Master", "Acquire an extremely precious Caryll Rune.", false],
  ["Cainhurst", "Gain entry to Cainhurst, the lost and ruined castle.", false],
  [
    "The Choir",
    "Gain entry to the realm of the Choir, the high stratum of the Healing Church.",
    false,
  ],
  [
    "The Source of the Dream",
    "Discover the abandoned old workshop, the source of the hunter's dream.",
    false,
  ],
  [
    "Nightmare Lecture Building",
    "Gain entry into the Byrgenwerth lecture building, that drifts within the realm of nightmare.",
    false,
  ],
  ["Father Gascoigne", "Defeat Father Gascoigne.", false],
  ["Vicar Amelia", "Defeat Vicar Amelia.", false],
  ["Shadow of Yharnam", "Defeat the Shadows of Yharnam.", false],
  ["Rom, the Vacuous Spider", "Defeat Rom, the Vacuous Spider.", false],
  ["The One Reborn", "Defeat The One Reborn.", false],
  [
    "Micolash, Host of the Nightmare",
    "Defeat Micolash, Host of the Nightmare.",
    false,
  ],
  ["Mergo's Wet Nurse", "Defeat Mergo's Wet Nurse.", false],
  ["Cleric Beast", "Defeat the Cleric Beast.", false],
  ["Blood-starved Beast", "Defeat the Blood-starved Beast.", false],
  ["The Witch of Hemwick", "Defeat the Witch of Hemwick.", false],
  ["Darkbeast Paarl", "Defeat Darkbeast Paarl.", false],
  ["Amygdala", "Defeat Great One: Amygdala.", false],
  ["Martyr Logarius", "Defeat Martyr Logarius.", false],
  ["Celestial Emissary", "Defeat the Celestial Emissary.", false],
  [
    "Ebrietas, Daughter of the Cosmos",
    "Defeat Ebrietas, Daughter of the Cosmos.",
    false,
  ],
  [
    "Blood Gem Contact",
    "Acquire a blood gem that imbues hunter weapons with special strength.",
    false,
  ],
  [
    "Rune Contact",
    "Acquire a Caryll Rune that endows hunters with special strength.",
    false,
  ],
  [
    "Chalice of Pthumeru",
    "Acquire the Chalice of Pthumeru that seals the catacombs that form a web deep below Yharnam.",
    false,
  ],
  [
    "Chalice of Ailing Loran",
    "Acquire the Chalice of Ailing Loran that seals the tragic land lost to the sands.",
    false,
  ],
  [
    "Chalice of Isz",
    "Acquire the Great Chalice of Isz that seals the home of the cosmic kin.",
    false,
  ],
  ["Old Hunter's Essence", "Acquire all old hunter weapons.", false],
  ["Orphan of Kos", "Defeat Great One: Orphan of Kos.", false],
  [
    "Ludwig, the Holy Blade",
    "Defeat the beast that was once Ludwig, the Holy Blade.",
    false,
  ],
  [
    "Lady Maria of the Astral Clocktower",
    "Defeat Lady Maria of the Astral Clocktower.",
    false,
  ],
  [
    "Living Failures",
    "Defeat the failed attempts to become Great Ones.",
    false,
  ],
  [
    "Laurence, the First Vicar",
    "Defeat the beast that was once Laurence, the First Vicar.",
    false,
  ],
];

export const bloodborneAchievements = (): SteamAchievement[] => {
  const icon = art("icon.png") ?? art("cover.png") ?? "";
  return TROPHIES.map(([displayName, description, hidden]) => ({
    name: trophyAchievementName(displayName),
    displayName,
    description,
    icon,
    icongray: icon,
    hidden,
  }));
};

export const openBloodborneLauncher = () => {
  const proc = spawn(launcherExe(), [], {
    cwd: downloadedPortDir(),
    detached: true,
    stdio: "ignore",
  });
  proc.once("error", (error) => {
    fail([`could not start the Bloodborne launcher: ${error.message}`]);
  });
  proc.unref();
};

/** Merges trophies.log into the game's achievements; the engine appends to it as trophies unlock. */
const syncTrophies = async (record: Game, logPath: string, notify = true) => {
  let text: string;
  try {
    text = await fs.promises.readFile(logPath, "utf-8");
  } catch {
    return; // no trophies yet
  }
  const unlocked = parseTrophyLog(text).flatMap(({ id, unlockTime }) => {
    const trophy = TROPHIES[id];
    return trophy
      ? [{ name: trophyAchievementName(trophy[0]), unlockTime }]
      : [];
  });
  if (unlocked.length === 0) return;
  await mergeAchievements(record, unlocked, notify).catch((err) =>
    logger.error("Bloodborne trophy merge failed", err)
  );
};

export const launchBloodborne = async (shop: GameShop, objectId: string) => {
  // The setup modal offers the download when Lantern Engine is missing.
  if (!getBloodborneSetup().portInstalled) {
    WindowManager.sendToAppWindows(
      "on-bloodborne-setup-required",
      shop,
      objectId
    );
    return null;
  }

  const portDir = downloadedPortDir();
  const key = levelKeys.game(shop, objectId);
  const record = await gamesSublevel.get(key).catch(() => null);
  if (record) {
    // Lets the process watcher track playtime from the engine itself, not the launcher.
    await gamesSublevel.put(key, {
      ...record,
      executablePath: playExe(),
      executablePathUpdatedAt: new Date(),
      trackingExecutablePaths: [
        path.join(engineDirOf(portDir), "out", "bb-probe.exe"),
      ],
      trackingExecutablePathsUpdatedAt: new Date(),
    });
  }

  // Without a game folder the launcher's setup guide asks for one.
  const settings = readLanternSettings();
  if (!gameDir(settings) || !record) {
    openBloodborneLauncher();
    return null;
  }

  const proc = spawn(playExe(), [], {
    cwd: portDir,
    detached: true,
    stdio: "ignore",
  });
  proc.once("error", (error) => {
    fail([`could not start Bloodborne: ${error.message}`]);
  });

  // Bloodborne.exe waits for the game, so the log is watched until it exits.
  const logPath = path.join(savesDir(portDir, settings), "trophies.log");
  // Unlocks from games started in Lantern's own launcher come in without a notification.
  await syncTrophies(record, logPath, false);
  const onChange = () => void syncTrophies(record, logPath);
  fs.watchFile(logPath, { interval: 2000 }, onChange);
  proc.once("exit", () => {
    fs.unwatchFile(logPath, onChange);
    onChange();
  });
  proc.unref();

  return null;
};
