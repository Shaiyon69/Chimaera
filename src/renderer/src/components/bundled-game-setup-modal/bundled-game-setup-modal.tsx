import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { BUNDLED_GAMES } from "@shared";

import { Modal } from "../modal/modal";
import { Button } from "../button/button";
import {
  BUNDLED_GAME_CONFIGURED_EVENT,
  BUNDLED_GAME_SETUP_EVENT,
} from "./events";

import "./bundled-game-setup-modal.scss";

/** Offers a bundled game's download; its own launcher handles everything after that. */
export function BundledGameSetupModal() {
  const { t } = useTranslation("bundled_game_setup");
  const [objectId, setObjectId] = useState<string | null>(null);
  const [progress, setProgress] = useState<{
    phase: "downloading" | "extracting";
    percent: number;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);

  const game = BUNDLED_GAMES.find((g) => g.objectId === objectId);

  const open = useCallback(async (id: string) => {
    if (!BUNDLED_GAMES.some((g) => g.objectId === id)) return;
    const { installed } = await window.electron.getBundledGameSetup(id);
    if (installed) {
      await window.electron.openGame("custom", id, "");
      return;
    }
    setError(null);
    setObjectId(id);
  }, []);

  useEffect(() => {
    const unsubscribe = window.electron.onBundledGameSetupRequired(open);
    const onSetup = (event: Event) =>
      void open((event as CustomEvent<string>).detail);
    window.addEventListener(BUNDLED_GAME_SETUP_EVENT, onSetup);
    return () => {
      unsubscribe();
      window.removeEventListener(BUNDLED_GAME_SETUP_EVENT, onSetup);
    };
  }, [open]);

  useEffect(
    () =>
      window.electron.onBundledGameProgress(({ id, phase, percent }) => {
        if (id === objectId) setProgress({ phase, percent });
      }),
    [objectId]
  );

  const download = async () => {
    if (!objectId) return;
    setError(null);
    setProgress({ phase: "downloading", percent: 0 });
    try {
      await window.electron.downloadBundledGame(objectId);
      window.dispatchEvent(new Event(BUNDLED_GAME_CONFIGURED_EVENT));
      setObjectId(null);
      await window.electron.openGame("custom", objectId, "");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setProgress(null);
    }
  };

  if (!game) return null;

  const close = () => setObjectId(null);

  return (
    <Modal
      visible
      title={t("title", { title: game.title })}
      description={t(`port_missing_${game.objectId}`)}
      onClose={close}
      clickOutsideToClose={false}
    >
      <div className="bundled-game-setup">
        {error && (
          <span className="bundled-game-setup__error">
            {t("port_download_failed", { message: error })}
          </span>
        )}
        <div className="bundled-game-setup__actions">
          <Button type="button" theme="outline" onClick={close}>
            {t("cancel")}
          </Button>
          <Button type="button" onClick={download} disabled={progress !== null}>
            {progress
              ? t(
                  progress.phase === "downloading"
                    ? "port_downloading"
                    : "port_extracting",
                  { percent: progress.percent }
                )
              : t(`download_port_${game.objectId}`)}
          </Button>
        </div>
      </div>
    </Modal>
  );
}
