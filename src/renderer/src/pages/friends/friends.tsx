import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { CommentDiscussionIcon } from "@primer/octicons-react";

import { useLastMessages } from "./messapp/use-last-messages";
import { useMessapp } from "./messapp/use-messapp";
import { usePresence } from "./messapp/presence";
import { setViewingPeer } from "./messapp/unread";
import { FriendsList } from "./friends-list";
import { ChatPanel } from "./chat-panel";
import type { FriendRow } from "./messapp/api";
import "./friends.scss";

export default function Friends() {
  const { t } = useTranslation("messapp");
  const messappState = useMessapp();
  const [active, setActive] = useState<FriendRow | null>(null);
  const [activity, setActivity] = useState(0);
  // Stable so the chat's realtime subscription isn't rebuilt on every render.
  const bumpActivity = useCallback(() => setActivity((n) => n + 1), []);
  const lastMessages = useLastMessages(
    messappState.userId,
    messappState.friends,
    messappState.keyring,
    activity
  );

  const presenceOf = usePresence(
    messappState.friends
      .filter((f) => f.friendship.status === "accepted")
      .map((f) => f.profile.id)
  );

  // Signing out (from the profile modal) must not leave the last chat open for the next account.
  useEffect(() => setActive(null), [messappState.userId]);

  // Tells the inbox which chat is on screen so its messages count as read.
  useEffect(() => {
    setViewingPeer(active?.profile.id ?? null);
    return () => setViewingPeer(null);
  }, [active]);

  if (!messappState.ready) {
    return (
      <div className="friends" aria-busy="true">
        <div className="friends__list friends__skeleton" />
        <div className="friends__chat friends__skeleton" />
      </div>
    );
  }
  // Signing in lives behind the profile button above Home, the one entry point.
  if (!messappState.userId) {
    return (
      <div className="friends__placeholder">
        <CommentDiscussionIcon size={32} />
        <p className="friends__empty">{t("signed_out")}</p>
        <p className="friends__hint">{t("signed_out_hint")}</p>
      </div>
    );
  }

  return (
    <div className="friends">
      <FriendsList
        userId={messappState.userId}
        friends={messappState.friends}
        friendsStatus={messappState.friendsStatus}
        activeId={active?.profile.id ?? null}
        lastMessages={lastMessages}
        presenceOf={presenceOf}
        onSelect={setActive}
        onChanged={messappState.refreshFriends}
        onRetry={messappState.retryFriends}
      />

      <section className="friends__chat">
        {active ? (
          <ChatPanel
            key={active.profile.id}
            userId={messappState.userId}
            peer={active.profile}
            friendshipId={active.friendship.id}
            onRemoved={async () => {
              setActive(null);
              await messappState.refreshFriends();
            }}
            presence={presenceOf(active.profile.id)}
            keyring={messappState.keyring}
            onUnlock={messappState.unlock}
            onActivity={bumpActivity}
          />
        ) : (
          <div className="friends__placeholder">
            <CommentDiscussionIcon size={32} />
            <p className="friends__empty">{t("select_friend")}</p>
            <p className="friends__hint">{t("select_friend_hint")}</p>
          </div>
        )}
      </section>
    </div>
  );
}
