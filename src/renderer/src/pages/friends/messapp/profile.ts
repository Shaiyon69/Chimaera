import { messapp } from "./client";
import { IMAGE_TYPES } from "./attachment-blob";
import type { ChimaeraProfile } from "./profile-merge";

const APP = "chimaera";
const COLUMNS =
  "profile_id, display_name, avatar_url, status_text, status_expires_at, bio, banner_url";
const MAX_PROFILE_IMAGE_BYTES = 5 * 1024 * 1024;

export type ProfileImageKind = "avatar" | "banner";

export class ProfileImageError extends Error {
  constructor(readonly kind: "type" | "size") {
    super(`Profile image rejected: ${kind}`);
  }
}

export const fetchChimaeraProfiles = async (ids: string[]) => {
  const profiles = new Map<string, ChimaeraProfile>();
  if (!ids.length) return profiles;

  const { data, error } = await messapp
    .from("app_profiles")
    .select(COLUMNS)
    .eq("app", APP)
    .in("profile_id", ids);
  if (error) throw new Error(error.message);

  for (const row of data as (ChimaeraProfile & { profile_id: string })[]) {
    profiles.set(row.profile_id, row);
  }
  return profiles;
};

export const fetchMyChimaeraProfile = async (myId: string) =>
  (await fetchChimaeraProfiles([myId])).get(myId) ?? null;

export const saveMyChimaeraProfile = async (
  myId: string,
  fields: Omit<ChimaeraProfile, "status_expires_at">
) => {
  const { error } = await messapp.from("app_profiles").upsert(
    {
      profile_id: myId,
      app: APP,
      ...fields,
      status_expires_at: null,
      updated_at: new Date().toISOString(),
    },
    { onConflict: "profile_id,app" }
  );
  if (error) throw new Error(error.message);
};

/**
 * Uploads to the public `avatars` bucket. Its policy needs every object name to
 * start with `<uid>-avatar-`, so banners carry a `banner-` infix after that prefix.
 */
export const uploadProfileImage = async (
  userId: string,
  file: File,
  kind: ProfileImageKind
) => {
  const type = file.type.toLowerCase();
  if (!IMAGE_TYPES.includes(type)) throw new ProfileImageError("type");
  if (file.size > MAX_PROFILE_IMAGE_BYTES) throw new ProfileImageError("size");

  const ext =
    /\.([a-z0-9]{2,5})$/i.exec(file.name)?.[1]?.toLowerCase() ?? "webp";
  const name = `${userId}-avatar-${kind === "banner" ? "banner-" : ""}${crypto.randomUUID()}.${ext}`;

  const { error } = await messapp.storage.from("avatars").upload(name, file, {
    contentType: type,
    cacheControl: "31536000",
    upsert: false,
  });
  if (error) throw new Error(error.message);

  return messapp.storage.from("avatars").getPublicUrl(name).data.publicUrl;
};
