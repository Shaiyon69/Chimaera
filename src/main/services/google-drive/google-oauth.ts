import http from "node:http";
import path from "node:path";
import crypto from "node:crypto";
import axios from "axios";
import jwt from "jsonwebtoken";
import { app, safeStorage, shell } from "electron";
import { createKeyStore } from "../messapp/key-store-core";
import { logger } from "../logger";
import { buildAuthUrl, createPkcePair } from "./drive-core";

const TOKEN_URL = "https://oauth2.googleapis.com/token";
const CONNECT_TIMEOUT_MS = 5 * 60_000;
const STORE_KEY = "default";

interface StoredAccount {
  refreshToken: string;
  email: string | null;
}

const keyStore = createKeyStore(
  path.join(app.getPath("userData"), "google-drive.json"),
  {
    available: () => safeStorage.isEncryptionAvailable(),
    encrypt: (text) => safeStorage.encryptString(text),
    decrypt: (data) => safeStorage.decryptString(data),
  }
);

// Used when the OS keystore is unavailable, so the session lasts until restart.
let memoryAccount: StoredAccount | null = null;
let accessToken: { value: string; expiresAt: number } | null = null;

const loadAccount = (): StoredAccount | null => {
  const raw = keyStore.load(STORE_KEY);
  if (raw) {
    try {
      return JSON.parse(raw) as StoredAccount;
    } catch {
      return null;
    }
  }
  return memoryAccount;
};

const saveAccount = (account: StoredAccount) => {
  memoryAccount = account;
  keyStore.save(STORE_KEY, JSON.stringify(account));
};

const clientCredentials = () => ({
  client_id: import.meta.env.MAIN_VITE_GOOGLE_CLIENT_ID,
  client_secret: import.meta.env.MAIN_VITE_GOOGLE_CLIENT_SECRET,
});

const tokenRequest = (params: Record<string, string>) =>
  axios
    .post<{
      access_token: string;
      expires_in: number;
      refresh_token?: string;
      id_token?: string;
    }>(TOKEN_URL, new URLSearchParams({ ...clientCredentials(), ...params }))
    .then((res) => res.data);

const page = (message: string) =>
  `<!doctype html><meta charset="utf-8"><title>Chimaera</title><body style="font-family:sans-serif;padding:2rem">${message}</body>`;

/** Waits for Google's redirect on a one-shot loopback server and returns the code. */
const waitForCode = (state: string, challenge: string) =>
  new Promise<{ code: string; redirectUri: string; close: () => void }>(
    (resolve, reject) => {
      const server = http.createServer();
      const close = () => server.close();
      const timer = setTimeout(() => {
        close();
        reject(new Error("google-drive/timeout"));
      }, CONNECT_TIMEOUT_MS);

      server.on("request", (req, res) => {
        const url = new URL(req.url ?? "/", "http://127.0.0.1");
        const code = url.searchParams.get("code");

        // Ignore stray hits (favicon etc.) and callbacks that don't carry our state.
        if (!code || url.searchParams.get("state") !== state) {
          res.writeHead(400).end(page("Invalid request."));
          return;
        }

        clearTimeout(timer);
        res.writeHead(200, { "Content-Type": "text/html" });
        res.end(page("Google Drive connected. You can close this tab."));
        const { port } = server.address() as { port: number };
        resolve({ code, redirectUri: `http://127.0.0.1:${port}`, close });
      });

      server.on("error", (err) => {
        clearTimeout(timer);
        reject(err);
      });

      server.listen(0, "127.0.0.1", () => {
        const { port } = server.address() as { port: number };
        void shell.openExternal(
          buildAuthUrl({
            clientId: clientCredentials().client_id,
            redirectUri: `http://127.0.0.1:${port}`,
            challenge,
            state,
          })
        );
      });
    }
  );

export const connectGoogleDrive = async () => {
  if (!clientCredentials().client_id) {
    throw new Error("google-drive/not-configured");
  }

  const state = crypto.randomBytes(16).toString("hex");
  const { verifier, challenge } = createPkcePair();

  const { code, redirectUri, close } = await waitForCode(state, challenge);
  try {
    const tokens = await tokenRequest({
      grant_type: "authorization_code",
      code,
      code_verifier: verifier,
      redirect_uri: redirectUri,
    });

    if (!tokens.refresh_token) throw new Error("google-drive/no-refresh-token");

    const claims = tokens.id_token ? jwt.decode(tokens.id_token) : null;
    const email =
      claims && typeof claims === "object" ? (claims.email ?? null) : null;

    saveAccount({ refreshToken: tokens.refresh_token, email });
    accessToken = {
      value: tokens.access_token,
      expiresAt: Date.now() + tokens.expires_in * 1000,
    };
    return { connected: true, email };
  } finally {
    close();
  }
};

export const getGoogleDriveStatus = () => {
  const account = loadAccount();
  return { connected: !!account, email: account?.email ?? null };
};

export const disconnectGoogleDrive = async () => {
  const account = loadAccount();
  accessToken = null;
  memoryAccount = null;
  keyStore.clear(STORE_KEY);

  if (!account) return;
  // Best effort: the local token is already gone, revocation only tidies Google's side.
  await axios
    .post(
      "https://oauth2.googleapis.com/revoke",
      new URLSearchParams({ token: account.refreshToken })
    )
    .catch((err) => logger.error("Failed to revoke Google token", err));
};

export const getGoogleAccessToken = async () => {
  if (accessToken && accessToken.expiresAt - 60_000 > Date.now()) {
    return accessToken.value;
  }

  const account = loadAccount();
  if (!account) throw new Error("google-drive/not-connected");

  const tokens = await tokenRequest({
    grant_type: "refresh_token",
    refresh_token: account.refreshToken,
  });
  accessToken = {
    value: tokens.access_token,
    expiresAt: Date.now() + tokens.expires_in * 1000,
  };
  return accessToken.value;
};
