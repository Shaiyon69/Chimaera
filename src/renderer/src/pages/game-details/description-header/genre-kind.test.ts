import { test } from "node:test";
import assert from "node:assert/strict";
import { genreKind } from "./genre-kind.js";

test("Steam genres map by id even when localized", () => {
  assert.equal(genreKind({ id: "3", description: "Rol" }, true), "rpg");
  assert.equal(genreKind({ id: "999", description: "Action" }, true), "action");
});

test("other shops map by name, specific genres first", () => {
  assert.equal(genreKind({ id: "0", name: "Action RPG" }, false), "rpg");
  assert.equal(
    genreKind({ id: "1", name: "Survival Horror" }, false),
    "horror"
  );
  assert.equal(genreKind({ id: "1", name: "Platform" }, false), "platformer");
  // Mapped shops number genres by position, so "3" is not Steam's RPG.
  assert.equal(genreKind({ id: "3", name: "Mystery" }, false), "other");
});
