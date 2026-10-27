import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { EyeClosedIcon } from "@primer/octicons-react";

import { Button } from "@renderer/components";
import { logger } from "@renderer/logger";
import type { MessageAttachment } from "./messapp/attachments";

interface SpoilerImageProps {
  attachment: MessageAttachment;
  mine: boolean;
  /** Who sent it, so the reveal control says which image it opens. */
  sender: string;
  /** Must be stable between renders, or the image is downloaded again each time. */
  load: (
    attachment: MessageAttachment,
    mine: boolean
  ) => Promise<string | null>;
}

/** A chat image that stays blurred until clicked, and can be blurred again. */
export function SpoilerImage({
  attachment,
  mine,
  sender,
  load,
}: Readonly<SpoilerImageProps>) {
  const { t } = useTranslation("messapp");
  const frameRef = useRef<HTMLDivElement>(null);
  const [url, setUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const [revealed, setRevealed] = useState(false);
  const [near, setNear] = useState(false);
  const [attempt, setAttempt] = useState(0);

  // Nothing is downloaded or decrypted until the tile is about to be seen.
  useEffect(() => {
    const frame = frameRef.current;
    if (!frame || near) return;

    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry.isIntersecting) setNear(true);
      },
      { rootMargin: "200px" }
    );
    observer.observe(frame);
    return () => observer.disconnect();
  }, [near]);

  useEffect(() => {
    if (!near) return;

    let cancelled = false;
    let objectUrl: string | null = null;
    setUrl(null);
    setFailed(false);

    load(attachment, mine)
      .then((loaded) => {
        objectUrl = loaded;
        if (cancelled) {
          if (loaded) URL.revokeObjectURL(loaded);
          return;
        }
        if (loaded) setUrl(loaded);
        else setFailed(true);
      })
      .catch((err) => {
        logger.error("Failed to load chat image", err);
        if (!cancelled) setFailed(true);
      });

    return () => {
      cancelled = true;
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [load, attachment, mine, near, attempt]);

  if (failed) {
    return (
      <div className="friends__retry" role="alert">
        <p className="friends__empty">{t("image_unavailable")}</p>
        <Button theme="outline" onClick={() => setAttempt((n) => n + 1)}>
          {t("retry")}
        </Button>
      </div>
    );
  }

  if (!url) {
    return (
      <div
        ref={frameRef}
        className="friends__spoiler friends__spoiler--loading"
        aria-busy="true"
      />
    );
  }

  return (
    <div className="friends__spoiler">
      <img
        src={url}
        // The name only reaches assistive tech once the user chose to reveal.
        alt={revealed ? attachment.file_name : ""}
        aria-hidden={!revealed}
        draggable={false}
        className={`friends__spoiler-image ${revealed ? "" : "friends__spoiler-image--hidden"}`}
      />
      {/* One element for both states, so keyboard focus survives the toggle. */}
      <button
        type="button"
        className={`friends__spoiler-toggle ${revealed ? "friends__spoiler-toggle--hide" : ""}`}
        aria-label={
          revealed
            ? t("spoiler_hide")
            : t("spoiler_reveal_named", { name: sender })
        }
        onClick={() => setRevealed(!revealed)}
      >
        {revealed ? <EyeClosedIcon size={16} /> : t("spoiler_reveal")}
      </button>
    </div>
  );
}
