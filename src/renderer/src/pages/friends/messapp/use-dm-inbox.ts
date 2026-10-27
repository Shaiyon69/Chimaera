import { useEffect, useRef } from "react";
import { useNavigate } from "react-router-dom";
import { useTranslation } from "react-i18next";

import { logger } from "@renderer/logger";

import {
  fetchDmMembers,
  fetchFriends,
  fetchLastMessage,
  subscribeToInbox,
} from "./api";
import { shownName } from "./profile-merge";
import { dmPeerRooms } from "./recency";
import { useMessappUserId } from "./use-messapp";
import {
  initUnread,
  markViewedRead,
  recordMessage,
  resetUnread,
} from "./unread";
import type { LatestMessage } from "./unread-core";

/**
 * Keeps unread state current app-wide and raises a toast for new DMs.
 * The toast names the sender only: messages are end-to-end encrypted and the
 * key may not be loaded here.
 */
export function useDmInbox() {
  const userId = useMessappUserId();
  const navigate = useNavigate();
  const { t } = useTranslation("messapp");

  // The subscription outlives renders; it reads these through a ref.
  const latestRef = useRef({ navigate, t });
  useEffect(() => {
    latestRef.current = { navigate, t };
  });

  useEffect(() => {
    if (!userId) {
      resetUnread();
      return;
    }

    let cancelled = false;
    let unsubscribe: (() => void) | undefined;
    // room id → peer id, and peer id → display name, for accepted friends.
    let peerOfRoom = new Map<string, string>();
    let names = new Map<string, string>();

    const loadRooms = async () => {
      const [members, friends] = await Promise.all([
        fetchDmMembers(),
        fetchFriends(userId),
      ]);
      const roomOfPeer = dmPeerRooms(members, userId);

      peerOfRoom = new Map();
      names = new Map();
      for (const { friendship, profile } of friends) {
        const room = roomOfPeer.get(profile.id);
        if (friendship.status !== "accepted" || !room) continue;
        peerOfRoom.set(room, profile.id);
        names.set(profile.id, shownName(profile));
      }
    };

    const notify = (peer: string) => {
      const { navigate: go, t: translate } = latestRef.current;
      try {
        const toast = new Notification(names.get(peer) ?? "", {
          body: translate("new_message"),
        });
        toast.onclick = () => {
          window.focus();
          go("/friends");
        };
      } catch (err) {
        logger.error("Failed to show message notification", err);
      }
    };

    (async () => {
      await loadRooms();

      const newest = new Map<string, LatestMessage>();
      await Promise.all(
        [...peerOfRoom].map(async ([room, peer]) => {
          const message = await fetchLastMessage(room);
          if (message) {
            newest.set(peer, {
              createdAt: message.created_at,
              mine: message.profile_id === userId,
            });
          }
        })
      );
      if (cancelled) return;
      initUnread(userId, newest);

      unsubscribe = subscribeToInbox(async (message) => {
        // An unknown room is usually a friend accepted since the last load.
        if (!peerOfRoom.has(message.dm_room_id)) {
          await loadRooms().catch((err) =>
            logger.error("Failed to refresh DM rooms", err)
          );
        }
        const peer = peerOfRoom.get(message.dm_room_id);
        if (cancelled || !peer) return;

        const isNew = recordMessage(peer, {
          createdAt: message.created_at,
          mine: message.profile_id === userId,
        });
        if (isNew) notify(peer);
      });
    })().catch((err) => logger.error("Failed to start DM inbox", err));

    window.addEventListener("focus", markViewedRead);

    return () => {
      cancelled = true;
      unsubscribe?.();
      window.removeEventListener("focus", markViewedRead);
    };
  }, [userId]);
}
