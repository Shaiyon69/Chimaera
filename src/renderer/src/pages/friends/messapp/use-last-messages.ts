import { useEffect, useState } from "react";

import { logger } from "@renderer/logger";

import { fetchDmMembers, fetchLastMessage, type FriendRow } from "./api";
import { decryptDm, type DmKeyring } from "./dm-crypto";
import { dmPeerRooms, type LastMessage } from "./recency";

const parseKey = (raw: string | null): JsonWebKey | null => {
  try {
    return raw ? JSON.parse(raw) : null;
  } catch {
    return null;
  }
};

// Who was messaged when, kept between sessions so the list opens in its real order instead of
// reshuffling once messages load. Only peer ids and times: never message text.
const orderKey = (userId: string) => `messapp-dm-order:${userId}`;

const readOrder = (userId: string | null) => {
  const order = new Map<string, LastMessage>();
  if (!userId) return order;
  try {
    const raw = JSON.parse(localStorage.getItem(orderKey(userId)) ?? "{}");
    for (const [peer, createdAt] of Object.entries(raw)) {
      if (typeof createdAt === "string") {
        order.set(peer, { createdAt, mine: false, text: null, cached: true });
      }
    }
  } catch {
    // No remembered order: the list sorts once messages load, as before.
  }
  return order;
};

const writeOrder = (userId: string, last: Map<string, LastMessage>) => {
  try {
    localStorage.setItem(
      orderKey(userId),
      JSON.stringify(
        Object.fromEntries([...last].map(([peer, m]) => [peer, m.createdAt]))
      )
    );
  } catch {
    // Storage off: only the first paint is affected.
  }
};

/**
 * The newest message with each accepted friend, for list order and previews.
 * `refreshKey` re-reads after activity in the open chat; conversations that
 * aren't open have no subscription, so they catch up on the next load.
 */
export function useLastMessages(
  userId: string | null,
  friends: FriendRow[],
  keyring: DmKeyring | null,
  refreshKey: number
) {
  const [last, setLast] = useState<Map<string, LastMessage>>(() =>
    readOrder(userId)
  );

  const accepted = friends.filter((f) => f.friendship.status === "accepted");
  const signature = accepted.map((f) => f.profile.id).join(",");

  useEffect(() => {
    if (!userId || !signature) return;
    // Sign-in can finish after the first render; seed the remembered order once it does.
    setLast((prev) => (prev.size > 0 ? prev : readOrder(userId)));

    let cancelled = false;

    (async () => {
      const rooms = dmPeerRooms(await fetchDmMembers(), userId);

      const entries = await Promise.all(
        accepted.flatMap((friend) => {
          const room = rooms.get(friend.profile.id);
          if (!room) return [];

          return [
            fetchLastMessage(room).then(async (message) => {
              if (!message) return null;

              const mine = message.profile_id === userId;
              const text = keyring
                ? await decryptDm(
                    keyring,
                    message.content,
                    mine,
                    parseKey(friend.profile.public_key)
                  )
                : null;
              return [
                friend.profile.id,
                { createdAt: message.created_at, mine, text },
              ] as const;
            }),
          ];
        })
      );

      if (!cancelled) {
        const fresh = new Map(
          entries.flatMap((entry) => (entry ? [entry] : []))
        );
        setLast(fresh);
        writeOrder(userId, fresh);
      }
    })().catch((err) => logger.error("Failed to load last messages", err));

    return () => {
      cancelled = true;
    };
    // `accepted` is derived from `signature`; listing it would refetch on every render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [userId, signature, keyring, refreshKey]);

  return last;
}
