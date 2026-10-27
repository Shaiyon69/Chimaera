import { useSyncExternalStore } from "react";

import {
  isUnread,
  seedReadMarks,
  unreadPeers,
  type LatestMessage,
  type ReadMarks,
} from "./unread-core";

// Module-level so the nav dock and the Friends page share one view of the inbox.
// Read marks live only on this device; the shared MESSAPP schema has no place for them.
const readKey = (userId: string) => `messapp:read:${userId}`;

let me: string | null = null;
let latest = new Map<string, LatestMessage>();
let read: ReadMarks = {};
let viewing: string | null = null;
let unread: ReadonlySet<string> = new Set();

const listeners = new Set<() => void>();

const publish = () => {
  unread = unreadPeers(latest, read);
  listeners.forEach((listener) => listener());
};

const persist = () => {
  if (!me) return;
  try {
    localStorage.setItem(readKey(me), JSON.stringify(read));
  } catch {
    // Storage can be blocked; read marks then last for this session only.
  }
};

const loadReadMarks = (userId: string): ReadMarks | null => {
  try {
    const raw = localStorage.getItem(readKey(userId));
    return raw ? JSON.parse(raw) : null;
  } catch {
    return null;
  }
};

const isWatching = (peer: string) => viewing === peer && document.hasFocus();

export const markRead = (peer: string) => {
  const message = latest.get(peer);
  if (!message || read[peer] === message.createdAt) return;

  read = { ...read, [peer]: message.createdAt };
  persist();
  publish();
};

/** Called by the Friends page with the open chat's peer, or null when none is open. */
export const setViewingPeer = (peer: string | null) => {
  viewing = peer;
  if (peer && document.hasFocus()) markRead(peer);
};

/** Window regained focus while a chat was already on screen. */
export const markViewedRead = () => {
  if (viewing) markRead(viewing);
};

export const initUnread = (
  userId: string,
  newest: Map<string, LatestMessage>
) => {
  me = userId;
  latest = newest;

  const stored = loadReadMarks(userId);
  read = stored ?? seedReadMarks(newest);
  if (!stored) persist();

  publish();
};

export const resetUnread = () => {
  me = null;
  latest = new Map();
  read = {};
  publish();
};

/** Records a live message; true when it is new to the user and worth a notification. */
export const recordMessage = (peer: string, message: LatestMessage) => {
  const current = latest.get(peer);
  if (current && current.createdAt >= message.createdAt) return false;

  latest = new Map(latest).set(peer, message);
  if (!message.mine && isWatching(peer)) {
    read = { ...read, [peer]: message.createdAt };
    persist();
  }
  publish();

  return isUnread(message, read[peer]);
};

const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
};

/** Peers whose newest message the user hasn't opened yet. */
export const useUnreadPeers = () =>
  useSyncExternalStore(
    subscribe,
    () => unread,
    () => unread
  );
