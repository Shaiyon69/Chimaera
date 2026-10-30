import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import {
  engineDirOf,
  gameDir,
  isPortDir,
  parseTrophyLog,
  pickPortRelease,
  savesDir,
} from "./bloodborne-core.js";

const touch = (file: string) => {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, "");
};

test("isPortDir needs Lantern Engine.exe and the engine, in engine/ or at the top", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "bb-port-"));
  try {
    assert.equal(isPortDir(dir), false);
    touch(path.join(dir, "Lantern Engine.exe"));
    assert.equal(isPortDir(dir), false);
    touch(path.join(dir, "out", "bb-probe.exe"));
    assert.equal(isPortDir(dir), true);
    assert.equal(engineDirOf(dir), dir);
    touch(path.join(dir, "engine", "out", "bb-probe.exe"));
    assert.equal(engineDirOf(dir), path.join(dir, "engine"));
    assert.equal(isPortDir(dir), true);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("pickPortRelease takes the Windows package zip and its digest", () => {
  const sha = "a".repeat(64);
  const release = {
    tag_name: "v0.9.38",
    assets: [
      { name: "Lantern.Engine.exe", browser_download_url: "https://x/exe" },
      {
        name: "LanternEngine-v0.9.38-linux-x64.tar.gz",
        browser_download_url: "https://x/linux",
        digest: `sha256:${"b".repeat(64)}`,
      },
      {
        name: "LanternEngine-v0.9.38-win64.zip",
        browser_download_url: "https://x/zip",
        digest: `sha256:${sha}`,
      },
    ],
  };
  assert.deepEqual(pickPortRelease(release), {
    tag: "v0.9.38",
    url: "https://x/zip",
    sha256: sha,
  });
  assert.equal(pickPortRelease({ ...release, assets: [] }), null);
  assert.equal(
    pickPortRelease({
      tag_name: "v1",
      assets: [
        { name: "LanternEngine-v1-win64.zip", browser_download_url: "u" },
      ],
    }),
    null
  );
  assert.equal(pickPortRelease(null), null);
});

test("savesDir follows user_dir and the player profile", () => {
  const port = path.join("C:", "le");
  assert.equal(savesDir(port, {}), path.join(port, "user"));
  assert.equal(
    savesDir(port, { player_profile: "Main" }),
    path.join(port, "user")
  );
  assert.equal(
    savesDir(port, {
      user_dir: path.join("D:", "saves"),
      player_profile: "Sam",
    }),
    path.join("D:", "saves", "profiles", "Sam")
  );
});

test("gameDir reads game_dir", () => {
  assert.equal(gameDir({ game_dir: "E:\\CUSA03173" }), "E:\\CUSA03173");
  assert.equal(gameDir({}), "");
});

test("parseTrophyLog reads id and unix seconds, skipping other lines", () => {
  assert.deepEqual(parseTrophyLog("14 1700000000\r\n\nnope\n21 1700000060\n"), [
    { id: 14, unlockTime: 1700000000000 },
    { id: 21, unlockTime: 1700000060000 },
  ]);
});
