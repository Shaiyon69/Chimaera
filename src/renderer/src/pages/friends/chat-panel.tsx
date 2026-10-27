import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";
import {
  ImageIcon,
  KebabHorizontalIcon,
  PersonIcon,
  XIcon,
} from "@primer/octicons-react";

import {
  Avatar,
  Button,
  ConfirmationModal,
  TextField,
} from "@renderer/components";
import { DropdownMenu } from "@renderer/components/dropdown-menu/dropdown-menu";
import {
  MESSAGE_PAGE_SIZE,
  fetchMessages,
  openDm,
  removeFriendship,
  sendMessage,
  subscribeToRoom,
  type DmMessage,
  type MessappProfile,
} from "./messapp/api";
import { decryptDm, encryptDm, type DmKeyring } from "./messapp/dm-crypto";
import { classifyUnlockError, isNetworkError } from "./messapp/errors";
import { IMAGE_TYPES } from "./messapp/attachment-blob";
import { shownName } from "./messapp/profile-merge";
import type { FriendPresence } from "./messapp/presence-core";
import { dayDiff, groupLines } from "./messapp/message-groups";
import {
  TYPING_SEND_MS,
  TYPING_SHOW_MS,
  createThrottle,
} from "./messapp/typing";
import {
  ImageError,
  fetchAttachments,
  loadAttachmentImage,
  prepareImage,
  sendImageMessage,
  type MessageAttachment,
  type PreparedImage,
} from "./messapp/attachments";
import { SpoilerImage } from "./spoiler-image";

// The attachment row is inserted just after its message, so a message that arrives over realtime may need a second look.
const ATTACHMENT_RETRY_MS = 1500;

const IMAGE_ERROR_KEYS = {
  type: "image_type_unsupported",
  size: "image_too_large",
  read: "image_failed",
} as const;

// Closer than this to the bottom counts as "reading the latest", so new messages keep the view pinned.
const STICK_TO_BOTTOM_PX = 80;

const UNLOCK_ERROR_KEYS = {
  wrong_pin: "wrong_pin",
  no_backup: "no_backup",
  key_mismatch: "key_mismatch",
  network: "network_error",
  unknown: "action_failed",
} as const;

interface ChatLine {
  id: string;
  mine: boolean;
  text: string | null;
  createdAt: string;
  attachments: MessageAttachment[];
  /** Set only on text sent from here that the server hasn't confirmed. */
  status?: "sending" | "failed";
}

interface PendingImage {
  image: PreparedImage;
  previewUrl: string;
}

interface ChatPanelProps {
  userId: string;
  peer: MessappProfile;
  friendshipId: string;
  presence: FriendPresence;
  keyring: DmKeyring | null;
  onUnlock: (pin: string) => Promise<void>;
  /** Called after the friendship is deleted, so the page can close the chat and reload the list. */
  onRemoved: () => Promise<void>;
  /** Called when a message is sent or arrives, so the friends list can refresh its preview. */
  onActivity?: () => void;
}

function UnlockForm({
  onUnlock,
}: Readonly<{ onUnlock: (pin: string) => Promise<void> }>) {
  const { t } = useTranslation("messapp");
  const [pin, setPin] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError(null);

    try {
      await onUnlock(pin);
    } catch (err) {
      setError(t(UNLOCK_ERROR_KEYS[classifyUnlockError(err)]));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form className="friends__unlock" onSubmit={submit}>
      <h3 className="friends__title">{t("unlock_title")}</h3>
      <p className="friends__hint">{t("unlock_description")}</p>
      <TextField
        type="password"
        inputMode="numeric"
        autoComplete="off"
        maxLength={6}
        label={t("pin")}
        value={pin}
        onChange={(event) => setPin(event.target.value.replaceAll(/\D/g, ""))}
        error={error}
      />
      <Button type="submit" disabled={busy || pin.length !== 6}>
        {busy ? t("unlocking") : t("unlock")}
      </Button>
    </form>
  );
}

