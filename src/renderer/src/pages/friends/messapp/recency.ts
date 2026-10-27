export interface DmMember {
  dm_room_id: string;
  profile_id: string;
}

export interface LastMessage {
  createdAt: string;
  mine: boolean;
  /** Decrypted text; "" for an image, null when this device can't read it. */
  text: string | null;
}

/** peer id → room id, for rooms holding exactly me and one other person. */
export const dmPeerRooms = (members: DmMember[], myId: string) => {
  const byRoom = new Map<string, string[]>();
  for (const m of members) {
    byRoom.set(m.dm_room_id, [
      ...(byRoom.get(m.dm_room_id) ?? []),
      m.profile_id,
    ]);
  }

  const peers = new Map<string, string>();
  for (const [room, ids] of byRoom) {
    const others = ids.filter((id) => id !== myId);
    if (ids.includes(myId) && others.length === 1) peers.set(others[0], room);
  }
  return peers;
};

/** Newest conversation first; friends never messaged keep their relative order after those who were. */
export const sortByRecency = <T extends { profile: { id: string } }>(
  rows: T[],
  last: Map<string, LastMessage>
) =>
  rows.toSorted((a, b) => {
    const at = last.get(a.profile.id)?.createdAt;
    const bt = last.get(b.profile.id)?.createdAt;
    if (at && bt) return bt.localeCompare(at);
    if (at) return -1;
    if (bt) return 1;
    return 0;
  });
