import assert from "node:assert/strict";
import crypto from "node:crypto";
import { describe, it } from "node:test";

import {
  backupsToPrune,
  buildAuthUrl,
  buildMultipartBody,
  createPkcePair,
  escapeQueryValue,
  gameFolderName,
} from "./drive-core.js";

describe("google drive core", () => {
  it("names game folders like the local backup folders", () => {
    assert.equal(gameFolderName("steam", "620"), "steam-620");
    assert.equal(gameFolderName("custom", "a/b:c"), "custom-a_b_c");
  });

  it("escapes quotes and backslashes in query values", () => {
    assert.equal(escapeQueryValue("it's a\\b"), "it\\'s a\\\\b");
  });

  it("builds a PKCE pair whose challenge is the verifier's SHA-256", () => {
    const { verifier, challenge } = createPkcePair();
    assert.equal(
      challenge,
      crypto.createHash("sha256").update(verifier).digest("base64url")
    );
  });

  it("asks for offline access with the PKCE challenge", () => {
    const url = new URL(
      buildAuthUrl({
        clientId: "id",
        redirectUri: "http://127.0.0.1:1234",
        challenge: "abc",
        state: "s",
      })
    );
    assert.equal(url.searchParams.get("code_challenge"), "abc");
    assert.equal(url.searchParams.get("access_type"), "offline");
    assert.match(url.searchParams.get("scope")!, /drive\.file/);
  });

  it("wraps metadata and bytes in a multipart body", () => {
    const body = buildMultipartBody(
      { name: "x.tar" },
      Buffer.from("DATA"),
      "B"
    );
    const text = body.toString();
    assert.match(text, /^--B\r\nContent-Type: application\/json/);
    assert.ok(text.includes('{"name":"x.tar"}'));
    assert.ok(text.endsWith("DATA\r\n--B--"));
  });

  it("prunes everything older than the newest N", () => {
    const backups = ["01", "02", "03", "04"].map((d) => ({
      id: `f${d}`,
      createdAt: `2026-10-${d}T00:00:00.000Z`,
    }));
    assert.deepEqual(backupsToPrune(backups, 2), ["f02", "f01"]);
    assert.deepEqual(backupsToPrune(backups, 10), []);
  });
});
