// `local:<path>` at the start of a value or inside a CSS url(...); stops at the
// quote or paren that closes the url.
const LOCAL_URL = /(^|url\(\s*["']?)local:([^"')]+)/g;

/** Rewrites every `local:<path>` URL in an attribute or CSS value with `convert`. */
export const rewriteLocalUrls = (
  value: string,
  convert: (path: string) => string
) => {
  if (!value.includes("local:")) return value;
  return value.replace(LOCAL_URL, (_, prefix: string, rawPath: string) => {
    let path = rawPath;
    // Callers often encodeURI the path for CSS; Electron's handler decodes it too.
    try {
      path = decodeURI(rawPath);
    } catch {
      // Not encoded: a literal % in the file name.
    }
    return prefix + convert(path);
  });
};
