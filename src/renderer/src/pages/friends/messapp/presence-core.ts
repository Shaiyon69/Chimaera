export interface PlayingNow {
  title: string;
  objectId: string;
  shop: string;
  iconUrl: string | null;
  /** Epoch ms the game started; lets friends show a live playtime. */
  startedAt?: number | null;
  /** "#rrggbb" from the cover, which friends cannot always read themselves (CORS, local files). */
  accent?: string | null;
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

/** The cover's vivid colour as "#rrggbb" from RGBA bytes, or null for grey or dark images. */
export const accentHex = (data: ArrayLike<number>): string | null => {
  let r = 0;
  let g = 0;
  let b = 0;
  let weight = 0;
  for (let i = 0; i + 3 < data.length; i += 4) {
    if (data[i + 3] < 128) continue;
    const max = Math.max(data[i], data[i + 1], data[i + 2]);
    if (!max) continue;
    const sat = (max - Math.min(data[i], data[i + 1], data[i + 2])) / max;
    // Vivid, bright pixels pick the colour; greys and shadows barely count.
    const w = sat * sat * (max / 255);
    r += data[i] * w;
    g += data[i + 1] * w;
    b += data[i + 2] * w;
    weight += w;
  }
  if (weight < 0.5) return null;
  const hex = (v: number) =>
    Math.round(v / weight)
      .toString(16)
      .padStart(2, "0");
  return `#${hex(r)}${hex(g)}${hex(b)}`;
};
