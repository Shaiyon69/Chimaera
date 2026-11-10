import { useCallback, useEffect, useState } from "react";
import type { Session } from "@supabase/supabase-js";

import { logger } from "@renderer/logger";

import { messapp } from "./client";
import { fetchFriends, signOut, unlockKeyring, type FriendRow } from "./api";
import { createKeyring, type DmKeyring } from "./dm-crypto";

const friendsCacheKey = (userId: string) => `messapp:friends:${userId}`;

const readFriendsCache = (userId: string): FriendRow[] => {
  try {
    return JSON.parse(localStorage.getItem(friendsCacheKey(userId)) ?? "[]");
  } catch {
    return [];
  }
};

// Supabase refreshes the access token in the background; reading it fresh
// each time (rather than from a closure) keeps the main process's session
// check from failing on a token that expired since the last render.
const currentAccessToken = async () =>
  (await messapp.auth.getSession()).data.session?.access_token ?? null;

export type FriendsStatus = "loading" | "ready" | "error";

/** Drops Chimaera's old key entry and cached friends, then ends the session. The shared
 * key vault stays, as on a Messapp sign-out: Messapp desktop and Purrch on this PC use it. */
export const logoutMessapp = async (userId: string | null) => {
  try {
    const token = await currentAccessToken();
    if (token) await window.electron.clearMessappKey(token);
  } catch (err) {
    logger.error("Failed to clear saved key", err);
  }

  if (userId) localStorage.removeItem(friendsCacheKey(userId));
  await signOut();
};

/** The signed-in Friends user id, for UI outside the Friends page. */
export function useMessappUserId() {
  const [userId, setUserId] = useState<string | null>(null);

  useEffect(() => {
    messapp.auth
      .getSession()
      .then(({ data }) => setUserId(data.session?.user.id ?? null));

    const { data } = messapp.auth.onAuthStateChange((_event, next) =>
      setUserId(next?.user.id ?? null)
    );

    return () => data.subscription.unsubscribe();
  }, []);

  return userId;
}

export function useMessapp() {
  const [session, setSession] = useState<Session | null>(null);
  const [ready, setReady] = useState(false);
  const [keyring, setKeyring] = useState<DmKeyring | null>(null);
  const [friends, setFriends] = useState<FriendRow[]>([]);
  const [friendsStatus, setFriendsStatus] = useState<FriendsStatus>("loading");

  const userId = session?.user.id ?? null;

  useEffect(() => {
    messapp.auth.getSession().then(({ data }) => {
      setSession(data.session);
      setReady(true);
    });

    const { data } = messapp.auth.onAuthStateChange((_event, next) =>
      setSession(next)
    );

    return () => data.subscription.unsubscribe();
  }, []);

  // The cache shows the last list instantly and while offline.
  const refreshFriends = useCallback(async () => {
    if (!userId) return;

    const rows = await fetchFriends(userId);
    setFriends(rows);
    setFriendsStatus("ready");
    localStorage.setItem(friendsCacheKey(userId), JSON.stringify(rows));
  }, [userId]);

  const retryFriends = useCallback(async () => {
    setFriendsStatus("loading");
    try {
      await refreshFriends();
    } catch (err) {
      logger.error("Failed to load friends", err);
      setFriendsStatus("error");
    }
  }, [refreshFriends]);

  useEffect(() => {
    if (!userId) {
      setFriends([]);
      setKeyring(null);
      return;
    }

    const cached = readFriendsCache(userId);
    setFriends(cached);
    // A cached list is shown straight away; only an empty one needs the loading state.
    setFriendsStatus(cached.length ? "ready" : "loading");
    retryFriends();

    // Saved by a previous unlock or by Messapp desktop on this PC (one shared OS vault),
    // keyed in Rust by a session it verifies for itself.
    (async () => {
      const token = await currentAccessToken();
      if (!token) return;

      const saved = await window.electron.loadMessappKey(token);
      if (saved) setKeyring(await createKeyring(saved.private, saved.legacy));
    })().catch((err) => logger.error("Failed to load saved key", err));
  }, [userId, retryFriends]);

  const unlock = useCallback(async (pin: string) => {
    const token = await currentAccessToken();
    if (!token) throw new Error("Not signed in");

    const { keyring: unlocked, privateJwk } = await unlockKeyring(pin);
    await window.electron.saveMessappKey(token, privateJwk);
    setKeyring(unlocked);
  }, []);

  return {
    ready,
    userId,
    keyring,
    friends,
    friendsStatus,
    refreshFriends,
    retryFriends,
    unlock,
  };
}
