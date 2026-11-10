export interface PlayingNow {
  title: string;
  objectId: string;
  shop: string;
  iconUrl: string | null;
  /** Epoch ms the game started; lets friends show a live playtime. */
  startedAt?: number | null;
}

export interface PresencePayload {
  playing: PlayingNow | null;
  since: number;
}

export interface FriendPresence {
  online: boolean;
  playing: PlayingNow | null;
}

export const OFFLINE: FriendPresence = { online: false, playing: null };

const isPlaying = (value: unknown): value is PlayingNow => {
  const v = value as PlayingNow | null;
  return !!v && typeof v.title === "string" && typeof v.objectId === "string";
};

const validStart = (value: unknown, now: number) =>
  typeof value === "number" &&
  Number.isFinite(value) &&
  value > 0 &&
  value <= now
    ? value
    : null;

/** Reads a friend's channel state; any client may track junk, so each entry is validated. */
export const toFriendPresence = (
  entries: readonly Partial<PresencePayload>[],
  now: number = Date.now()
): FriendPresence => {
  if (!entries.length) return OFFLINE;

  // One user can have several windows open; the newest entry wins.
  const latest = entries.reduce((a, b) =>
    (b.since ?? 0) > (a.since ?? 0) ? b : a
  );

  return {
    online: true,
    playing: isPlaying(latest.playing)
      ? {
          ...latest.playing,
          // Small clock skew between PCs must not drop the time.
          startedAt: validStart(latest.playing.startedAt, now + 60_000),
        }
      : null,
  };
};

/** "1h 05m" / "12m"; null under a minute or without a start time. */
export const playtimeLabel = (
  startedAt: number | null | undefined,
  now: number = Date.now()
) => {
  if (!startedAt) return null;
  const minutes = Math.floor(Math.max(0, now - startedAt) / 60_000);
  if (minutes < 1) return null;
  const hours = Math.floor(minutes / 60);
  return hours
    ? `${hours}h ${String(minutes % 60).padStart(2, "0")}m`
    : `${minutes}m`;
};

/** What this user tracks; with sharing off only "online" is revealed. */
export const toPayload = (
  playing: PlayingNow | null,
  share: boolean,
  now: number = Date.now()
): PresencePayload => ({ playing: share ? playing : null, since: now });