export function ChatPanel({
  userId,
  peer,
  friendshipId,
  presence,
  keyring,
  onUnlock,
  onRemoved,
  onActivity,
}: Readonly<ChatPanelProps>) {
  const { t } = useTranslation("messapp");
  const [lines, setLines] = useState<ChatLine[]>([]);
  const [roomId, setRoomId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [connected, setConnected] = useState(true);
  const [reloadKey, setReloadKey] = useState(0);
  const [hasOlder, setHasOlder] = useState(false);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [pending, setPending] = useState<PendingImage | null>(null);
  const [peerTyping, setPeerTyping] = useState(false);
  const [dragging, setDragging] = useState(false);
  const [confirmingRemove, setConfirmingRemove] = useState(false);
  const navigate = useNavigate();
  const typingTimerRef = useRef<ReturnType<typeof setTimeout>>(undefined);
  const sendTypingRef = useRef<(userId: string) => void>(() => {});
  const allowTypingRef = useRef(createThrottle(TYPING_SEND_MS));
  const fileInputRef = useRef<HTMLInputElement>(null);
  const pendingUrlRef = useRef<string | null>(null);
  const messagesRef = useRef<HTMLDivElement>(null);
  const draftRef = useRef<HTMLTextAreaElement>(null);
  const pinnedRef = useRef(true);
  // Height before older messages were prepended, so the view can hold its place.
  const prependFromRef = useRef<number | null>(null);

  const peerKey = useMemo<JsonWebKey | null>(() => {
    try {
      return peer.public_key ? JSON.parse(peer.public_key) : null;
    } catch {
      return null;
    }
  }, [peer.public_key]);

  const toLine = useCallback(
    async (
      message: DmMessage,
      attachments: MessageAttachment[] = []
    ): Promise<ChatLine> => {
      const mine = message.profile_id === userId;
      return {
        id: message.id,
        mine,
        text: keyring
          ? await decryptDm(keyring, message.content, mine, peerKey)
          : null,
        createdAt: message.created_at,
        attachments,
      };
    },
    [keyring, peerKey, userId]
  );

  // One attachments query per page of history rather than one per message.
  const toLines = useCallback(
    async (rows: DmMessage[]) => {
      const attachments = await fetchAttachments(rows.map((row) => row.id));

      return Promise.all(
        rows.map((row) =>
          toLine(
            row,
            attachments.filter((a) => a.message_id === row.id)
          )
        )
      );
    },
    [toLine]
  );

  // The sender's own insert also arrives over realtime, so appends dedupe by id.
  // A copy that carries attachments replaces one that arrived before them.
  const appendLine = useCallback(
    (line: ChatLine) =>
      setLines((current) => {
        const index = current.findIndex((l) => l.id === line.id);
        if (index === -1) return [...current, line];
        if (!line.attachments.length || current[index].attachments.length) {
          return current;
        }

        const next = [...current];
        next[index] = line;
        return next;
      }),
    []
  );

  const loadImage = useCallback(
    (attachment: MessageAttachment, mine: boolean) =>
      keyring
        ? loadAttachmentImage(attachment, keyring, mine, peerKey)
        : Promise.resolve(null),
    [keyring, peerKey]
  );

  const clearPending = useCallback(() => {
    if (pendingUrlRef.current) URL.revokeObjectURL(pendingUrlRef.current);
    pendingUrlRef.current = null;
    setPending(null);
    if (fileInputRef.current) fileInputRef.current.value = "";
  }, []);

  useEffect(() => clearPending, [clearPending]);

  useEffect(() => {
    if (!keyring) return;

    let cancelled = false;
    let unsubscribe = () => {};
    const retryTimers: ReturnType<typeof setTimeout>[] = [];

    setLoading(true);
    setError(null);
    setConnected(true);
    pinnedRef.current = true;

    (async () => {
      try {
        const room = await openDm(peer.id);
        const rows = await fetchMessages(room);
        const history = await toLines(rows);
        if (cancelled) return;

        setRoomId(room);
        setLines(history);
        setHasOlder(rows.length === MESSAGE_PAGE_SIZE);

        const channel = subscribeToRoom(
          room,
          async (message) => {
            const attachments = await fetchAttachments([message.id]).catch(
              () => []
            );
            const line = await toLine(message, attachments);
            if (cancelled) return;
            appendLine(line);
            if (!line.mine) setPeerTyping(false);
            onActivity?.();

            // An image message shows up before its attachment row does.
            if (
              !attachments.length &&
              (message.is_spoiler || line.text === "")
            ) {
              retryTimers.push(
                setTimeout(async () => {
                  const retried = await fetchAttachments([message.id]).catch(
                    () => []
                  );
                  if (!cancelled && retried.length) {
                    appendLine(await toLine(message, retried));
                  }
                }, ATTACHMENT_RETRY_MS)
              );
            }
          },
          (isConnected) => {
            if (!cancelled) setConnected(isConnected);
          },
          (typingId) => {
            if (cancelled || typingId !== peer.id) return;

            setPeerTyping(true);
            clearTimeout(typingTimerRef.current);
            typingTimerRef.current = setTimeout(
              () => setPeerTyping(false),
              TYPING_SHOW_MS
            );
          }
        );
        sendTypingRef.current = channel.sendTyping;
        unsubscribe = channel.close;
      } catch {
        if (!cancelled) setError(t("action_failed"));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();

    return () => {
      cancelled = true;
      retryTimers.forEach(clearTimeout);
      clearTimeout(typingTimerRef.current);
      sendTypingRef.current = () => {};
      setPeerTyping(false);
      unsubscribe();
    };
  }, [keyring, peer.id, toLine, toLines, appendLine, onActivity, t, reloadKey]);

  const items = useMemo(() => groupLines(lines), [lines]);

  useLayoutEffect(() => {
    const el = messagesRef.current;
    if (!el) return;

    if (prependFromRef.current !== null) {
      el.scrollTop += el.scrollHeight - prependFromRef.current;
      prependFromRef.current = null;
    } else if (pinnedRef.current) {
      el.scrollTop = el.scrollHeight;
    }
  }, [lines]);

  // Grow the composer with its content; CSS max-height caps it and scrolls beyond.
  useLayoutEffect(() => {
    const el = draftRef.current;
    if (!el) return;

    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [draft]);

  const loadOlder = async () => {
    if (!roomId || loadingOlder || lines.length === 0) return;

    setLoadingOlder(true);
    setError(null);
    try {
      const rows = await fetchMessages(roomId, lines[0].createdAt);
      const older = await toLines(rows);

      prependFromRef.current = messagesRef.current?.scrollHeight ?? null;
      setLines((current) => {
        const seen = new Set(current.map((l) => l.id));
        return [...older.filter((l) => !seen.has(l.id)), ...current];
      });
      setHasOlder(rows.length === MESSAGE_PAGE_SIZE);
    } catch (err) {
      setError(t(isNetworkError(err) ? "network_error" : "action_failed"));
    } finally {
      setLoadingOlder(false);
    }
  };

  const removeFriend = async () => {
    setConfirmingRemove(false);
    try {
      await removeFriendship(friendshipId);
      await onRemoved();
    } catch (err) {
      setError(t(isNetworkError(err) ? "network_error" : "action_failed"));
    }
  };

  // Checked first so nobody unlocks a chat that can't be read, and the friend is named either way.
  if (!peerKey) {
    return (
      <div className="friends__thread">
        <h3 className="friends__title">{shownName(peer)}</h3>
        <p className="friends__empty">{t("peer_no_key")}</p>
      </div>
    );
  }
  if (!keyring) return <UnlockForm onUnlock={onUnlock} />;

  // One path for the picker, paste and drop; prepareImage rejects anything unsupported.
  const attachFile = async (file: File) => {
    setError(null);
    try {
      const image = await prepareImage(file);
      clearPending();

      const previewUrl = URL.createObjectURL(
        new Blob([image.bytes], { type: image.type })
      );
      pendingUrlRef.current = previewUrl;
      setPending({ image, previewUrl });
    } catch (err) {
      setError(
        t(
          err instanceof ImageError
            ? IMAGE_ERROR_KEYS[err.kind]
            : "image_failed"
        )
      );
      if (fileInputRef.current) fileInputRef.current.value = "";
    }
  };

  const pickImage = (event: React.ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    if (file) void attachFile(file);
  };

  const sendText = async (room: string, tempId: string, text: string) => {
    try {
      const row = await sendMessage(
        userId,
        room,
        await encryptDm(keyring, peerKey, text)
      );
      const line = await toLine(row);

      // The realtime echo can land first, in which case the confirmed row is already listed.
      setLines((current) =>
        current.some((l) => l.id === line.id)
          ? current.filter((l) => l.id !== tempId)
          : current.map((l) => (l.id === tempId ? line : l))
      );
      onActivity?.();
    } catch {
      setLines((current) =>
        current.map((l) => (l.id === tempId ? { ...l, status: "failed" } : l))
      );
    }
  };

  const send = async (event: React.FormEvent) => {
    event.preventDefault();
    const text = draft.trim();
    if ((!text && !pending) || !roomId || sending) return;

    // Text appears at once and settles when the server confirms; images wait because the upload can fail.
    if (!pending) {
      const tempId = `local-${crypto.randomUUID()}`;
      pinnedRef.current = true;
      setLines((current) => [
        ...current,
        {
          id: tempId,
          mine: true,
          text,
          createdAt: new Date().toISOString(),
          attachments: [],
          status: "sending",
        },
      ]);
      setDraft("");
      setError(null);
      void sendText(roomId, tempId, text);
      return;
    }

    setSending(true);
    setError(null);

    try {
      const { message, attachment } = await sendImageMessage({
        userId,
        roomId,
        keyring,
        peerKey,
        image: pending.image,
        caption: text,
      });
      appendLine(await toLine(message, [attachment]));
      onActivity?.();
      clearPending();
      setDraft("");
    } catch {
      // The draft and image stay so the user can retry.
      setError(t("send_failed"));
    } finally {
      setSending(false);
    }
  };

  const retrySend = (line: ChatLine) => {
    if (!roomId || line.text === null) return;

    setLines((current) =>
      current.map((l) => (l.id === line.id ? { ...l, status: "sending" } : l))
    );
    void sendText(roomId, line.id, line.text);
  };

  const discardLine = (id: string) =>
    setLines((current) => current.filter((l) => l.id !== id));

  const dayLabel = (iso: string) => {
    const diff = dayDiff(iso);
    if (diff === 0) return t("today");
    if (diff === 1) return t("yesterday");

    const date = new Date(iso);
    return date.toLocaleDateString([], {
      weekday: "long",
      month: "long",
      day: "numeric",
      year:
        date.getFullYear() === new Date().getFullYear() ? undefined : "numeric",
    });
  };

  return (
    <div
      className={`friends__thread ${dragging ? "friends__thread--dragging" : ""}`}
      onDragOver={(event) => {
        if (roomId && event.dataTransfer.types.includes("Files")) {
          event.preventDefault();
          setDragging(true);
        }
      }}
      onDragLeave={(event) => {
        // Moving onto a child also fires leave; only clear when the pointer exits the thread.
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
          setDragging(false);
        }
      }}
      onDrop={(event) => {
        event.preventDefault();
        setDragging(false);
        const file = event.dataTransfer.files[0];
        if (roomId && file) void attachFile(file);
      }}
    >
      <header className="friends__header">
        <span className="friends__avatar">
          <Avatar size={36} src={peer.avatar_url} alt="" />
          {presence.online && (
            <span className="friends__online" title={t("online")} />
          )}
        </span>
        <div className="friends__header-main">
          <h3 className="friends__title">{shownName(peer)}</h3>
          {presence.playing ? (
            <button
              type="button"
              className="friends__playing"
              onClick={() =>
                navigate(
                  `/game/${presence.playing?.shop}/${presence.playing?.objectId}`
                )
              }
            >
              {presence.playing.iconUrl && (
                <img src={presence.playing.iconUrl} alt="" width={16} />
              )}
              <span>{t("playing", { game: presence.playing.title })}</span>
            </button>
          ) : (
            peer.status_text && (
              <span className="friends__status">{peer.status_text}</span>
            )
          )}
        </div>

        <DropdownMenu
          align="end"
          items={[
            {
              label: t("remove_friend_title"),
              icon: <PersonIcon size={16} />,
              onClick: () => setConfirmingRemove(true),
            },
          ]}
        >
          <button
            type="button"
            className="friends__menu"
            aria-label={t("more_actions")}
          >
            <KebabHorizontalIcon size={16} />
          </button>
        </DropdownMenu>
      </header>

      {dragging && (
        <div className="friends__drop" aria-hidden="true">
          {t("drop_image")}
        </div>
      )}

      <div
        ref={messagesRef}
        onScroll={(event) => {
          const el = event.currentTarget;
          pinnedRef.current =
            el.scrollHeight - el.scrollTop - el.clientHeight <
            STICK_TO_BOTTOM_PX;
        }}
        className="friends__messages"
        role="log"
        aria-live="polite"
        aria-relevant="additions"
        aria-busy={loading || loadingOlder}
        aria-label={t("conversation_with", { name: peer.username })}
      >
        {hasOlder && !loading && (
          <Button
            theme="outline"
            className="friends__older"
            disabled={loadingOlder}
            onClick={loadOlder}
          >
            {t(loadingOlder ? "loading_messages" : "load_older")}
          </Button>
        )}
        {loading && <p className="friends__empty">{t("loading_messages")}</p>}
        {!loading && !error && lines.length === 0 && (
          <p className="friends__empty">
            {t("no_messages", { name: peer.username })}
          </p>
        )}
        {items.map((item) => {
          if (item.kind === "day") {
            return (
              <div key={item.key} className="friends__day">
                {dayLabel(item.createdAt)}
              </div>
            );
          }

          const { line, startsGroup, endsGroup } = item;

          return (
            <div
              key={line.id}
              className={`friends__message ${line.mine ? "friends__message--mine" : ""} ${startsGroup ? "friends__message--starts" : ""}`}
            >
              {line.attachments.map((attachment) => (
                <SpoilerImage
                  key={attachment.id}
                  attachment={attachment}
                  mine={line.mine}
                  sender={line.mine ? t("you") : shownName(peer)}
                  load={loadImage}
                />
              ))}
              {(line.text !== "" || !line.attachments.length) && (
                <div
                  className={`friends__bubble ${line.mine ? "friends__bubble--mine" : ""} ${endsGroup ? "friends__bubble--end" : ""}`}
                >
                  <span className="friends__sr-only">
                    {line.mine ? t("you") : peer.username}:{" "}
                  </span>
                  {line.text ?? (
                    <span className="friends__bubble-unreadable">
                      {t("unreadable")}
                    </span>
                  )}
                </div>
              )}
              {line.status === "failed" ? (
                <span className="friends__send-state" role="alert">
                  <span className="friends__send-failed">
                    {t("send_failed")}
                  </span>
                  <button
                    type="button"
                    className="friends__link"
                    onClick={() => retrySend(line)}
                  >
                    {t("retry")}
                  </button>
                  <button
                    type="button"
                    className="friends__link"
                    onClick={() => discardLine(line.id)}
                  >
                    {t("discard")}
                  </button>
                </span>
              ) : line.status === "sending" ? (
                <span className="friends__time">{t("sending")}</span>
              ) : (
                endsGroup && (
                  <time
                    className="friends__time"
                    dateTime={line.createdAt}
                    title={new Date(line.createdAt).toLocaleString()}
                  >
                    {new Date(line.createdAt).toLocaleTimeString([], {
                      hour: "numeric",
                      minute: "2-digit",
                    })}
                  </time>
                )
              )}
            </div>
          );
        })}
      </div>

      {/* Always rendered so the line appearing never shifts the composer. */}
      <p className="friends__typing" role="status">
        {peerTyping ? t("typing", { name: shownName(peer) }) : ""}
      </p>

      {!connected && (
        <p className="friends__hint" role="status">
          {t("reconnecting")}
        </p>
      )}

      {error && (
        <div className="friends__retry" role="alert">
          <p className="friends__error">{error}</p>
          {!roomId && (
            <Button
              theme="outline"
              onClick={() => setReloadKey((key) => key + 1)}
            >
              {t("retry")}
            </Button>
          )}
        </div>
      )}

      {pending && (
        <div className="friends__pending">
          {/* Blurred like every chat image; the preview is never shown clear. */}
          <img
            src={pending.previewUrl}
            alt=""
            draggable={false}
            className="friends__pending-image"
          />
          <span className="friends__name">{pending.image.name}</span>
          <Button
            theme="outline"
            aria-label={t("remove_image")}
            disabled={sending}
            onClick={clearPending}
          >
            <XIcon size={16} />
          </Button>
        </div>
      )}

      <form className="friends__composer" onSubmit={send}>
        <input
          ref={fileInputRef}
          type="file"
          hidden
          accept={IMAGE_TYPES.join(",")}
          onChange={pickImage}
        />
        <Button
          theme="outline"
          aria-label={t("attach_image")}
          disabled={sending || !roomId}
          onClick={() => fileInputRef.current?.click()}
        >
          <ImageIcon size={16} />
        </Button>
        <textarea
          ref={draftRef}
          className="friends__input"
          rows={1}
          aria-label={t("message_placeholder")}
          placeholder={t("message_placeholder")}
          value={draft}
          onChange={(event) => {
            setDraft(event.target.value);
            if (event.target.value.trim() && allowTypingRef.current()) {
              sendTypingRef.current(userId);
            }
          }}
          onPaste={(event) => {
            const file = event.clipboardData.files[0];
            if (roomId && file) {
              event.preventDefault();
              void attachFile(file);
            }
          }}
          onKeyDown={(event) => {
            // Enter sends, Shift+Enter breaks the line; skip while an IME is composing.
            if (
              event.key === "Enter" &&
              !event.shiftKey &&
              !event.nativeEvent.isComposing
            ) {
              event.preventDefault();
              event.currentTarget.form?.requestSubmit();
            }
          }}
        />
        <Button
          type="submit"
          disabled={(!draft.trim() && !pending) || !roomId || sending}
        >
          {t("send")}
        </Button>
      </form>

      <ConfirmationModal
        visible={confirmingRemove}
        title={t("remove_friend_title")}
        descriptionText={t("remove_friend_confirm", { name: peer.username })}
        onClose={() => setConfirmingRemove(false)}
        onConfirm={removeFriend}
        cancelButtonLabel={t("cancel")}
        confirmButtonLabel={t("remove")}
        confirmButtonTheme="danger"
      />
    </div>
  );
}
