import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { CheckIcon, XIcon } from "@primer/octicons-react";

import { Avatar, Button, TextField } from "@renderer/components";
import {
  acceptFriendRequest,
  removeFriendship,
  searchProfiles,
  sendFriendRequest,
  type FriendRow,
  type MessappProfile,
} from "./messapp/api";
import { isNetworkError } from "./messapp/errors";
import { shownName } from "./messapp/profile-merge";
import { sortByRecency, type LastMessage } from "./messapp/recency";
import { playtimeLabel, type FriendPresence } from "./messapp/presence-core";
import { useMinuteClock } from "./messapp/presence";
import { useUnreadPeers } from "./messapp/unread";
import type { FriendsStatus } from "./messapp/use-messapp";

interface FriendsListProps {
  userId: string;
  friends: FriendRow[];
  friendsStatus: FriendsStatus;
  activeId: string | null;
  lastMessages: Map<string, LastMessage>;
  presenceOf: (id: string) => FriendPresence;
  onSelect: (friend: FriendRow) => void;
  onChanged: () => Promise<void>;
  onRetry: () => Promise<void>;
}

function PersonLabel({
  profile,
  preview,
  presence,
  unread,
}: Readonly<{
  profile: MessappProfile;
  preview?: string;
  presence?: FriendPresence;
  unread?: boolean;
}>) {
  const { t } = useTranslation("messapp");
  const now = useMinuteClock();
  const time = playtimeLabel(presence?.playing?.startedAt, now);
  const playing = presence?.playing
    ? time
      ? t("playing_time", { game: presence.playing.title, time })
      : t("playing", { game: presence.playing.title })
    : undefined;
  const subline = preview ?? playing ?? profile.status_text;

  return (
    <>
      {/* Decorative: the row's name and buttons already carry the person's name. */}
      <span className="friends__avatar">
        <Avatar size={32} src={profile.avatar_url} alt="" />
        {presence?.online && (
          <span className="friends__online" title={t("online")} />
        )}
      </span>
      <span className="friends__person">
        <span
          className={`friends__name${unread ? " friends__name--unread" : ""}`}
        >
          {shownName(profile)}
        </span>
        {subline && (
          <span
            className={`friends__status${unread ? " friends__status--unread" : ""}`}
          >
            {subline}
          </span>
        )}
      </span>
      {unread && <span className="friends__unread" title={t("unread")} />}
    </>
  );
}

