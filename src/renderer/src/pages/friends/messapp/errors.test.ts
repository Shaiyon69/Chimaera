import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { classifyAuthError, classifyUnlockError } from "./errors.js";

describe("classifyAuthError", () => {
  it("names the cases it recognises", () => {
    assert.equal(
      classifyAuthError(new Error("Invalid login credentials")),
      "invalid_credentials"
    );
    assert.equal(
      classifyAuthError(new Error("User already registered")),
      "already_registered"
    );
    assert.equal(
      classifyAuthError(new Error("email rate limit exceeded")),
      "rate_limited"
    );
    assert.equal(classifyAuthError(new Error("Failed to fetch")), "network");
  });

  it("falls back to unknown", () => {
    assert.equal(classifyAuthError(new Error("boom")), "unknown");
    assert.equal(classifyAuthError("boom"), "unknown");
  });
});

describe("classifyUnlockError", () => {
  it("only blames the PIN for a failed decrypt", () => {
    assert.equal(classifyUnlockError(new Error("Incorrect PIN")), "wrong_pin");
    assert.equal(classifyUnlockError(new Error("Failed to fetch")), "network");
    assert.equal(classifyUnlockError(new Error("JWT expired")), "unknown");
  });

  it("maps backup problems", () => {
    assert.equal(
      classifyUnlockError(
        new Error("No key backup. Set a PIN in MESSAPP first.")
      ),
      "no_backup"
    );
    assert.equal(
      classifyUnlockError(
        new Error("Key backup does not match your published key.")
      ),
      "key_mismatch"
    );
  });
});
