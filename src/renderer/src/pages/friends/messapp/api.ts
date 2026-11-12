import { logger } from "@renderer/logger";

import { messapp } from "./client";
import { fetchChimaeraProfiles } from "./profile";
import { applyChimaeraProfile, type ChimaeraProfile } from "./profile-merge";
import { unwrapPrivateKey } from "./crypto";
import { createKeyring, type DmKeyring } from "./dm-crypto";
import type { DmMember } from "./recency";
import { pairRooms } from "./dm-rooms-core";
import { parseMessageInsert } from "./realtime-core";

export interface MessappProfile {
  id: string;
  username: string;
  unique_tag: string | null;
  public_key: string | null;
  /** Chimaera-only profile fields, filled by withChimaeraProfiles. */
  avatar_url?: string | null;
  status_text?: string | null;
  display_name?: string | null;
  bio?: string | null;
  banner_url?: string | null;
}

export interface Friendship {
  id: string;
  sender_id: string;
  receiver_id: string;
  status: string;
}

export interface FriendRow {
  friendship: Friendship;
  profile: MessappProfile;
}

export interface DmMessage {
  id: string;
  profile_id: string;
  content: string;
  created_at: string;
  is_spoiler: boolean;
  origin: string;
}

/** Chimaera's own slice of a DM; MESSAPP tags what it sends as 'messapp'. */
export const ORIGIN = "chimaera";
const MESSAGE_COLUMNS =
  "id, profile_id, content, created_at, is_spoiler, origin";

const PROFILE_COLUMNS = "id, username, unique_tag, public_key";

const orThrow = <T>({
  data,
  error,
}: {
  data: T | null;
  error: { message: string } | null;
}) => {
  if (error) throw new Error(error.message);
  return data as T;
};

export const signIn = async (email: string, password: string) => {
  const { error } = await messapp.auth.signInWithPassword({ email, password });
  if (error) throw new Error(error.message);
};

// The on_auth_user_created trigger copies `username` into profiles.
export const signUp = async (
  email: string,
  password: string,
  username: string
) => {
  const { data, error } = await messapp.auth.signUp({
    email,
    password,
    options: { data: { username } },
  });
  if (error) throw new Error(error.message);

  return { needsConfirmation: !data.session };
};

export const signOut = () => messapp.auth.signOut();

// A missing or failing profile table must not hide people; they just show bare.
const withChimaeraProfiles = async (
  profiles: MessappProfile[]
): Promise<MessappProfile[]> => {
  const overrides = await fetchChimaeraProfiles(
    profiles.map((p) => p.id)
  ).catch((err) => {
    logger.error("Failed to load Chimaera profiles", err);
    return new Map<string, ChimaeraProfile>();
  });
  return profiles.map((p) => applyChimaeraProfile(p, overrides.get(p.id)));
};

export const fetchFriends = async (myId: string): Promise<FriendRow[]> => {
  const friendships = orThrow<Friendship[]>(
    await messapp
      .from("friendships")
      .select("id, sender_id, receiver_id, status")
  );
  if (!friendships.length) return [];

  const otherId = (f: Friendship) =>
    f.sender_id === myId ? f.receiver_id : f.sender_id;

  const profiles = orThrow<MessappProfile[]>(
    await messapp
      .from("profiles")
      .select(PROFILE_COLUMNS)
      .in("id", [...new Set(friendships.map(otherId))])
  );
  const byId = new Map(
    (await withChimaeraProfiles(profiles)).map((p) => [p.id, p])
  );

  return friendships.flatMap((friendship) => {
    const profile = byId.get(otherId(friendship));
    return profile ? [{ friendship, profile }] : [];
  });
};

export const searchProfiles = async (query: string) =>
  withChimaeraProfiles(
    orThrow<MessappProfile[]>(
      await messapp.rpc("search_profiles_for_friend", { search_query: query })
    )
  );

export const sendFriendRequest = async (myId: string, targetId: string) =>
  orThrow(
    await messapp
      .from("friendships")
      .insert({ sender_id: myId, receiver_id: targetId })
  );

export const acceptFriendRequest = async (friendshipId: string) =>
  orThrow(
    await messapp
      .from("friendships")
      .update({ status: "accepted" })
      .eq("id", friendshipId)
  );

export const removeFriendship = async (friendshipId: string) =>
  orThrow(await messapp.from("friendships").delete().eq("id", friendshipId));

/* A pair's room never changes, so opening a chat skips the create_or_get_dm round
   trip once the room is known: seeded from fetchDmMembers, filled by the RPC.
   Keyed by the signed-in user so another account never reuses these rooms. */
const knownRooms = new Map<string, string>();
const signedInUserId = async () =>
  (await messapp.auth.getSession()).data.session?.user.id ?? "";

export const openDm = async (peerId: string) => {
  const key = `${await signedInUserId()}:${peerId}`;
  const known = knownRooms.get(key);
  if (known) return known;
  const room = orThrow<{ id: string }>(
    await messapp.rpc("create_or_get_dm", { peer_id: peerId })
  ).id;
  knownRooms.set(key, room);
  return room;
};

export const MESSAGE_PAGE_SIZE = 100;

/** One page of history, oldest first. `before` pages back from a message's created_at. */
export const fetchMessages = async (roomId: string, before?: string) => {
  let query = messapp
    .from("messages")
    .select(MESSAGE_COLUMNS)
    .eq("dm_room_id", roomId)
    .eq("origin", ORIGIN)
    .eq("is_deleted", false);
  if (before) query = query.lt("created_at", before);

  const rows = orThrow<DmMessage[]>(
    await query
      .order("created_at", { ascending: false })
      .limit(MESSAGE_PAGE_SIZE)
  );

  return rows.toReversed();
};

