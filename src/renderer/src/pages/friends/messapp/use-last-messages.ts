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
  const [last, setLast] = useState<Map<string, LastMessage>>(new Map());

  const accepted = friends.filter((f) => f.friendship.status === "accepted");
  const signature = accepted.map((f) => f.profile.id).join(",");

  useEffect(() => {
    if (!userId || !signature) return;

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
        setLast(new Map(entries.flatMap((entry) => (entry ? [entry] : []))));
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
