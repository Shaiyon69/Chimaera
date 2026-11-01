import { useContext, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";
import { PlusCircleIcon } from "@primer/octicons-react";

import { Button, Modal, TextField, SteamIcon } from "@renderer/components";
import type { DownloadSource, Game, GameRepack } from "@types";

import { DownloadSettingsModal } from "./download-settings-modal";
import { gameDetailsContext } from "@renderer/context";
import { Downloader } from "@shared";
import { countBy, orderBy } from "lodash-es";
import {
  useDate,
  useAppDispatch,
  useAppSelector,
  useIsNonSteamExecutable,
} from "@renderer/hooks";
import { clearNewDownloadOptions } from "@renderer/features";
import { levelDBService } from "@renderer/services/leveldb.service";
import { logger } from "@renderer/logger";
import {
  canDownloadOnSteam,
  getGameKey,
  isSteamImportedGame,
} from "@renderer/helpers";
import "./repacks-modal.scss";

export interface RepacksModalProps {
  visible: boolean;
  startDownload: (
    repack: GameRepack,
    downloader: Downloader,
    downloadPath: string,
    automaticallyExtract: boolean,
    addToQueueOnly?: boolean,
    fileIndices?: number[],
    selectedFilesSize?: number | null,
    automaticallyDeleteArchiveFiles?: boolean,
    signal?: AbortSignal
  ) => Promise<{ ok: boolean; error?: string }>;
  onClose: () => void;
}

type Availability = "online" | "partial" | "offline";

const SKELETON_ROWS = 3;

function getAvailability(repack: GameRepack): Availability {
  const uris = Array.isArray(repack.uris) ? repack.uris : [];
  const unavailable = new Set(repack.unavailableUris ?? []);
  const availableCount = uris.filter((uri) => !unavailable.has(uri)).length;

  if (availableCount === 0) return "offline";
  if (availableCount === uris.length) return "online";
  return "partial";
}

export function RepacksModal({
  visible,
  startDownload,
  onClose,
}: Readonly<RepacksModalProps>) {
  const [repack, setRepack] = useState<GameRepack | null>(null);
  const [showSelectFolderModal, setShowSelectFolderModal] = useState(false);
  const [downloadSources, setDownloadSources] = useState<DownloadSource[]>([]);
  const [selectedSources, setSelectedSources] = useState<string[]>([]);
  const [filterTerm, setFilterTerm] = useState("");

  const [lastCheckTimestamp, setLastCheckTimestamp] = useState<string | null>(
    null
  );
  const [isLoadingTimestamp, setIsLoadingTimestamp] = useState(true);
  const [viewedRepackIds, setViewedRepackIds] = useState<Set<string>>(
    new Set()
  );

  const { game, repacks, repacksStatus, reloadRepacks } =
    useContext(gameDetailsContext);

  const { t } = useTranslation("game_details");

  const { formatDate } = useDate();
  const navigate = useNavigate();
  const dispatch = useAppDispatch();
  const userPreferences = useAppSelector(
    (state) => state.userPreferences.value
  );
  const showNewBadges =
    userPreferences?.enableNewDownloadOptionsBadges !== false;

  useEffect(() => {
    if (!visible) return;
    levelDBService
      .values("downloadSources")
      .then((sources) => setDownloadSources(sources as DownloadSource[]))
      .catch((error) => logger.error("Failed to read download sources", error));
  }, [visible]);

  useEffect(() => {
    const fetchLastCheckTimestamp = async () => {
      setIsLoadingTimestamp(true);

      try {
        const timestamp = (await levelDBService.get(
          "downloadSourcesSinceValue",
          null,
          "utf8"
        )) as string | null;

        setLastCheckTimestamp(timestamp);
      } catch {
        setLastCheckTimestamp(null);
      } finally {
        setIsLoadingTimestamp(false);
      }
    };

    if (visible && showNewBadges) {
      fetchLastCheckTimestamp();
    } else {
      setIsLoadingTimestamp(false);
    }
  }, [visible, repacks, showNewBadges]);

  useEffect(() => {
    if (
      visible &&
      game?.newDownloadOptionsCount &&
      game.newDownloadOptionsCount > 0
    ) {
      const gameKey = getGameKey(game.shop, game.objectId);
      levelDBService
        .get(gameKey, "games")
        .then((gameData) => {
          if (gameData) {
            const updated = {
              ...(gameData as Game),
              newDownloadOptionsCount: undefined,
            };
            return levelDBService.put(gameKey, updated, "games");
          }
          return Promise.resolve();
        })
        .catch((error) =>
          logger.error("Failed to clear the new download options count", error)
        );

      const gameId = `${game.shop}:${game.objectId}`;
      dispatch(clearNewDownloadOptions({ gameId }));
    }
  }, [visible, game, dispatch]);

  const sortedRepacks = useMemo(
    () => orderBy(repacks, [(repack) => repack.uploadDate], ["desc"]),
    [repacks]
  );

  // Chips come from the options themselves, so every chip matches something.
  const sourceChips = useMemo(() => {
    const counts = countBy(repacks, (repack) => repack.downloadSourceName);
    return orderBy(Object.entries(counts), [1, 0], ["desc", "asc"]);
  }, [repacks]);

  const term = filterTerm.trim().toLowerCase();

  const filteredRepacks = useMemo(
    () =>
      sortedRepacks.filter(
        (repack) =>
          (selectedSources.length === 0 ||
            selectedSources.includes(repack.downloadSourceName)) &&
          (!term ||
            repack.title.toLowerCase().includes(term) ||
            repack.downloadSourceName.toLowerCase().includes(term))
      ),
    [sortedRepacks, selectedSources, term]
  );

  const isFiltered = term !== "" || selectedSources.length > 0;

  const handleRepackClick = (repack: GameRepack) => {
    setRepack(repack);
    setShowSelectFolderModal(true);
    setViewedRepackIds((prev) => new Set(prev).add(repack.id));
  };

  const toggleSource = (name: string) => {
    setSelectedSources((prev) =>
      prev.includes(name) ? prev.filter((n) => n !== name) : [...prev, name]
    );
  };

  const clearFilters = () => {
    setFilterTerm("");
    setSelectedSources([]);
  };

  const goToSources = () => {
    onClose();
    navigate("/settings?tab=download_sources");
  };

  const checkIfLastDownloadedOption = (repack: GameRepack) => {
    if (!game?.download) return false;
    if (!Array.isArray(repack.uris)) return false;
    return repack.uris.some((uri) => uri.includes(game.download!.uri));
  };

  const isNewRepack = (repack: GameRepack): boolean => {
    if (isLoadingTimestamp) return false;
    if (viewedRepackIds.has(repack.id)) return false;
    if (!lastCheckTimestamp || !repack.createdAt) return false;

    const lastCheckDate = new Date(lastCheckTimestamp);
    if (isNaN(lastCheckDate.getTime())) return false;

    return repack.createdAt > lastCheckDate.toISOString();
  };

  const isNonSteamExecutable = useIsNonSteamExecutable(game);
  const showSteamOption =
    (canDownloadOnSteam(game) ||
      (isSteamImportedGame(game) && isNonSteamExecutable)) &&
    selectedSources.length === 0 &&
    (!term ||
      "steam".includes(term) ||
      (game?.title ?? "").toLowerCase().includes(term));

  const handleSteamOptionClick = () => {
    if (!game) return;

    void window.electron.installGameOnSteam(game.objectId);
    onClose();
  };

  useEffect(() => {
    if (!visible) clearFilters();
  }, [visible]);

  const renderStatus = (availability: Availability) => (
    <span
      className={`repacks-modal__status repacks-modal__status--${availability}`}
      title={t(`source_${availability}`)}
    >
      <span className="repacks-modal__status-dot" aria-hidden="true" />
      {t(`availability_${availability}`)}
    </span>
  );

  const renderEmpty = () => {
    if (repacksStatus === "error") {
      return (
        <div className="repacks-modal__empty" role="alert">
          <p>{t("download_options_load_failed")}</p>
          <Button type="button" theme="outline" onClick={reloadRepacks}>
            {t("retry_download_options")}
          </Button>
        </div>
      );
    }

    if (isFiltered && repacks.length > 0) {
      return (
        <div className="repacks-modal__empty">
          <p>{t("no_download_options_match")}</p>
          <Button type="button" theme="outline" onClick={clearFilters}>
            {t("clear_download_filters")}
          </Button>
        </div>
      );
    }

    if (downloadSources.length === 0) {
      return (
        <div className="repacks-modal__empty">
          <p>{t("no_download_sources_added")}</p>
          <Button type="button" theme="primary" onClick={goToSources}>
            <PlusCircleIcon />
            {t("add_download_source", { ns: "settings" })}
          </Button>
        </div>
      );
    }

    return (
      <div className="repacks-modal__empty">
        <p>
          {t("no_download_options_in_sources", {
            count: downloadSources.length,
          })}
        </p>
        <Button type="button" theme="outline" onClick={goToSources}>
          {t("manage_download_sources")}
        </Button>
      </div>
    );
  };

  const renderList = () => {
    if (repacksStatus === "loading" && repacks.length === 0) {
      return (
        <ul
          className="repacks-modal__list"
          aria-busy="true"
          aria-label={t("loading_download_options")}
        >
          {Array.from({ length: SKELETON_ROWS }, (_, index) => (
            <li key={index} className="repacks-modal__skeleton" />
          ))}
        </ul>
      );
    }

    if (filteredRepacks.length === 0 && !showSteamOption) return renderEmpty();

    return (
      <ul className="repacks-modal__list">
        {showSteamOption && game && (
          <li>
            <button
              type="button"
              className="repacks-modal__row"
              onClick={handleSteamOptionClick}
            >
              <span className="repacks-modal__row-main">
                <span className="repacks-modal__row-title">
                  <SteamIcon size={16} />
                  {game.title}
                </span>
                <span className="repacks-modal__row-meta">
                  {t("download_on_steam")}
                </span>
              </span>
              <span className="repacks-modal__row-side">
                {renderStatus("online")}
              </span>
            </button>
          </li>
        )}

        {filteredRepacks.map((repack) => {
          const isNew = showNewBadges && isNewRepack(repack);
          const isLast = checkIfLastDownloadedOption(repack);
          const meta = [
            repack.fileSize?.trim(),
            repack.uploadDate ? formatDate(repack.uploadDate) : null,
          ].filter(Boolean);

          return (
            <li key={repack.id}>
              <button
                type="button"
                className="repacks-modal__row"
                onClick={() => handleRepackClick(repack)}
              >
                <span className="repacks-modal__row-main">
                  <span
                    className="repacks-modal__row-title"
                    title={repack.title}
                  >
                    {repack.title}
                  </span>
                  <span className="repacks-modal__row-meta">
                    <span className="repacks-modal__source">
                      {repack.downloadSourceName}
                    </span>
                    {meta.map((part) => (
                      <span key={part}>{part}</span>
                    ))}
                  </span>
                </span>

                <span className="repacks-modal__row-side">
                  {isNew && (
                    <span className="repacks-modal__tag repacks-modal__tag--new">
                      {t("new_download_option")}
                    </span>
                  )}
                  {isLast && (
                    <span className="repacks-modal__tag">
                      {t("last_downloaded_option")}
                    </span>
                  )}
                  {renderStatus(getAvailability(repack))}
                </span>
              </button>
            </li>
          );
        })}
      </ul>
    );
  };

  return (
    <>
      <DownloadSettingsModal
        visible={showSelectFolderModal}
        onClose={() => setShowSelectFolderModal(false)}
        startDownload={startDownload}
        repack={repack}
      />

      <Modal
        visible={visible}
        title={t("download_options_title")}
        description={t("repacks_modal_description")}
        onClose={onClose}
        large
      >
        <div className="repacks-modal">
          {repacks.length > 0 && (
            <div className="repacks-modal__toolbar">
              <TextField
                placeholder={t("filter")}
                aria-label={t("filter")}
                value={filterTerm}
                onChange={(event) => setFilterTerm(event.target.value)}
              />
              <span className="repacks-modal__count" aria-live="polite">
                {isFiltered
                  ? t("download_options_shown", {
                      shown: filteredRepacks.length,
                      total: repacks.length,
                    })
                  : t("download_options_total", { count: repacks.length })}
              </span>
            </div>
          )}

          {sourceChips.length > 1 && (
            <div
              className="repacks-modal__chips"
              role="group"
              aria-label={t("filter_by_source")}
            >
              {sourceChips.map(([name, count]) => {
                const active = selectedSources.includes(name);
                return (
                  <button
                    key={name}
                    type="button"
                    className={`repacks-modal__chip ${active ? "repacks-modal__chip--active" : ""}`}
                    aria-pressed={active}
                    onClick={() => toggleSource(name)}
                  >
                    {name}
                    <span className="repacks-modal__chip-count">{count}</span>
                  </button>
                );
              })}
            </div>
          )}

          {renderList()}
        </div>
      </Modal>
    </>
  );
}
