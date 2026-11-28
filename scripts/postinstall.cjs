const util = require("node:util");
const fs = require("node:fs");
const path = require("node:path");
const { Readable } = require("node:stream");
const { pipeline } = require("node:stream/promises");

const exec = util.promisify(require("node:child_process").exec);

const ludusaviVersion = "0.29.0";

const fileName = {
  win32: `ludusavi-v${ludusaviVersion}-win64.zip`,
  linux: `ludusavi-v${ludusaviVersion}-linux.tar.gz`,
  darwin: `ludusavi-v${ludusaviVersion}-mac.tar.gz`,
};

const ludusaviBinaryName = {
  win32: "ludusavi.exe",
  linux: "ludusavi",
  darwin: "ludusavi",
};

const downloadLudusavi = async () => {
  if (
    fs.existsSync(
      path.join(process.cwd(), "ludusavi", ludusaviBinaryName[process.platform])
    )
  ) {
    console.log("Ludusavi already exists, skipping download...");
    return;
  }

  const file = fileName[process.platform];
  const downloadUrl = `https://github.com/mtkennerly/ludusavi/releases/download/v${ludusaviVersion}/${file}`;

  console.log(`Downloading ${file}...`);

  const response = await fetch(downloadUrl);
  if (!response.ok)
    throw new Error(`Ludusavi download failed: HTTP ${response.status}`);

  const targetPath = path.join(process.cwd(), "ludusavi");
  const binary = path.join(targetPath, ludusaviBinaryName[process.platform]);

  try {
    await pipeline(Readable.fromWeb(response.body), fs.createWriteStream(file));

    console.log(`Downloaded ${file}, extracting...`);

    await fs.promises.mkdir(targetPath, { recursive: true });

    // extract-zip truncated ludusavi.exe (38 KB short, unrunnable); Windows' bsdtar reads zips intact.
    // Full path: Git Bash puts GNU tar (no zip support) first on PATH.
    const tar =
      process.platform === "win32"
        ? `"${path.join(process.env.SystemRoot, "System32", "tar.exe")}"`
        : "tar";
    await exec(`${tar} -xf "${file}" -C "${targetPath}"`);

    if (process.platform !== "win32") {
      fs.chmodSync(binary, 0o755);
    }

    console.log(`Extracted ${file}, removing compressed downloaded file...`);
  } catch (error) {
    // A half-extracted binary would pass the "already exists" check forever.
    fs.rmSync(binary, { force: true });
    throw error;
  } finally {
    // A failed or partial download must not linger as a broken archive.
    fs.rmSync(file, { force: true });
  }
};

downloadLudusavi().catch((error) => {
  console.error(`Ludusavi setup failed: ${error.message}`);
  process.exitCode = 1;
});
