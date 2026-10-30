import fs from "node:fs";
import path from "node:path";

export const LAUNCHER_EXE = "Lantern Engine.exe";

/** The package's engine folder: engine\ since 0.9.37, the package itself before. */
export const engineDirOf = (dir: string): string =>
  fs.existsSync(path.join(dir, "engine", "out", "bb-probe.exe"))
    ? path.join(dir, "engine")
    : dir;

/** A Lantern Engine package: its launcher and the engine's bb-probe.exe. */
export const isPortDir = (dir: string): boolean =>
  fs.existsSync(path.join(dir, LAUNCHER_EXE)) &&
  fs.existsSync(path.join(engineDirOf(dir), "out", "bb-probe.exe"));

export interface PortRelease {
  tag: string;
  url: string;
  sha256: string;
}

/** The Windows package zip and its GitHub-provided sha256 from a releases/latest payload; null if either is missing. */
export const pickPortRelease = (release: unknown): PortRelease | null => {
  const r = (release && typeof release === "object" ? release : {}) as {
    tag_name?: unknown;
    assets?: {
      name?: unknown;
      browser_download_url?: unknown;
      digest?: unknown;
    }[];
  };
  const asset = Array.isArray(r.assets)
    ? r.assets.find(
        (a) =>
          typeof a.name === "string" &&
          /^LanternEngine-.+-win64\.zip$/i.test(a.name)
      )
    : undefined;
  const sha256 =
    typeof asset?.digest === "string"
      ? /^sha256:([0-9a-f]{64})$/i.exec(asset.digest)?.[1]
      : undefined;
  if (
    typeof r.tag_name !== "string" ||
    typeof asset?.browser_download_url !== "string" ||
    !sha256
  )
    return null;
  return {
    tag: r.tag_name,
    url: asset.browser_download_url,
    sha256: sha256.toLowerCase(),
  };
};

export interface LanternSettings {
  game_dir?: unknown;
  user_dir?: unknown;
  player_profile?: unknown;
}

const expand = (value: string) =>
  value.replace(
    /%([^%]+)%/g,
    (whole, name: string) => process.env[name] ?? whole
  );

/** Where the engine keeps saves and trophies.log, as the launcher's GameEnvironment.UserDir resolves it. */
export const savesDir = (portDir: string, settings: LanternSettings) => {
  const root =
    typeof settings.user_dir === "string" && settings.user_dir
      ? expand(settings.user_dir)
      : path.join(portDir, "user");
  const profile =
    typeof settings.player_profile === "string"
      ? settings.player_profile.replace(/[^\p{L}\p{N} _-]/gu, "").trim()
      : "";
  return profile && profile.toLowerCase() !== "main"
    ? path.join(root, "profiles", profile)
    : root;
};

export const gameDir = (settings: LanternSettings) =>
  typeof settings.game_dir === "string" ? expand(settings.game_dir) : "";

/** trophies.log lines are "<trophy id> <unix seconds>". */
export const parseTrophyLog = (
  text: string
): { id: number; unlockTime: number }[] =>
  text.split(/\r?\n/).flatMap((line) => {
    const match = /^\s*(\d+)\s+(\d+)\s*$/.exec(line);
    return match
      ? [{ id: Number(match[1]), unlockTime: Number(match[2]) * 1000 }]
      : [];
  });
