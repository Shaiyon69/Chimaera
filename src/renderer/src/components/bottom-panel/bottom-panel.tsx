import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { CommentDiscussionIcon, DownloadIcon } from "@primer/octicons-react";

import {
  useAppSelector,
  useDownload,
  useLibrary,
  useToast,
  useUserDetails,
} from "@renderer/hooks";

import {
  fetchReleaseNotes,
  parseReleaseNotes,
  WhatsNewModal,
} from "../whats-new-modal/whats-new-modal";
import type { ReleaseNotesSection } from "../whats-new-modal/release-notes-core";
import { logger } from "@renderer/logger";
import "./bottom-panel.scss";

import { useNavigate } from "react-router-dom";

// Chimaera's page on Skibidevs: version history with each release's notes.
const RELEASE_NOTES_URL =
  "https://shaiyon69.github.io/Skibidevs/apptic/?app=chimaera";

// The last version whose What's new panel was closed; a newer one opens the panel once.
const WHATS_NEW_SEEN_KEY = "whatsNewSeenVersion";

const readSeenVersion = () => {
  try {
    return localStorage.getItem(WHATS_NEW_SEEN_KEY);
  } catch {
    return null;
  }
};

const markSeen = (version: string) => {
  try {
    localStorage.setItem(WHATS_NEW_SEEN_KEY, version);
  } catch {
    // Storage off: the panel may show again next start, which is harmless.
  }
};

const HYDRA_CLOUD_GRADIENT_START_COLOR = "#16b195";
const HYDRA_CLOUD_GRADIENT_END_COLOR = "#3e62c0";

