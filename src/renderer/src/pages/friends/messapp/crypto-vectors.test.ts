// Compatibility gate: crypto-vectors.json is shaiyon-core's tests/vectors.json, made by
// MESSAPP's crypto.js (shaiyon-core scripts/gen-vectors.mjs). Copy it again after a regen.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

import {
  decryptBytes,
  deriveSharedKey,
  importPrivateKey,
  toBase64,
  unwrapPrivateKey,
} from "./crypto.js";
import { createKeyring, decryptDm } from "./dm-crypto.js";

const v = JSON.parse(
  readFileSync(new URL("./crypto-vectors.json", import.meta.url), "utf8")
);
const publicOf = ({ kty, crv, x, y }: JsonWebKey): JsonWebKey => ({
  kty,
  crv,
  x,
  y,
});
const content = (name: string) => JSON.stringify(v[name]);

describe("MESSAPP crypto vectors", () => {
  it("opens a DM as receiver and as sender", async () => {
    const bob = await createKeyring(v.bob);
    const alice = await createKeyring(v.alice);
    assert.equal(
      await decryptDm(bob, content("textEnvelope"), false, publicOf(v.alice)),
      v.text
    );
    assert.equal(
      await decryptDm(alice, content("textEnvelope"), true, publicOf(v.bob)),
      v.text
    );
  });

  it("opens messages sealed with a legacy key or to self", async () => {
    const alice = await createKeyring(v.alice, [v.aliceOld]);
    const bob = publicOf(v.bob);
    assert.equal(
      await decryptDm(alice, content("legacyEnvelope"), false, bob),
      v.legacyText
    );
    assert.equal(
      await decryptDm(alice, content("selfEnvelope"), true, bob),
      v.selfText
    );
    const noLegacy = await createKeyring(v.alice);
    assert.equal(
      await decryptDm(noLegacy, content("legacyEnvelope"), false, bob),
      null
    );
  });

  it("opens a binary attachment", async () => {
    const key = await deriveSharedKey(
      await importPrivateKey(v.bob),
      publicOf(v.alice)
    );
    const bytes = await decryptBytes(key, v.binaryEnvelope);
    assert.ok(bytes);
    assert.equal(toBase64(bytes), v.binaryBase64);
  });

  for (const name of [
    "pinBackupV3",
    "pinBackupLegacySalted",
    "pinBackupLegacyUnsalted",
  ]) {
    it(`unwraps ${name}`, async () => {
      assert.deepEqual(
        await unwrapPrivateKey(v.pin, v[name]),
        JSON.parse(v.privateJson)
      );
    });
  }

  it("refuses a wrong PIN", async () => {
    await assert.rejects(unwrapPrivateKey("000000", v.pinBackupV3));
  });
});
