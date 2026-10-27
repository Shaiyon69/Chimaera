import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  attachmentObjectPath,
  openAttachment,
  sealAttachment,
} from "./attachment-blob.js";
import { createKeyring } from "./dm-crypto.js";

const newKeyring = async () => {
  const pair = await crypto.subtle.generateKey(
    { name: "ECDH", namedCurve: "P-256" },
    true,
    ["deriveKey"]
  );
  const keyring = await createKeyring(
    await crypto.subtle.exportKey("jwk", pair.privateKey)
  );

  return keyring;
};

describe("attachment blob", () => {
  it("round-trips for both sides and carries MESSAPP's fields", async () => {
    const alice = await newKeyring();
    const bob = await newKeyring();
    const bytes = new Uint8Array([1, 2, 3, 250, 251]).buffer;

    const blob = await sealAttachment(alice, bob.publicJwk, bytes, {
      type: "image/png",
      name: "a.png",
      size: 5,
    });

    assert.deepEqual(
      Object.keys(blob).sort(),
      ["ciphertext", "iv", "name", "size", "spub", "tpub", "type"].sort()
    );
    assert.deepEqual(
      new Uint8Array((await openAttachment(alice, blob, true, bob.publicJwk))!),
      new Uint8Array(bytes)
    );
    assert.deepEqual(
      new Uint8Array(
        (await openAttachment(bob, blob, false, alice.publicJwk))!
      ),
      new Uint8Array(bytes)
    );
  });

  it("returns null for a third party", async () => {
    const alice = await newKeyring();
    const bob = await newKeyring();
    const eve = await newKeyring();

    const blob = await sealAttachment(
      alice,
      bob.publicJwk,
      new Uint8Array([9]).buffer,
      { type: "image/png", name: "a.png", size: 1 }
    );

    assert.equal(await openAttachment(eve, blob, false, alice.publicJwk), null);
  });
});

describe("attachmentObjectPath", () => {
  it("extracts the path from public and signed URLs", () => {
    assert.equal(
      attachmentObjectPath(
        "https://x.supabase.co/storage/v1/object/public/chat-attachments/u/r/f.json"
      ),
      "u/r/f.json"
    );
    assert.equal(
      attachmentObjectPath(
        "https://x.supabase.co/storage/v1/object/sign/chat-attachments/u/r/f.json?token=1"
      ),
      "u/r/f.json"
    );
  });

  it("passes bare paths and rejects foreign or local URLs", () => {
    assert.equal(attachmentObjectPath("u/r/f.json"), "u/r/f.json");
    assert.equal(attachmentObjectPath("https://example.com/a.png"), "");
    assert.equal(attachmentObjectPath("blob:abc"), "");
  });
});
