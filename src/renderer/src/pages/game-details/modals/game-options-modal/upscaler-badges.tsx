import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { Badge } from "@renderer/components";
import type { LibraryGame } from "@types";

import "./optiscaler-section.scss";

type Upscaler = Awaited<
  ReturnType<typeof window.electron.getGameUpscalers>
>[number];

export function UpscalerBadges({
  game,
  revision,
}: Readonly<{ game: LibraryGame; revision: number }>) {
  const { t } = useTranslation("game_details");
  const [found, setFound] = useState<Upscaler[] | null>(null);

  useEffect(() => {
    void window.electron
      .getGameUpscalers(game.shop, game.objectId)
      .then(setFound)
      .catch(() => setFound(null));
  }, [game.shop, game.objectId, game.executablePath, revision]);

  if (!found) return null;

  // One badge per kind and version; the same DLL often sits in several folders.
  const unique = [
    ...new Map(
      found.map((u) => [`${u.kind}|${u.version}|${u.added}`, u])
    ).values(),
  ];

  return (
    <div className="optiscaler">
      <h3>{t("upscalers_title")}</h3>
      {unique.length ? (
        <div className="optiscaler__badges">
          {unique.map((u) => (
            <span
              key={`${u.kind}|${u.version}|${u.added}`}
              title={found
                .filter((f) => f.kind === u.kind && f.version === u.version)
                .map((f) => f.file)
                .join("\n")}
            >
              <Badge>
                {[
                  t(`upscalers_kind_${u.kind}`),
                  u.version,
                  u.added ? `(${t("upscalers_added")})` : null,
                ]
                  .filter(Boolean)
                  .join(" ")}
              </Badge>
            </span>
          ))}
        </div>
      ) : (
        <p className="optiscaler__hint">{t("upscalers_none")}</p>
      )}
    </div>
  );
}
