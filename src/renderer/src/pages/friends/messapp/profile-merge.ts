/** What a user chose to show inside Chimaera. Kept apart from their MESSAPP profile: blank means blank. */
export interface ChimaeraProfile {
  display_name: string | null;
  avatar_url: string | null;
  status_text: string | null;
  status_expires_at: string | null;
  bio: string | null;
  banner_url: string | null;
}

// Mirrors the CHECK constraints on public.app_profiles.
export const PROFILE_LIMITS = { display_name: 40, status_text: 80, bio: 300 };

interface BaseProfile {
  username: string;
}

export type MergedProfile<T extends BaseProfile> = Omit<
  T,
  "avatar_url" | "status_text"
> & {
  avatar_url: string | null;
  status_text: string | null;
  display_name: string | null;
  bio: string | null;
  banner_url: string | null;
};

const filled = (value: string | null | undefined) =>
  value && value.trim() ? value : null;

export const applyChimaeraProfile = <T extends BaseProfile>(
  base: T,
  override: ChimaeraProfile | undefined
): MergedProfile<T> => {
  const statusExpired =
    !!override?.status_expires_at &&
    new Date(override.status_expires_at).getTime() < Date.now();

  return {
    ...base,
    display_name: filled(override?.display_name),
    // Never fall back to the MESSAPP avatar/status; the two apps keep separate profiles.
    avatar_url: filled(override?.avatar_url),
    status_text: statusExpired ? null : filled(override?.status_text),
    bio: filled(override?.bio),
    banner_url: filled(override?.banner_url),
  };
};

/** The name shown in Chimaera; the username stays the handle people search for. */
export const shownName = (profile: {
  username: string;
  display_name?: string | null;
}) => profile.display_name || profile.username;
