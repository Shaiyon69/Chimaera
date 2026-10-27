import { messapp } from "./client";
import { sendMessage, type DmMessage } from "./api";
import {
  IMAGE_TYPES,
  attachmentObjectPath,
  openAttachment,
  sealAttachment,
  type AttachmentBlob,
} from "./attachment-blob";
import { encryptDm, type DmKeyring } from "./dm-crypto";

const BUCKET = "chat-attachments";
const MAX_DIMENSION = 1920;
const MAX_IMAGE_BYTES = 10 * 1024 * 1024;
// Anything bigger is refused before it is read or decoded; resizing can shrink a photo, not a 100 MB file.
const MAX_SOURCE_BYTES = 30 * 1024 * 1024;
const SIGNED_URL_TTL_SECONDS = 3600;

export interface MessageAttachment {
  id: string;
  message_id: string;
  file_url: string;
  file_type: string;
  file_name: string;
  file_size: number;
  is_spoiler: boolean;
}

export interface PreparedImage {
  bytes: ArrayBuffer;
  type: string;
  name: string;
  size: number;
}

export type ImageErrorKind = "type" | "size" | "read";

export class ImageError extends Error {
  constructor(readonly kind: ImageErrorKind) {
    super(`Image rejected: ${kind}`);
  }
}

const canvasBlob = (canvas: HTMLCanvasElement, type: string) =>
  new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, type, 0.85));

/** Validates the file and shrinks anything larger than 1920px; GIFs stay as they are so they keep animating. */
export const prepareImage = async (file: File): Promise<PreparedImage> => {
  const type = file.type.split(";", 1)[0].trim().toLowerCase();
  if (!IMAGE_TYPES.includes(type)) throw new ImageError("type");
  // A GIF is sent as it is, so its source size is its final size.
  if (file.size > (type === "image/gif" ? MAX_IMAGE_BYTES : MAX_SOURCE_BYTES)) {
    throw new ImageError("size");
  }

  let bytes: ArrayBuffer;
  let outType = type;

  try {
    bytes = await file.arrayBuffer();

    if (type !== "image/gif") {
      const bitmap = await createImageBitmap(file);
      const scale = MAX_DIMENSION / Math.max(bitmap.width, bitmap.height);

      if (scale < 1) {
        const canvas = document.createElement("canvas");
        canvas.width = Math.round(bitmap.width * scale);
        canvas.height = Math.round(bitmap.height * scale);
        canvas
          .getContext("2d")
          ?.drawImage(bitmap, 0, 0, canvas.width, canvas.height);

        // Canvas can't encode AVIF, so that one comes back as WebP.
        outType = type === "image/png" ? "image/png" : "image/webp";
        const resized = await canvasBlob(canvas, outType);
        if (!resized) throw new ImageError("read");
        bytes = await resized.arrayBuffer();
      }
      bitmap.close();
    }
  } catch (err) {
    if (err instanceof ImageError) throw err;
    throw new ImageError("read");
  }

  if (bytes.byteLength > MAX_IMAGE_BYTES) throw new ImageError("size");

  return {
    bytes,
    type: outType,
    name: file.name.slice(0, 160) || "image",
    size: bytes.byteLength,
  };
};

interface SendImageArgs {
  userId: string;
  roomId: string;
  keyring: DmKeyring;
  peerKey: JsonWebKey;
  image: PreparedImage;
  caption: string;
}

/**
 * Same order as MESSAPP: upload, then the message row, then the attachment row.
 * Images are always flagged as spoilers so they arrive blurred in both apps.
 */
export const sendImageMessage = async ({
  userId,
  roomId,
  keyring,
  peerKey,
  image,
  caption,
}: SendImageArgs): Promise<{
  message: DmMessage;
  attachment: MessageAttachment;
}> => {
  const path = `${userId}/${roomId}/${crypto.randomUUID()}.json`;
  const blob = await sealAttachment(keyring, peerKey, image.bytes, {
    type: image.type,
    name: image.name,
    size: image.size,
  });

  const { error: uploadError } = await messapp.storage
    .from(BUCKET)
    .upload(
      path,
      new Blob([JSON.stringify(blob)], { type: "application/json" }),
      {
        contentType: "application/json",
        upsert: false,
      }
    );
  if (uploadError) throw new Error(uploadError.message);

  let message: DmMessage;
  try {
    message = await sendMessage(
      userId,
      roomId,
      await encryptDm(keyring, peerKey, caption),
      true
    );
  } catch (err) {
    // The upload is useless without its message; a retry uploads under a new name.
    await messapp.storage.from(BUCKET).remove([path]);
    throw err;
  }

  const { data, error } = await messapp
    .from("message_attachments")
    .insert({
      message_id: message.id,
      // MESSAPP readers pull the object path out of this URL and sign it themselves.
      file_url: messapp.storage.from(BUCKET).getPublicUrl(path).data.publicUrl,
      file_type: `encrypted:${image.type}`,
      // The real name lives inside the encrypted blob; this column is readable by the server.
      file_name: "image",
      file_size: image.size,
      is_spoiler: true,
    })
    .select()
    .single();

  if (error) {
    // A message without its image would show as an empty bubble in both apps.
    await messapp.from("messages").delete().eq("id", message.id);
    await messapp.storage.from(BUCKET).remove([path]);
    throw new Error(error.message);
  }

  return { message, attachment: data as MessageAttachment };
};

export const fetchAttachments = async (messageIds: string[]) => {
  if (!messageIds.length) return [];

  const { data, error } = await messapp
    .from("message_attachments")
    .select(
      "id, message_id, file_url, file_type, file_name, file_size, is_spoiler"
    )
    .in("message_id", messageIds);
  if (error) throw new Error(error.message);

  return data as MessageAttachment[];
};

/** Downloads and decrypts one image into an object URL; the caller owns revoking it. Null when it can't be opened. */
export const loadAttachmentImage = async (
  attachment: MessageAttachment,
  keyring: DmKeyring,
  sentByMe: boolean,
  peerKey: JsonWebKey | null
): Promise<string | null> => {
  const path = attachmentObjectPath(attachment.file_url);
  if (!path) return null;

  const { data, error } = await messapp.storage
    .from(BUCKET)
    .createSignedUrl(path, SIGNED_URL_TTL_SECONDS);
  if (error) throw new Error(error.message);

  const response = await fetch(data.signedUrl);
  if (!response.ok)
    throw new Error(`Attachment fetch failed: ${response.status}`);

  const blob = (await response.json()) as AttachmentBlob;
  const bytes = await openAttachment(keyring, blob, sentByMe, peerKey);
  if (!bytes) return null;

  // Whatever the sender claims, only known image types are shown.
  const type = (attachment.file_type.replace("encrypted:", "") || blob.type)
    .split(";", 1)[0]
    .trim()
    .toLowerCase();
  if (!IMAGE_TYPES.includes(type)) return null;

  return URL.createObjectURL(new Blob([bytes], { type }));
};
