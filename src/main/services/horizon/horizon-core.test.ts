import { test } from "node:test";
import assert from "node:assert/strict";
import { pickLauncherRelease } from "./horizon-core.js";

const digest = `sha256:${"AB".repeat(32)}`;
const zip = (
  name = "horizon-launcher-windows-x64.zip",
  d: unknown = digest
) => ({
  name,
  browser_download_url: `https://example.test/${name}`,
  digest: d,
});

test("picks the newest stable launcher release, skipping game and nightly tags", () => {
  const release = pickLauncherRelease([
    { tag_name: "nightly", prerelease: true, assets: [zip()] },
    { tag_name: "v0.2.97", prerelease: false, assets: [zip()] },
    { tag_name: "launcher@0.2.28", prerelease: false, assets: [zip()] },
    { tag_name: "launcher@0.2.27", prerelease: false, assets: [zip()] },
  ]);
  assert.deepEqual(release, {
    tag: "launcher@0.2.28",
    url: "https://example.test/horizon-launcher-windows-x64.zip",
    sha256: "ab".repeat(32),
  });
});

test("skips launcher releases without a verifiable Windows zip", () => {
  const release = pickLauncherRelease([
    {
      tag_name: "launcher@0.2.30",
      assets: [zip("horizon-launcher-linux-x86_64.tar.gz")],
    },
    { tag_name: "launcher@0.2.29", assets: [zip(undefined, null)] },
    { tag_name: "launcher@0.2.28", assets: [zip()] },
  ]);
  assert.equal(release?.tag, "launcher@0.2.28");
});

test("returns null for a bad payload", () => {
  assert.equal(pickLauncherRelease({ message: "rate limited" }), null);
  assert.equal(pickLauncherRelease([null, "x"]), null);
});
