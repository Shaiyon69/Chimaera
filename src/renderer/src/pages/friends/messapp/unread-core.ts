export interface LatestMessage {
  createdAt: string;
  mine: boolean;
}

/** peer id → created_at of the newest message already seen. */
export type ReadMarks = Record<string, string>;

/**
 * Both sides are server timestamps, so a skewed device clock can't hide or
 * invent unread messages.
 */
export const isUnread = (
  latest: LatestMessage | undefined,
  readAt: string | undefined
) => !!latest && !latest.mine && (!readAt || latest.createdAt > readAt);

export const unreadPeers = (
  latest: Map<string, LatestMessage>,
  read: ReadMarks
): ReadonlySet<string> =>
  new Set(
    [...latest]
      .filter(([peer, message]) => isUnread(message, read[peer]))
      .map(([peer]) => peer)
  );

/** Everything already in the inbox counts as read the first time, so old chats don't light up. */
export const seedReadMarks = (latest: Map<string, LatestMessage>): ReadMarks =>
  Object.fromEntries([...latest].map(([peer, m]) => [peer, m.createdAt]));
