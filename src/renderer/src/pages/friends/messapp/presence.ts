import { useEffect, useRef, useState } from "react";

import { useAppSelector } from "@renderer/hooks";

import { messapp } from "./client";
import {
  OFFLINE,
  toFriendPresence,
  toPayload,
  type FriendPresence,
  type PlayingNow,
  type PresencePayload,
} from "./presence-core";

const SHARE_KEY = "messapp:share-playing";
const SHARE_EVENT = "messapp:share-playing-changed";

// shortcut: channels are public, so any signed-in user can join one by name; fine for
// "playing" data, move to private channels with RLS on realtime.messages if that changes.
const channelName = (userId: string) => `chimaera-presence-${userId}`;

export const getSharePlaying = () => {
  try {
    return localStorage.getItem(SHARE_KEY) !== "false";
  } catch {
    return true;
  }
};

export const setSharePlaying = (share: boolean) => {
  try {
    localStorage.setItem(SHARE_KEY, String(share));
  } catch {
    // Storage blocked: the toggle just won't persist.
  }
  window.dispatchEvent(new Event(SHARE_EVENT));
};

/** Tracks this user's online state and current game; mount once, app-wide. */
export function usePresenceBroadcast(userId: string | null) {
  const gameRunning = useAppSelector((state) => state.gameRunning.gameRunning);
  const [share, setShare] = useState(getSharePlaying);

  useEffect(() => {
    const sync = () => setShare(getSharePlaying());
    window.addEventListener(SHARE_EVENT, sync);
    return () => window.removeEventListener(SHARE_EVENT, sync);
  }, []);

  // sessionDurationInMillis grows every tick, so the start is fixed once per game.
  const started = useRef<{ id: string; at: number } | null>(null);
  if (!gameRunning) started.current = null;
  else if (started.current?.id !== gameRunning.id)
    started.current = {
      id: gameRunning.id,
      at: Date.now() - (gameRunning.sessionDurationInMillis || 0),
    };

  const playing: PlayingNow | null = gameRunning && {
    title: gameRunning.title,
    objectId: gameRunning.objectId,
    shop: gameRunning.shop,
    iconUrl: gameRunning.iconUrl,
    startedAt: started.current?.at ?? null,
  };
  // A stable primitive, so the effect re-tracks only when the game actually changes.
  const playingKey = playing ? JSON.stringify(playing) : "";

  useEffect(() => {
    if (!userId) return;

    const payload = toPayload(
      playingKey ? (JSON.parse(playingKey) as PlayingNow) : null,
      share
    );
    const channel = messapp.channel(channelName(userId));

    channel.subscribe((status) => {
      if (status === "SUBSCRIBED") void channel.track(payload);
    });

    return () => {
      void messapp.removeChannel(channel);
    };
  }, [userId, playingKey, share]);
}

/** Live presence of the given friends, keyed by their user id. */
export function usePresence(friendIds: string[]) {
  const [presence, setPresence] = useState<Map<string, FriendPresence>>(
    new Map()
  );
  const idsKey = friendIds.join(",");

  useEffect(() => {
    if (!idsKey) return;

    const channels = idsKey.split(",").map((id) => {
      const channel = messapp.channel(channelName(id));

      channel
        .on("presence", { event: "sync" }, () => {
          const entries = Object.values(channel.presenceState()).flat();
          setPresence((prev) =>
            new Map(prev).set(
              id,
              toFriendPresence(entries as Partial<PresencePayload>[])
            )
          );
        })
        .subscribe();

      return channel;
    });

    return () => {
      channels.forEach((channel) => void messapp.removeChannel(channel));
      setPresence(new Map());
    };
  }, [idsKey]);

  return (id: string) => presence.get(id) ?? OFFLINE;
}

/** The current time, refreshed every minute so playtimes tick. */
export function useMinuteClock() {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 60_000);
    return () => clearInterval(id);
  }, []);
  return now;
}
