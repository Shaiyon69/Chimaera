// The renderer points at files on disk with `local:<path>` URLs, which Electron
// serves with protocol.handle. Tauri serves its `local` scheme at a per-platform
// URL (http://local.localhost/ on Windows), so rewrite those URLs wherever they
// reach the page: element attributes, inline styles and fetch.
import { convertFileSrc } from "@tauri-apps/api/core";
import { rewriteLocalUrls } from "./local-urls-core";

const rewrite = (value: string) =>
  rewriteLocalUrls(value, (path) => convertFileSrc(path, "local"));

const URL_ATTRIBUTES = new Set(["src", "href", "poster", "style"]);

const setAttribute = Element.prototype.setAttribute;
Element.prototype.setAttribute = function (name: string, value: string) {
  const rewritten =
    URL_ATTRIBUTES.has(name.toLowerCase()) && typeof value === "string"
      ? rewrite(value)
      : value;
  setAttribute.call(this, name, rewritten);
};

const patchSetter = (prototype: object, property: string) => {
  const descriptor = Object.getOwnPropertyDescriptor(prototype, property);
  if (!descriptor?.set) return;
  const set = descriptor.set;
  Object.defineProperty(prototype, property, {
    ...descriptor,
    set(value: unknown) {
      set.call(this, typeof value === "string" ? rewrite(value) : value);
    },
  });
};

for (const prototype of [
  HTMLImageElement.prototype,
  HTMLVideoElement.prototype,
  HTMLSourceElement.prototype,
  HTMLAudioElement.prototype,
]) {
  patchSetter(prototype, "src");
}
patchSetter(HTMLVideoElement.prototype, "poster");
// React writes inline styles as style[name] = value.
for (const property of [
  "background",
  "backgroundImage",
  "maskImage",
  "webkitMaskImage",
  "content",
  "listStyleImage",
  "borderImage",
  "borderImageSource",
]) {
  patchSetter(CSSStyleDeclaration.prototype, property);
}

const setProperty = CSSStyleDeclaration.prototype.setProperty;
CSSStyleDeclaration.prototype.setProperty = function (
  property: string,
  value: string | null,
  priority?: string
) {
  setProperty.call(
    this,
    property,
    typeof value === "string" ? rewrite(value) : value,
    priority
  );
};

const nativeFetch = globalThis.fetch;
globalThis.fetch = (input: RequestInfo | URL, init?: RequestInit) =>
  nativeFetch(typeof input === "string" ? rewrite(input) : input, init);
