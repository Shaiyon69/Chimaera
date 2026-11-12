import type { DmMember } from "./recency";

/**
 * Peer id -> DM room id for the signed-in user's one-to-one rooms, read from the
 * dm_members rows the friends list already loads. Rooms with any other member
 * count are skipped (create_or_get_dm only returns two-member rooms for a peer).
 * A pair with several such rooms is left out too: the RPC picks the oldest, which
 * these rows cannot tell, and a different pick would split the conversation.
 */
export const pairRooms = (members: DmMember[], userId: string) => {
  const byRoom = new Map<string, Set<string>>();
  for (const { dm_room_id: roomId, profile_id: profileId } of members) {
    const set = byRoom.get(roomId) ?? new Set<string>();
    set.add(profileId);
    byRoom.set(roomId, set);
  }

  const rooms = new Map<string, string>();
  const ambiguous = new Set<string>();
  for (const [roomId, profiles] of byRoom) {
    if (profiles.size !== 2 || !profiles.has(userId)) continue;
    const peer = [...profiles].find((id) => id !== userId);
    if (!peer) continue;
    if (rooms.has(peer)) ambiguous.add(peer);
    else rooms.set(peer, roomId);
  }
  ambiguous.forEach((peer) => rooms.delete(peer));
  return rooms;
};
