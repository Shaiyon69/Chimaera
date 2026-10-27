import { messapp } from "./client";
import {
  fetchMyChimaeraProfile,
  saveMyChimaeraProfile,
  uploadProfileImage,
  type ProfileImageKind,
} from "./profile";
import { PROFILE_LIMITS } from "./profile-merge";

type SyncedImages = Partial<Record<ProfileImageKind, string>>;

const syncedKey = (userId: string) => `messapp:synced-images:${userId}`;

// Remembers which local image each uploaded copy came from, so saving the
// profile does not re-upload an unchanged picture every time.
const readSynced = (userId: string): SyncedImages => {
  try {
    return JSON.parse(localStorage.getItem(syncedKey(userId)) ?? "{}");
  } catch {
    return {};
  }
};

const toFile = async (localUrl: string) => {
  const blob = await (await fetch(localUrl)).blob();
  return new File([blob], `image.${blob.type.split("/")[1] ?? "webp"}`, {
    type: blob.type,
  });
};

/**
 * Pushes the local profile (name, bio, avatar, banner) to the Friends server so
 * friends see what the user edits in Chimaera. No-op when not signed in to Friends.
 * `status` is Friends-only; omit it to keep the saved one.
 */
export const syncLocalProfileToFriends = async (status?: string) => {
  const userId = (await messapp.auth.getSession()).data.session?.user.id;
  if (!userId) return;

  const [me, current] = await Promise.all([
    window.electron.getMe(),
    fetchMyChimaeraProfile(userId),
  ]);
  if (!me) return;

  const synced = readSynced(userId);
  const nextSynced: SyncedImages = {};

  const image = async (kind: ProfileImageKind, local: string | null) => {
    const uploaded = current?.[`${kind}_url`] ?? null;
    if (!local) return null;
    if (!local.startsWith("local:")) return local;
    if (local === synced[kind] && uploaded) {
      nextSynced[kind] = local;
      return uploaded;
    }

    const url = await uploadProfileImage(userId, await toFile(local), kind);
    nextSynced[kind] = local;
    return url;
  };

  // shortcut: clamps to the server's column limits, so a longer local bio is cut for friends; upgrade by raising the limits server-side.
  await saveMyChimaeraProfile(userId, {
    display_name: me.displayName.slice(0, PROFILE_LIMITS.display_name),
    bio: me.bio?.slice(0, PROFILE_LIMITS.bio) || null,
    status_text: (status ?? current?.status_text)?.trim() || null,
    avatar_url: await image("avatar", me.profileImageUrl),
    banner_url: await image("banner", me.backgroundImageUrl),
  });

  try {
    localStorage.setItem(syncedKey(userId), JSON.stringify(nextSynced));
  } catch {
    // Worst case the next save re-uploads the images.
  }
};
