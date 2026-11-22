import cn from "classnames";
import { XIcon } from "@primer/octicons-react";
import ProtonDBLogo from "@renderer/assets/protondb-logo.svg?url";
import { useTranslation } from "react-i18next";

import "./protondb-badge.scss";

const tierTranslation: Record<string, string> = {
  borked: "protondb_tier_borked",
  bronze: "protondb_tier_bronze",
  silver: "protondb_tier_silver",
  gold: "protondb_tier_gold",
  platinum: "protondb_tier_platinum",
};

/** A known ProtonDB tier in lower case, or null for missing, pending or unknown tiers. */
export const toProtonTier = (value?: string | null) => {
  const tier = value?.toLowerCase().trim();
  return tier && tier in tierTranslation ? tier : null;
};

interface ProtonDBBadgeProps {
  badge: string;
  className?: string;
}

export function ProtonDBBadge({ badge, className }: ProtonDBBadgeProps) {
  const { t } = useTranslation("game_details");

  const label = t(tierTranslation[badge] ?? "protondb_tier_unknown");

  return (
    <span
      className={cn("protondb-badge", `protondb-badge--${badge}`, className)}
      title={t("protondb_badge_title", { tier: label })}
    >
      {/* Borked gets a shape as well as a colour, so it never reads by hue alone. */}
      {badge === "borked" ? (
        <XIcon size={12} aria-hidden="true" />
      ) : (
        <img
          src={ProtonDBLogo}
          alt=""
          aria-hidden="true"
          className="protondb-badge__logo"
          width={12}
          height={12}
          loading="lazy"
        />
      )}
      <span>{label}</span>
    </span>
  );
}
