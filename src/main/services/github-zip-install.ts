import { app } from "electron";
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { Readable, Transform } from "node:stream";
import { pipeline } from "node:stream/promises";
import type { ReadableStream as WebReadableStream } from "node:stream/web";
import { SevenZip } from "./7zip";

export interface ZipRelease {
  url: string;
  sha256: string;
}

export interface GithubZipInstallOptions {
  release: ZipRelease;
  /** Names the error messages and the temp files in userData. */
  label: string;
  tempName: string;
  targetDir: string;
  isValidDir: (dir: string) => boolean;
  missingMessage: string;
  onProgress: (phase: "downloading" | "extracting", percent: number) => void;
  /** Runs after the swap with the replaced folder, if there was one. */
  afterSwap?: (dir: string, old: string | null) => Promise<void>;
}

/** Downloads a GitHub release zip, checks its sha256, extracts it and swaps it into place. */
export const installGithubZip = async ({
  release,
  label,
  tempName,
  targetDir,
  isValidDir,
  missingMessage,
  onProgress,
  afterSwap,
}: GithubZipInstallOptions) => {
  const root = app.getPath("userData");
  const zipPath = path.join(root, `${tempName}.zip.part`);
  const extractDir = path.join(root, `${tempName}.tmp`);

  try {
    const response = await fetch(release.url);
    if (!response.ok || !response.body) {
      throw new Error(`${label} download failed: HTTP ${response.status}`);
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
            onProgress("downloading", percent);
          }
          callback(null, chunk);
        },
      }),
      fs.createWriteStream(zipPath)
    );

    if (hash.digest("hex") !== release.sha256) {
      throw new Error(`${label} download is corrupted (checksum mismatch)`);
    }

    onProgress("extracting", 0);
    await fs.promises.rm(extractDir, { recursive: true, force: true });
    await SevenZip.extractFile(
      { filePath: zipPath, outputPath: extractDir },
      ({ percent }) => onProgress("extracting", Math.floor(percent))
    );

    // Some release zips wrap everything in one folder.
    const inner = fs
      .readdirSync(extractDir, { withFileTypes: true })
      .filter((e) => e.isDirectory());
    const packageRoot =
      !isValidDir(extractDir) && inner.length === 1
        ? path.join(extractDir, inner[0].name)
        : extractDir;

    if (!isValidDir(packageRoot)) throw new Error(missingMessage);

    // Swap in last so a failed attempt never leaves a half-installed package. Windows refuses
    // the first rename while the game runs, which aborts and leaves the old one untouched.
    const old = `${targetDir}.old`;
    await fs.promises.rm(old, { recursive: true, force: true });
    if (fs.existsSync(targetDir)) await fs.promises.rename(targetDir, old);
    try {
      await fs.promises.rename(packageRoot, targetDir);
    } catch (error) {
      if (fs.existsSync(old)) await fs.promises.rename(old, targetDir);
      throw error;
    }
    await afterSwap?.(targetDir, fs.existsSync(old) ? old : null);
    await fs.promises.rm(old, { recursive: true, force: true });
  } finally {
    await fs.promises.rm(zipPath, { force: true });
    await fs.promises.rm(extractDir, { recursive: true, force: true });
  }
};
