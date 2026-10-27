import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { applyChimaeraProfile, shownName } from "./profile-merge.js";

const base = {
  username: "alice",
  avatar_url: "https://x/a.png",
  status_text: "busy",
};

const override = {
  display_name: null,
  avatar_url: null,
  status_text: null,
  status_expires_at: null,
  bio: null,
  banner_url: null,
};

describe("applyChimaeraProfile", () => {
  it("shows nothing from MESSAPP when there is no override", () => {
    const merged = applyChimaeraProfile(base, undefined);

    assert.equal(merged.avatar_url, null);
    assert.equal(merged.status_text, null);
    assert.equal(merged.display_name, null);
    assert.equal(shownName(merged), "alice");
  });

  it("uses filled override fields and leaves blank ones blank", () => {
    const merged = applyChimaeraProfile(base, {
      ...override,
      display_name: "Ali the Hunter",
      avatar_url: "   ",
      status_text: "Playing Bloodborne",
    });

    assert.equal(shownName(merged), "Ali the Hunter");
    assert.equal(merged.avatar_url, null);
    assert.equal(merged.status_text, "Playing Bloodborne");
  });

  it("drops an expired status", () => {
    const merged = applyChimaeraProfile(base, {
      ...override,
      status_text: "Playing Bloodborne",
      status_expires_at: new Date(Date.now() - 1000).toISOString(),
    });

    assert.equal(merged.status_text, null);
  });
});
