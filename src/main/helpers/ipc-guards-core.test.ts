import assert from "node:assert/strict";
import os from "node:os";
import path from "node:path";
import { describe, it } from "node:test";

import { isOwnTempFile, isSafeExternalUrl } from "./ipc-guards-core.js";

describe("isSafeExternalUrl", () => {
  it("allows web and mail links", () => {
    assert.equal(
      isSafeExternalUrl("https://store.steampowered.com/app/1/"),
      true
    );
    assert.equal(isSafeExternalUrl("http://example.com"), true);
    assert.equal(isSafeExternalUrl("mailto:a@b.co"), true);
  });

  it("rejects schemes that can launch local programs", () => {
    for (const url of [
      "file:///C:/Windows/System32/calc.exe",
      "javascript:alert(1)",
      "steam://run/1",
      "ms-msdt:/id",
      "\\\\host\\share\\x.exe",
      "C:\\Windows\\System32\\calc.exe",
      "not a url",
      "",
    ]) {
      assert.equal(isSafeExternalUrl(url), false, url);
    }
  });

  it("rejects non-strings", () => {
    assert.equal(isSafeExternalUrl(undefined), false);
    assert.equal(isSafeExternalUrl({ toString: () => "https://a.b" }), false);
  });
});

describe("isOwnTempFile", () => {
  const tempDir = os.tmpdir();

  it("accepts a file saveTempFile would create", () => {
    assert.equal(
      isOwnTempFile(path.join(tempDir, "hydra-temp-1-a.png"), tempDir),
      true
    );
  });

  it("rejects other files, traversal and nested paths", () => {
    assert.equal(
      isOwnTempFile(path.join(tempDir, "other.png"), tempDir),
      false
    );
    assert.equal(
      isOwnTempFile(path.join(tempDir, "hydra-temp-1", "x"), tempDir),
      false
    );
    assert.equal(
      isOwnTempFile(
        path.join(tempDir, "hydra-temp-1", "..", "..", "victim.txt"),
        tempDir
      ),
      false
    );
    assert.equal(isOwnTempFile("hydra-temp-1-a.png", tempDir), false);
    assert.equal(isOwnTempFile(undefined, tempDir), false);
  });
});
