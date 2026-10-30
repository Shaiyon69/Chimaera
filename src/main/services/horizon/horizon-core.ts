import fs from "node:fs";
import path from "node:path";

export const LAUNCHER_EXE = "horizon-launcher.exe";

const WINDOWS_ZIP = "horizon-launcher-windows-x64.zip";

export const isLauncherDir = (dir: string): boolean =>
  fs.existsSync(path.join(dir, LAUNCHER_EXE));

export interface LauncherRelease {
  tag: string;
  url: string;
  sha256: string;
}

interface ReleasePayload {
  tag_name?: unknown;
  prerelease?: unknown;
  draft?: unknown;
  assets?: {
    name?: unknown;
    browser_download_url?: unknown;
    digest?: unknown;
  }[];
}

/**
 * The newest stable launcher release from a GitHub releases list. The repo's
 * releases/latest is the game build, so the launcher@ tags are picked out here.
 */
export const pickLauncherRelease = (
  releases: unknown
): LauncherRelease | null => {
  if (!Array.isArray(releases)) return null;
  for (const r of releases as ReleasePayload[]) {
    if (!r || typeof r !== "object" || r.prerelease || r.draft) continue;
    if (typeof r.tag_name !== "string" || !r.tag_name.startsWith("launcher@"))
      continue;
    const asset = Array.isArray(r.assets)
      ? r.assets.find((a) => a?.name === WINDOWS_ZIP)
      : undefined;
    const sha256 =
      typeof asset?.digest === "string"
        ? /^sha256:([0-9a-f]{64})$/i.exec(asset.digest)?.[1]
        : undefined;
    if (typeof asset?.browser_download_url !== "string" || !sha256) continue;
    return {
      tag: r.tag_name,
      url: asset.browser_download_url,
      sha256: sha256.toLowerCase(),
    };
  }
  return null;
};
