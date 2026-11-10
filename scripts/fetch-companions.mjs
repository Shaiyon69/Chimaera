// Builds the companion web apps from the sibling `Chimaera-Brood` repo and copies them
// into resources/companions/<id>, which the `companion:` scheme serves. Companions with
// `bundle` also get their card data and pictures (`<crate> --bundle <dir>`), so they
// open without waiting on the network.
// Usage: node scripts/fetch-companions.mjs [--skip-build]
//   --skip-build  copy the last build without running `dx build`
// Set CHIMAERA_COMPANIONS_DIR to use a checkout somewhere other than ../Chimaera-Brood.
// Without a checkout it warns and exits 0, so the app builds without companions.
import { spawnSync } from "node:child_process";
import { cpSync, existsSync, rmSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// id = folder under resources/companions (and the Dioxus base_path); crate = package in the repo.
const COMPANIONS = [{ id: "terraria", crate: "terraria-guide", bundle: true }];

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const repo = resolve(
  process.env.CHIMAERA_COMPANIONS_DIR ?? join(root, "..", "Chimaera-Brood")
);
const skipBuild = process.argv.includes("--skip-build");

if (!existsSync(join(repo, "Cargo.toml"))) {
  console.warn(
    `Chimaera-Brood repo not found at ${repo}; skipping companions.`
  );
  process.exit(0);
}

for (const { id, crate, bundle } of COMPANIONS) {
  const built = join(repo, "target", "dx", crate, "release", "web", "public");
  if (!skipBuild) {
    const result = spawnSync(
      "dx",
      ["build", "--platform", "web", "--release"],
      {
        cwd: join(repo, crate),
        stdio: "inherit",
        shell: process.platform === "win32",
      }
    );
    if (result.status !== 0) {
      console.error(`dx build failed for ${crate} (is dioxus-cli installed?)`);
      process.exit(1);
    }
    if (bundle) {
      // Missing items only fall back to the network at run time, so a failed bundle warns.
      const result = spawnSync(
        "cargo",
        ["run", "--release", "-q", "-p", crate, "--", "--bundle", built],
        { cwd: repo, stdio: "inherit", shell: process.platform === "win32" }
      );
      if (result.status !== 0)
        console.warn(
          `bundle failed for ${crate}; it will load from the network.`
        );
    }
  }
  if (!existsSync(join(built, "index.html"))) {
    console.error(`no web build at ${built}`);
    process.exit(1);
  }
  const dest = join(root, "resources", "companions", id);
  rmSync(dest, { recursive: true, force: true });
  cpSync(built, dest, { recursive: true });
  console.log(`companion ${id}: ${built} -> ${dest}`);
}
