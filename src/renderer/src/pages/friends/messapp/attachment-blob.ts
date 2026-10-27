import { decryptBytes, deriveSharedKey, encryptBytes } from "./crypto.js";
import type { DmEnvelope, DmKeyring } from "./dm-crypto.js";

export const IMAGE_TYPES = [
  "image/jpeg",
  "image/png",
  "image/gif",
  "image/webp",
  "image/avif",
];

/** The shape MESSAPP uploads for a DM attachment: the AES-GCM envelope plus the original file's metadata. */
export interface AttachmentBlob extends DmEnvelope {
  type: string;
  name: string;
  size: number;
}

export const sealAttachment = async (
  keyring: DmKeyring,
  peerPublicJwk: JsonWebKey,
  bytes: ArrayBuffer,
  meta: { type: string; name: string; size: number }
): Promise<AttachmentBlob> => {
  const shared = await deriveSharedKey(keyring.privateKey, peerPublicJwk);

  return {
    ...(await encryptBytes(shared, bytes)),
    ...meta,
    spub: keyring.publicJwk,
    tpub: { ...peerPublicJwk, ext: true },
  };
};

/** Null when no key we hold opens it. `sentByMe` picks which stamped public key is the counterpart. */
export const openAttachment = async (
  keyring: DmKeyring,
  blob: AttachmentBlob,
  sentByMe: boolean,
  peerPublicJwk: JsonWebKey | null
): Promise<ArrayBuffer | null> => {
  let counterpart = peerPublicJwk;
  if (blob.spub && blob.tpub) counterpart = sentByMe ? blob.tpub : blob.spub;
  if (!counterpart) return null;

  try {
    return await decryptBytes(
      await deriveSharedKey(keyring.privateKey, counterpart),
      blob
    );
  } catch {
    return null;
  }
};

/** Storage object path inside chat-attachments, from the URL MESSAPP stores in file_url (or a bare path). */
export const attachmentObjectPath = (fileUrl: string) => {
  if (!fileUrl || /^(?:data:|blob:)/i.test(fileUrl)) return "";
  if (!/^https?:\/\//i.test(fileUrl)) return fileUrl;

  try {
    const match = new URL(fileUrl).pathname.match(
      /\/storage\/v1\/object\/(?:public|sign)\/chat-attachments\/(.+)$/
    );
    return match ? decodeURIComponent(match[1]) : "";
  } catch {
    return "";
  }
};
