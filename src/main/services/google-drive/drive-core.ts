import crypto from "node:crypto";

export const ROOT_FOLDER = "Chimaera Saves";
export const FOLDER_MIME = "application/vnd.google-apps.folder";
export const DEFAULT_KEEP = 10;

export const SCOPES = [
  "https://www.googleapis.com/auth/drive.file",
  "openid",
  "email",
];

/** Same sanitising as the local per-game folder, so both sides name a game alike. */
export const gameFolderName = (shop: string, objectId: string) =>
  `${shop}-${objectId}`.replace(/[<>:"/\\|?*]+/g, "_");

/** Escapes a value for use inside a single-quoted Drive `q` string. */
export const escapeQueryValue = (value: string) =>
  value.replace(/\\/g, "\\\\").replace(/'/g, "\\'");

const base64Url = (buf: Buffer) => buf.toString("base64url");

export const createPkcePair = () => {
  const verifier = base64Url(crypto.randomBytes(48));
  const challenge = base64Url(
    crypto.createHash("sha256").update(verifier).digest()
  );
  return { verifier, challenge };
};

export const buildAuthUrl = (params: {
  clientId: string;
  redirectUri: string;
  challenge: string;
  state: string;
}) => {
  const url = new URL("https://accounts.google.com/o/oauth2/v2/auth");
  url.search = new URLSearchParams({
    client_id: params.clientId,
    redirect_uri: params.redirectUri,
    response_type: "code",
    scope: SCOPES.join(" "),
    code_challenge: params.challenge,
    code_challenge_method: "S256",
    state: params.state,
    // Without these Google only returns a refresh token on the first consent.
    access_type: "offline",
    prompt: "consent",
  }).toString();
  return url.toString();
};

/**
 * Drive multipart upload body: JSON metadata part, then the file bytes.
 * shortcut: whole file buffered in memory, save tars are small; use a
 * resumable upload if backups ever reach hundreds of MB.
 */
export const buildMultipartBody = (
  metadata: Record<string, unknown>,
  content: Buffer,
  boundary: string
) =>
  Buffer.concat([
    Buffer.from(
      `--${boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n` +
        `${JSON.stringify(metadata)}\r\n` +
        `--${boundary}\r\nContent-Type: application/x-tar\r\n\r\n`
    ),
    content,
    Buffer.from(`\r\n--${boundary}--`),
  ]);

/** Ids of the oldest backups beyond the newest `keep`. */
export const backupsToPrune = (
  backups: { id: string; createdAt: string }[],
  keep = DEFAULT_KEEP
) =>
  [...backups]
    .sort((a, b) => b.createdAt.localeCompare(a.createdAt))
    .slice(keep)
    .map((b) => b.id);
