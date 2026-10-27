import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { CommentDiscussionIcon } from "@primer/octicons-react";

import { Button, TextField } from "@renderer/components";
import { signIn, signUp } from "./messapp/api";
import { classifyAuthError } from "./messapp/errors";
import { useLastMessages } from "./messapp/use-last-messages";
import { useMessapp } from "./messapp/use-messapp";
import { usePresence } from "./messapp/presence";
import { setViewingPeer } from "./messapp/unread";
import { FriendsList } from "./friends-list";
import { ChatPanel } from "./chat-panel";
import type { FriendRow } from "./messapp/api";
import "./friends.scss";

type AuthMode = "sign_in" | "sign_up";

const AUTH_ERROR_KEYS = {
  invalid_credentials: "sign_in_failed",
  already_registered: "already_registered",
  rate_limited: "rate_limited",
  network: "network_error",
  unknown: "action_failed",
} as const;

function AuthForm() {
  const { t } = useTranslation("messapp");
  const [mode, setMode] = useState<AuthMode>("sign_in");
  const [email, setEmail] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const isSignUp = mode === "sign_up";

  const switchMode = () => {
    setMode(isSignUp ? "sign_in" : "sign_up");
    setError(null);
    setNotice(null);
  };

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError(null);
    setNotice(null);

    try {
      if (isSignUp) {
        const { needsConfirmation } = await signUp(
          email,
          password,
          username.trim()
        );
        if (needsConfirmation) {
          setNotice(t("check_email"));
          setMode("sign_in");
        }
      } else {
        await signIn(email, password);
      }
    } catch (err) {
      // Never surface the raw SDK message; map what we recognize, fall back to one calm string.
      const kind = classifyAuthError(err);
      // "Incorrect email or password" is wrong copy for a failed sign-up.
      setError(
        t(
          isSignUp && kind === "invalid_credentials"
            ? "action_failed"
            : AUTH_ERROR_KEYS[kind]
        )
      );
    } finally {
      setBusy(false);
    }
  };

  return (
    <form className="friends__sign-in" onSubmit={submit}>
      <h2 className="friends__title">
        {t(isSignUp ? "sign_up_title" : "sign_in_title")}
      </h2>
      <p className="friends__hint">
        {t(isSignUp ? "sign_up_description" : "sign_in_description")}
      </p>
      {notice && (
        <p className="friends__notice" role="status">
          {notice}
        </p>
      )}
      <TextField
        label={t("email")}
        type="email"
        autoComplete="username"
        value={email}
        onChange={(event) => setEmail(event.target.value)}
      />
      {isSignUp && (
        <TextField
          label={t("username")}
          autoComplete="nickname"
          value={username}
          onChange={(event) => setUsername(event.target.value)}
        />
      )}
      <TextField
        label={t("password")}
        type="password"
        autoComplete={isSignUp ? "new-password" : "current-password"}
        value={password}
        onChange={(event) => setPassword(event.target.value)}
        error={error}
      />
      <Button
        type="submit"
        disabled={busy || !email || !password || (isSignUp && !username.trim())}
      >
        {busy
          ? t(isSignUp ? "signing_up" : "signing_in")
          : t(isSignUp ? "sign_up" : "sign_in")}
      </Button>
      <p className="friends__no-account">
        {t(isSignUp ? "have_account" : "no_account")}{" "}
        <button
          type="button"
          className="friends__link"
          disabled={busy}
          onClick={switchMode}
        >
          {t(isSignUp ? "sign_in" : "sign_up")}
        </button>
      </p>
    </form>
  );
}

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
  if (!messappState.userId) return <AuthForm />;

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
