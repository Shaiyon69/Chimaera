// Never show a raw SDK/network error to the user; map the few cases we can
// name to calm copy and fall back to one generic message otherwise.
export const isNetworkError = (err: unknown) =>
  err instanceof Error && /failed to fetch|network/i.test(err.message);

export type AuthErrorKind =
  | "invalid_credentials"
  | "already_registered"
  | "rate_limited"
  | "network"
  | "unknown";

export const classifyAuthError = (err: unknown): AuthErrorKind => {
  if (isNetworkError(err)) return "network";
  if (!(err instanceof Error)) return "unknown";
  if (err.message === "Invalid login credentials") return "invalid_credentials";
  if (/already registered/i.test(err.message)) return "already_registered";
  if (/rate limit|too many/i.test(err.message)) return "rate_limited";
  return "unknown";
};

export type UnlockErrorKind =
  | "wrong_pin"
  | "no_backup"
  | "key_mismatch"
  | "network"
  | "unknown";

// Only a failed PIN decrypt may say "Incorrect PIN"; any other failure would send the user guessing PINs.
export const classifyUnlockError = (err: unknown): UnlockErrorKind => {
  if (isNetworkError(err)) return "network";
  if (!(err instanceof Error)) return "unknown";
  if (err.message === "Incorrect PIN") return "wrong_pin";
  if (err.message.startsWith("No key backup")) return "no_backup";
  if (err.message.startsWith("Key backup does not match")) {
    return "key_mismatch";
  }
  return "unknown";
};
