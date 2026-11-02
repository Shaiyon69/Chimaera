import { useEffect, useId, useState } from "react";
import { useTranslation } from "react-i18next";

import { Badge } from "@renderer/components";
import type { LibraryGame } from "@types";

import { Disclosure } from "./graphics-ui";

import "./optiscaler-section.scss";

type Upscaler = Awaited<
  ReturnType<typeof window.electron.getGameUpscalers>
>[number];

export function UpscalerBadges({
  game,
  revision,
}: Readonly<{ game: LibraryGame; revision: number }>) {
  const { t } = useTranslation("game_details");
  const headingId = useId();
  const [found, setFound] = useState<Upscaler[] | null>(null);

  useEffect(() => {
    void window.electron
      .getGameUpscalers(game.shop, game.objectId)
      .then(setFound)
      .catch(() => setFound(null));
  }, [game.shop, game.objectId, game.executablePath, revision]);

  if (!found) return null;

  const label = (u: Upscaler) =>
    [
      t(`upscalers_kind_${u.kind}`),
      u.version,
      u.added ? `(${t("upscalers_added")})` : null,
    ]
      .filter(Boolean)
      .join(" ");

  // One badge per kind and version; the same DLL often sits in several folders.
  const unique = [...new Map(found.map((u) => [label(u), u])).values()];

  return (
    <section className="optiscaler" aria-labelledby={headingId}>
      <h3 id={headingId}>{t("upscalers_title")}</h3>
      {unique.length ? (
        <>
          <div className="optiscaler__badges">
            {unique.map((u) => (
              <Badge key={label(u)}>{label(u)}</Badge>
            ))}
          </div>
          <Disclosure summary={t("upscalers_files", { count: found.length })}>
            <ul className="optiscaler__files">
              {found.map((u) => (
                <li key={u.file}>
                  {u.file} · {label(u)}
                </li>
              ))}
            </ul>
          </Disclosure>
        </>
      ) : (
        <p className="optiscaler__hint">{t("upscalers_none")}</p>
      )}
    </section>
  );
}
