import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { languageNames } from "./language-names.js";

const localesDir = path.resolve("src/locales");

test("languageNames lists every locale folder with its language_name", () => {
  const folders = fs
    .readdirSync(localesDir, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort();

  assert.deepEqual(Object.keys(languageNames).sort(), folders);

  for (const folder of folders) {
    const translation = JSON.parse(
      fs.readFileSync(path.join(localesDir, folder, "translation.json"), "utf8")
    ) as { language_name: string };
    assert.equal(languageNames[folder], translation.language_name, folder);
  }
});
