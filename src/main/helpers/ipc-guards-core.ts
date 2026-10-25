import path from "node:path";

// Only schemes that open a browser or mail client; file:, javascript: and custom
// protocol handlers can launch local programs.
const ALLOWED_EXTERNAL_PROTOCOLS = new Set(["https:", "http:", "mailto:"]);

export const TEMP_FILE_PREFIX = "hydra-temp-";

export const isSafeExternalUrl = (raw: unknown): raw is string => {
  if (typeof raw !== "string") return false;

  try {
    return ALLOWED_EXTERNAL_PROTOCOLS.has(new URL(raw).protocol);
  } catch {
    return false;
  }
};

// Only files that saveTempFile created: directly inside tempDir, with its prefix.
export const isOwnTempFile = (filePath: unknown, tempDir: string): boolean => {
  if (typeof filePath !== "string" || !path.isAbsolute(filePath)) return false;

  const resolved = path.resolve(filePath);

  return (
    path.dirname(resolved) === path.resolve(tempDir) &&
    path.basename(resolved).startsWith(TEMP_FILE_PREFIX)
  );
};