export const sendMessage = async (
  myId: string,
  roomId: string,
  content: string,
  isSpoiler = false
) =>
  orThrow<DmMessage>(
    await messapp
      .from("messages")
      .insert({
        profile_id: myId,
        dm_room_id: roomId,
        content,
        is_encrypted: true,
        is_spoiler: isSpoiler,
        origin: ORIGIN,
      })
      .select(MESSAGE_COLUMNS)
      .single()
  );

/** Rooms the signed-in user belongs to, with every member; RLS hides other people's rooms. */
export const fetchDmMembers = async () => {
  const members = orThrow<DmMember[]>(
    await messapp.from("dm_members").select("dm_room_id, profile_id")
  );
  const userId = await signedInUserId();
  if (userId) {
    for (const [peer, room] of pairRooms(members, userId)) {
      if (!knownRooms.has(`${userId}:${peer}`))
        knownRooms.set(`${userId}:${peer}`, room);
    }
  }
  return members;
};

export const fetchLastMessage = async (roomId: string) =>
  orThrow<DmMessage[]>(
    await messapp
      .from("messages")
      .select(MESSAGE_COLUMNS)
      .eq("dm_room_id", roomId)
      .eq("origin", ORIGIN)
      .eq("is_deleted", false)
      .order("created_at", { ascending: false })
      .limit(1)
  )[0] ?? null;

/** One Chimaera message by id, or null when it is another app's, deleted, or hidden by RLS. */
const fetchMessageById = async (id: string) =>
  orThrow<(DmMessage & { dm_room_id: string })[]>(
    await messapp
      .from("messages")
      .select(`${MESSAGE_COLUMNS}, dm_room_id`)
      .eq("id", id)
      .eq("origin", ORIGIN)
      .eq("is_deleted", false)
      .limit(1)
  )[0] ?? null;

/* Live updates come from MESSAPP's broadcast triggers (20261103155000), not
   postgres_changes: any postgres_changes subscriber keeps Realtime polling the
   WAL, which drains the project's Disk IO budget. Broadcasts carry ids only. */
const onInsert =
  (handle: (message: DmMessage & { dm_room_id: string }) => void) =>
  ({ payload }: { payload: unknown }) => {
    const insert = parseMessageInsert(payload);
    if (!insert) return;
    fetchMessageById(insert.id)
      .then((message) => {
        if (message) handle(message);
      })
      .catch((err) => logger.error("Failed to fetch new DM", err));
  };

export const subscribeToRoom = (
  roomId: string,
  onMessage: (message: DmMessage) => void,
  onStatus: (connected: boolean) => void,
  onTyping: (userId: string) => void
) => {
  let typingReady = false;

  const messages = messapp
    .channel(`room:${roomId}`, { config: { private: true } })
    .on("broadcast", { event: "messages" }, onInsert(onMessage))
    .subscribe((status) => onStatus(status === "SUBSCRIBED"));

  // Typing stays on its own channel so MESSAPP's room topic never sees it; nothing is stored.
  const typing = messapp
    .channel(`chimaera-dm-${roomId}`)
    .on("broadcast", { event: "typing" }, ({ payload }) => {
      if (typeof payload?.userId === "string") onTyping(payload.userId);
    })
    .subscribe((status) => {
      typingReady = status === "SUBSCRIBED";
    });

  return {
    // Before the channel is joined a send would fall back to REST, so skip it.
    sendTyping: (userId: string) => {
      if (!typingReady) return;
      void typing.send({
        type: "broadcast",
        event: "typing",
        payload: { userId },
      });
    },
    close: () => {
      void messapp.removeChannel(messages);
      void messapp.removeChannel(typing);
    },
  };
};

/** Every new Chimaera message in any of my rooms, for unread tracking. */
export const subscribeToInbox = (
  userId: string,
  onMessage: (message: DmMessage & { dm_room_id: string }) => void
) => {
  const channel = messapp
    .channel(`user:${userId}`, { config: { private: true } })
    .on("broadcast", { event: "dm_activity" }, onInsert(onMessage))
    .subscribe();

  return () => {
    void messapp.removeChannel(channel);
  };
};

/**
 * Opens the PIN-wrapped key backup. Read-only on purpose: a wrong or
 * regenerated key written back would lock users out of MESSAPP history.
 */
export const unlockKeyring = async (
  pin: string
): Promise<{ keyring: DmKeyring; privateJwk: string }> => {
  const secrets = orThrow<
    { encrypted_private_key: string | null; public_key: string | null }[]
  >(await messapp.rpc("get_my_profile_secrets"))[0];

  if (!secrets?.encrypted_private_key) {
    throw new Error("No key backup. Set a PIN in MESSAPP first.");
  }

  const jwk = await unwrapPrivateKey(pin, secrets.encrypted_private_key);
  const keyring = await createKeyring(jwk);

  // A different public half would mean decrypting with someone else's key.
  if (secrets.public_key) {
    const published = JSON.parse(secrets.public_key);
    if (published.x !== keyring.publicJwk.x) {
      throw new Error("Key backup does not match your published key.");
    }
  }

  return { keyring, privateJwk: JSON.stringify(jwk) };
};
