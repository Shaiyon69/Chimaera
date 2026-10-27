export interface PlayingNow {
  title: string;
  objectId: string;
  shop: string;
  iconUrl: string | null;
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

/** Reads a friend's channel state; any client may track junk, so each entry is validated. */
export const toFriendPresence = (
  entries: readonly Partial<PresencePayload>[]
): FriendPresence => {
  if (!entries.length) return OFFLINE;

  // One user can have several windows open; the newest entry wins.
  const latest = entries.reduce((a, b) =>
    (b.since ?? 0) > (a.since ?? 0) ? b : a
  );

  return {
    online: true,
    playing: isPlaying(latest.playing) ? latest.playing : null,
  };
};

/** What this user tracks; with sharing off only "online" is revealed. */
export const toPayload = (
  playing: PlayingNow | null,
  share: boolean,
  now: number = Date.now()
): PresencePayload => ({ playing: share ? playing : null, since: now });
