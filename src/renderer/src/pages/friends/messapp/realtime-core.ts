/** A new DM row as announced by the broadcast triggers (ids only, no content). */
export interface MessageInsert {
  id: string;
  roomId: string;
}

/**
 * Reads the payload MESSAPP's triggers broadcast on `room:<id>` (`messages`)
 * and `user:<id>` (`dm_activity`): `{ type, id, profile_id, dm_room_id, ... }`.
 * Only inserts into a DM room matter; everything else is ignored.
 */
export const parseMessageInsert = (payload: unknown): MessageInsert | null => {
  if (!payload || typeof payload !== "object") return null;
  const row = payload as Record<string, unknown>;
  if (row.type !== "INSERT") return null;
  if (typeof row.id !== "string" || typeof row.dm_room_id !== "string") {
    return null;
  }
  return { id: row.id, roomId: row.dm_room_id };
};
