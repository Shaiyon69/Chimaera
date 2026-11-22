import { useTranslation } from "react-i18next";
import { LinkExternalIcon } from "@primer/octicons-react";

import { Modal } from "../modal/modal";
import { Button } from "../button/button";
import type { ReleaseNotesSection } from "./release-notes-core";
import "./whats-new-modal.scss";

export { parseReleaseNotes } from "./release-notes-core";

// The release body on Skibidevs; the release workflow writes it from the commits since the last version.
export const fetchReleaseNotes = async (version: string) => {
  const response = await fetch(
    `https://api.github.com/repos/Shaiyon69/Skibidevs/releases/tags/chimaera-v${version}`,
    { headers: { Accept: "application/vnd.github+json" } }
  );
  // No release for this version (a local or test build): nothing to show.
  if (response.status === 404) return "";
  if (!response.ok) throw new Error(`release notes: HTTP ${response.status}`);
  const release = (await response.json()) as { body?: string | null };
  return release.body ?? "";
};

export interface WhatsNewModalProps {
  visible: boolean;
  version: string;
  sections: ReleaseNotesSection[];
  onClose: () => void;
  onOpenAllReleases: () => void;
}

export function WhatsNewModal({
  visible,
  version,
  sections,
  onClose,
  onOpenAllReleases,
}: Readonly<WhatsNewModalProps>) {
  const { t } = useTranslation("whats_new");

  return (
    <Modal visible={visible} title={t("title", { version })} onClose={onClose}>
      <div className="whats-new">
        <div className="whats-new__sections">
          {sections.map((section, index) => (
            <section
              key={`${section.title ?? ""}-${index}`}
              className="whats-new__section"
            >
              {section.title && (
                <h3 className="whats-new__section-title">{section.title}</h3>
              )}
              <ul className="whats-new__list">
                {section.items.map((item, itemIndex) => (
                  <li key={`${itemIndex}-${item}`}>{item}</li>
                ))}
              </ul>
            </section>
          ))}
        </div>

        <div className="whats-new__actions">
          <Button theme="outline" onClick={onOpenAllReleases}>
            {t("all_releases")}
            <LinkExternalIcon size={14} aria-hidden="true" />
          </Button>
          <Button onClick={onClose}>{t("got_it")}</Button>
        </div>
      </div>
    </Modal>
  );
}
