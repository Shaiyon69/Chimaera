import type { BackendModule, InitOptions, ResourceKey } from "i18next";
import en from "./en/translation.json";
import { languageNames } from "./language-names";

export { languageNames };

// Only English (the fallback) ships in the startup bundle. The other languages
// were about 2.6 MB of JS that every launch parsed; they now load on demand.
export const localeBackend: BackendModule = {
  type: "backend",
  init() {
    // Nothing to configure; read() does all the work.
  },
  read(language, namespace, callback) {
    // Unknown codes (e.g. "en-US" from the detector) fall back to "en".
    if (language === "en" || !(language in languageNames)) {
      callback(null, {});
      return;
    }
    import(`./${language}/translation.json`).then(
      (module: { default: Record<string, ResourceKey> }) =>
        callback(null, module.default[namespace] ?? {}),
      (error: Error) => callback(error, null)
    );
  },
};

// Every top-level key of translation.json is a namespace. Listing them makes
// init and changeLanguage load all of them before rendering, so nothing suspends.
export const localeInitOptions: InitOptions = {
  resources: { en },
  partialBundledLanguages: true,
  ns: Object.keys(en),
  // Keep the i18next default; without it the first namespace above becomes the default.
  defaultNS: "translation",
};
