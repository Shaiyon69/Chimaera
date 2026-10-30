import { test } from "node:test";
import assert from "node:assert/strict";
import { rewriteLocalUrls } from "./local-urls-core.js";

const convert = (path: string) =>
  `http://local.localhost/${encodeURIComponent(path)}`;

test("rewrites a plain local: URL", () => {
  assert.equal(
    rewriteLocalUrls("local:C:\\Users\\me\\avatar.webp", convert),
    "http://local.localhost/C%3A%5CUsers%5Cme%5Cavatar.webp"
  );
});

test("rewrites local: URLs inside CSS url() and decodes encodeURI paths", () => {
  assert.equal(
    rewriteLocalUrls(
      'linear-gradient(red, blue), url("local:C:/My%20Games/hero.jpg")',
      convert
    ),
    'linear-gradient(red, blue), url("http://local.localhost/C%3A%2FMy%20Games%2Fhero.jpg")'
  );
  assert.equal(
    rewriteLocalUrls("url(local:/home/me/a.png)", convert),
    "url(http://local.localhost/%2Fhome%2Fme%2Fa.png)"
  );
});

test("leaves other values alone", () => {
  assert.equal(
    rewriteLocalUrls("https://cdn/a.png", convert),
    "https://cdn/a.png"
  );
  assert.equal(rewriteLocalUrls("cached-image:x", convert), "cached-image:x");
  assert.equal(rewriteLocalUrls("a local: note", convert), "a local: note");
});

test("keeps a literal % that is not an escape", () => {
  assert.equal(
    rewriteLocalUrls("local:C:/100%.png", convert),
    `http://local.localhost/${encodeURIComponent("C:/100%.png")}`
  );
});