export function BottomPanel() {
  const { t } = useTranslation(["bottom_panel", "sidebar", "whats_new"]);

  const navigate = useNavigate();

  const { userDetails, hasActiveSubscription } = useUserDetails();

  const { downloadLibrary: library } = useLibrary();

  const { showSuccessToast } = useToast();

  const { lastPacket, progress, downloadSpeed, eta } = useDownload();

  const extraction = useAppSelector((state) => state.download.extraction);

  const [version, setVersion] = useState("");
  const [releaseNotes, setReleaseNotes] = useState<ReleaseNotesSection[]>([]);
  const [showWhatsNew, setShowWhatsNew] = useState(false);

  const loadReleaseNotes = useCallback(async (forVersion: string) => {
    const sections = parseReleaseNotes(await fetchReleaseNotes(forVersion));
    setReleaseNotes(sections);
    return sections;
  }, []);

  useEffect(() => {
    // Dev builds share the released version number; only real releases announce themselves.
    if (!version || import.meta.env.DEV || readSeenVersion() === version)
      return;
    // A failed fetch leaves the version unseen, so the next start tries again.
    loadReleaseNotes(version)
      .then((sections) => {
        if (sections.length > 0) setShowWhatsNew(true);
        else markSeen(version);
      })
      .catch((error) => logger.warn("What's new: no release notes", error));
  }, [version, loadReleaseNotes]);

  const closeWhatsNew = () => {
    setShowWhatsNew(false);
    markSeen(version);
  };

  const openWhatsNew = async () => {
    try {
      const sections =
        releaseNotes.length > 0
          ? releaseNotes
          : await loadReleaseNotes(version);
      if (sections.length > 0) {
        setShowWhatsNew(true);
        return;
      }
    } catch (error) {
      logger.warn("What's new: no release notes", error);
    }
    window.electron.openExternal(RELEASE_NOTES_URL);
  };
  const [commonRedistStatus, setCommonRedistStatus] = useState<string | null>(
    null
  );

  useEffect(() => {
    window.electron.getVersion().then((result) => setVersion(result));
  }, []);

  useEffect(() => {
    const unlisten = window.electron.onCommonRedistProgress(
      ({ log, complete }) => {
        if (log === "Installation timed out" || complete) {
          setCommonRedistStatus(null);

          if (complete) {
            showSuccessToast(
              t("installation_complete"),
              t("installation_complete_message")
            );
          }

          return;
        }

        setCommonRedistStatus(log);
      }
    );

    return () => unlisten();
  }, [t, showSuccessToast]);

  const status = useMemo(() => {
    if (commonRedistStatus) {
      return t("installing_common_redist", { log: commonRedistStatus });
    }

    if (extraction) {
      const extractingGame = library.find(
        (game) => game.id === extraction.visibleId
      );

      if (extractingGame) {
        const extractionPercentage = Math.round(extraction.progress * 100);
        return t("extracting", {
          title: extractingGame.title,
          percentage: `${extractionPercentage}%`,
        });
      }
    }

    const game = lastPacket
      ? library.find((game) => game.id === lastPacket?.gameId)
      : undefined;

    if (game) {
      if (lastPacket?.isRecovering)
        return t("recovering", {
          title: game.title,
          percentage: `${Math.round((lastPacket.recoveryProgress ?? 0) * 100)}%`,
        });

      if (lastPacket?.isReconnecting)
        return t("reconnecting", {
          title: game.title,
        });

      if (lastPacket?.isCheckingFiles)
        return t("checking_files", {
          title: game.title,
          percentage: progress,
        });

      if (lastPacket?.isDownloadingMetadata)
        return t("downloading_metadata", {
          title: game.title,
          percentage: progress,
        });

      if (!eta) {
        return t("calculating_eta", {
          title: game.title,
          percentage: progress,
        });
      }

      return t("downloading", {
        title: game.title,
        percentage: progress,
        eta,
        speed: downloadSpeed,
      });
    }

    return null;
  }, [
    t,
    library,
    lastPacket,
    progress,
    eta,
    downloadSpeed,
    commonRedistStatus,
    extraction,
  ]);

  return (
    <footer className="bottom-panel">
      <div className="bottom-panel__left">
        {hasActiveSubscription && (
          <button
            type="button"
            className="bottom-panel__help-button"
            data-open-support-chat
          >
            <svg
              width="0"
              height="0"
              aria-hidden="true"
              className="bottom-panel__gradient-defs"
            >
              <defs>
                <linearGradient
                  id="hydra-cloud-gradient"
                  x1="0%"
                  y1="100%"
                  x2="100%"
                  y2="0%"
                >
                  <stop
                    offset="0%"
                    stopColor={HYDRA_CLOUD_GRADIENT_START_COLOR}
                  />
                  <stop
                    offset="100%"
                    stopColor={HYDRA_CLOUD_GRADIENT_END_COLOR}
                  />
                </linearGradient>
              </defs>
            </svg>
            <CommentDiscussionIcon
              size={14}
              className="bottom-panel__help-icon"
            />
            <small>{t("need_help", { ns: "sidebar" })}</small>
          </button>
        )}
      </div>

      <div
        className={`bottom-panel__center${
          !userDetails || !hasActiveSubscription
            ? " bottom-panel__center--start"
            : ""
        }`}
      >
        <button
          type="button"
          className="bottom-panel__downloads-button"
          onClick={() => navigate("/downloads")}
        >
          {status ? (
            <small>{status}</small>
          ) : (
            <DownloadIcon
              size={14}
              aria-label={t("downloads", { ns: "sidebar" })}
            />
          )}
        </button>
      </div>

      <div className="bottom-panel__right">
        <button
          type="button"
          className="bottom-panel__version-button"
          title={t("whats_new", { ns: "whats_new" })}
          onClick={openWhatsNew}
        >
          <small>v{version}</small>
        </button>
      </div>

      <WhatsNewModal
        visible={showWhatsNew}
        version={version}
        sections={releaseNotes}
        onClose={closeWhatsNew}
        onOpenAllReleases={() =>
          window.electron.openExternal(RELEASE_NOTES_URL)
        }
      />
    </footer>
  );
}