export function FriendsList({
  userId,
  friends,
  friendsStatus,
  activeId,
  lastMessages,
  presenceOf,
  onSelect,
  onChanged,
  onRetry,
}: Readonly<FriendsListProps>) {
  const { t } = useTranslation("messapp");
  const unread = useUnreadPeers();
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<MessappProfile[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [searched, setSearched] = useState(false);
  const [busyKey, setBusyKey] = useState<string | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    const focusSearch = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        searchRef.current?.focus();
      }
    };

    window.addEventListener("keydown", focusSearch);
    return () => window.removeEventListener("keydown", focusSearch);
  }, []);

  const accepted = sortByRecency(
    friends.filter((f) => f.friendship.status === "accepted"),
    lastMessages
  );

  const previewFor = (friend: FriendRow) => {
    const last = lastMessages.get(friend.profile.id);
    if (!last) return undefined;

    let body = last.text;
    if (last.text === null) body = t("preview_encrypted");
    else if (last.text === "") body = t("preview_image");

    return last.mine ? t("preview_you", { text: body }) : (body ?? undefined);
  };
  const incoming = friends.filter(
    (f) =>
      f.friendship.status === "pending" && f.friendship.receiver_id === userId
  );
  const outgoing = friends.filter(
    (f) =>
      f.friendship.status === "pending" && f.friendship.sender_id === userId
  );

  // Each action refreshes the list; failures map to one of two calm messages, never raw SDK text.
  // `key` names the row so only its buttons lock while the request is in flight.
  const run = async (key: string, action: () => Promise<unknown>) => {
    if (busyKey) return;

    setBusyKey(key);
    setError(null);
    try {
      await action();
      await onChanged();
    } catch (err) {
      setError(t(isNetworkError(err) ? "network_error" : "action_failed"));
    } finally {
      setBusyKey(null);
    }
  };

  const clearSearch = () => {
    setQuery("");
    setResults([]);
    setSearched(false);
  };

  const search = async (event: React.FormEvent) => {
    event.preventDefault();
    setError(null);

    const trimmed = query.trim();
    if (!trimmed) return;

    try {
      const found = await searchProfiles(trimmed);
      setResults(found.filter((p) => p.id !== userId));
      setSearched(true);
    } catch (err) {
      setError(t(isNetworkError(err) ? "network_error" : "action_failed"));
    }
  };

  const known = new Set(friends.map((f) => f.profile.id));

  return (
    <aside className="friends__list">
      <form className="friends__search" onSubmit={search}>
        <TextField
          ref={searchRef}
          placeholder={t("search_placeholder")}
          aria-label={t("search_placeholder")}
          value={query}
          onChange={(event) => {
            setQuery(event.target.value);
            setSearched(false);
          }}
          onKeyDown={(event) => {
            if (event.key === "Escape") clearSearch();
          }}
        />
      </form>

      {/* Only this part scrolls, so search stays in reach in a long list. */}
      <div className="friends__scroll">
        {error && (
          <p className="friends__error" role="alert">
            {error}
          </p>
        )}

        {searched && results.length === 0 && (
          <p className="friends__empty">{t("no_results")}</p>
        )}

        {results.length > 0 && (
          <>
            <h3 className="friends__heading">{t("search_results")}</h3>
            <ul className="friends__group">
              {results.map((profile) => (
                <li key={profile.id} className="friends__row">
                  <PersonLabel profile={profile} />
                  {known.has(profile.id) ? (
                    <span className="friends__tag">{t("already_added")}</span>
                  ) : (
                    <Button
                      theme="outline"
                      disabled={busyKey !== null}
                      aria-label={t("add_named", { name: profile.username })}
                      onClick={() =>
                        run(`add-${profile.id}`, async () => {
                          await sendFriendRequest(userId, profile.id);
                          clearSearch();
                        })
                      }
                    >
                      {t("add_friend")}
                    </Button>
                  )}
                </li>
              ))}
            </ul>
          </>
        )}

        {incoming.length > 0 && (
          <>
            <h3 className="friends__heading">{t("requests")}</h3>
            <ul className="friends__group">
              {incoming.map(({ friendship, profile }) => (
                <li key={friendship.id} className="friends__row">
                  <PersonLabel profile={profile} />
                  <Button
                    theme="outline"
                    title={t("accept")}
                    disabled={busyKey !== null}
                    aria-label={t("accept_named", { name: profile.username })}
                    onClick={() =>
                      run(friendship.id, () =>
                        acceptFriendRequest(friendship.id)
                      )
                    }
                  >
                    <CheckIcon size={16} />
                  </Button>
                  <Button
                    theme="outline"
                    title={t("decline")}
                    disabled={busyKey !== null}
                    aria-label={t("decline_named", { name: profile.username })}
                    onClick={() =>
                      run(friendship.id, () => removeFriendship(friendship.id))
                    }
                  >
                    <XIcon size={16} />
                  </Button>
                </li>
              ))}
            </ul>
          </>
        )}

        <h3 className="friends__heading">{t("friends")}</h3>
        {accepted.length === 0 && friendsStatus === "loading" && (
          <p className="friends__empty" role="status">
            {t("loading_friends")}
          </p>
        )}
        {accepted.length === 0 && friendsStatus === "error" && (
          <div className="friends__retry" role="alert">
            <p className="friends__error">{t("network_error")}</p>
            <Button theme="outline" onClick={() => void onRetry()}>
              {t("retry")}
            </Button>
          </div>
        )}
        {accepted.length === 0 && friendsStatus === "ready" && (
          <p className="friends__empty">{t("no_friends")}</p>
        )}
        <ul className="friends__group">
          {accepted.map((friend) => (
            <li key={friend.friendship.id} className="friends__row">
              <button
                type="button"
                className={`friends__select ${
                  activeId === friend.profile.id
                    ? "friends__select--active"
                    : ""
                }`}
                aria-current={
                  activeId === friend.profile.id ? "true" : undefined
                }
                onClick={() => onSelect(friend)}
              >
                <PersonLabel
                  profile={friend.profile}
                  preview={previewFor(friend)}
                  presence={presenceOf(friend.profile.id)}
                  unread={unread.has(friend.profile.id)}
                />
              </button>
            </li>
          ))}
        </ul>

        {outgoing.length > 0 && (
          <>
            <h3 className="friends__heading">{t("sent")}</h3>
            <ul className="friends__group">
              {outgoing.map(({ friendship, profile }) => (
                <li key={friendship.id} className="friends__row">
                  <PersonLabel profile={profile} />
                  <Button
                    theme="outline"
                    disabled={busyKey !== null}
                    aria-label={t("cancel_named", { name: profile.username })}
                    onClick={() =>
                      run(friendship.id, () => removeFriendship(friendship.id))
                    }
                  >
                    {t("cancel")}
                  </Button>
                </li>
              ))}
            </ul>
          </>
        )}
      </div>
    </aside>
  );
}
