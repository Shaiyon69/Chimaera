import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { BookIcon } from "@primer/octicons-react";
import { Button, Modal } from "@renderer/components";
import { logger } from "@renderer/logger";
import { SidebarSection } from "../sidebar-section/sidebar-section";
import { companionUrl, findCompanion } from "./registry";
import "./companion-section.scss";

export interface CompanionSectionProps {
  shop: string;
  objectId: string | undefined;
}

export function CompanionSection({
  shop,
  objectId,
}: Readonly<CompanionSectionProps>) {
  const { t } = useTranslation("game_details");
  const companion = findCompanion(shop, objectId);
  const url = companion ? companionUrl(companion.id) : null;
  const [available, setAvailable] = useState(false);
  const [open, setOpen] = useState(false);

  useEffect(() => {
    setAvailable(false);
    if (!url) return;
    let cancelled = false;
    // Builds without the companions repo ship no bundle; only offer what is there.
    fetch(url, { method: "HEAD" })
      .then((response) => {
        if (!cancelled) setAvailable(response.ok);
      })
      .catch((error) => logger.warn("Companion check failed", url, error));
    return () => {
      cancelled = true;
    };
  }, [url]);

  if (!companion || !url || !available) return null;

  const title = t(companion.titleKey);

  return (
    <SidebarSection title={t("companion")}>
      <div className="companion-section">
        <p className="companion-section__hint">{t("companion_hint")}</p>
        <Button theme="outline" onClick={() => setOpen(true)}>
          <BookIcon />
          {t("open_companion", { title })}
        </Button>
      </div>
      <Modal
        visible={open}
        title={title}
        onClose={() => setOpen(false)}
        noContentPadding
        className="companion-modal"
      >
        <iframe className="companion-modal__frame" src={url} title={title} />
      </Modal>
    </SidebarSection>
  );
}
