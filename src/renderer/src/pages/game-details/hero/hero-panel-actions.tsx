import {
  DownloadIcon,
  FileIcon,
  GearIcon,
  HeartFillIcon,
  HeartIcon,
  PinIcon,
  PinSlashIcon,
  PlayIcon,
  PlusCircleIcon,
} from "@primer/octicons-react";
import { Button, ConfirmationModal } from "@renderer/components";
import { FileArchive, XCircle } from "lucide-react";
import {
  useDownload,
  useGameExecutablePicker,
  useFinishedDownloadStep,
  useLibrary,
  useToast,
  useUserDetails,
} from "@renderer/hooks";
import { useContext, useState } from "react";
import { useTranslation } from "react-i18next";
import { useLocation, useNavigate } from "react-router-dom";
import { gameDetailsContext } from "@renderer/context";
import { logger } from "@renderer/logger";
import {
  canDownloadOnSteam,
  handleClassicsLaunchError,
} from "@renderer/helpers";
import { DiscSelectionModal } from "../modals/disc-selection-modal";

import "./hero-panel-actions.scss";
import { useEffect } from "react";
import { findBundledGame } from "@shared";
import { linkedExecutables } from "@renderer/pages/library/linked-games";
import {
  BUNDLED_GAME_CONFIGURED_EVENT,
  openBundledGameSetup,
} from "@renderer/components/bundled-game-setup-modal/events";

export function HeroPanelActions() {
  const [toggleLibraryGameDisabled, setToggleLibraryGameDisabled] =
    useState(false);

  const { isGameDeleting } = useDownload();
  const { userDetails } = useUserDetails();

  const {
    game,
    repacks,
    isGameRunning,
    shop,
    objectId,
    gameTitle,
    shopDetails,
    setShowGameOptionsModal,
    setGameOptionsInitialCategory,
    setShowRepacksModal,
    updateGame,
    selectGameExecutable,
    isTransferring,
    transferProgress,
  } = useContext(gameDetailsContext);

  const { lastPacket } = useDownload();

  const isGameDownloading =
    game?.download?.status === "active" && lastPacket?.gameId === game?.id;

  const pickGameExecutable = useGameExecutablePicker();
  const finishedDownloadStep = useFinishedDownloadStep(game);

  const { updateLibrary } = useLibrary();

  const { showSuccessToast, showErrorToast } = useToast();

  const navigate = useNavigate();
  const location = useLocation();

  const [showDiscSelectionModal, setShowDiscSelectionModal] = useState(false);

  useEffect(() => {
    const state = (location.state as Record<string, unknown> | null) ?? {};
    if (!state.openDiscSelection) return;
    if (game?.shop !== "launchbox" || (game?.discs?.length ?? 0) <= 1) return;

    setShowDiscSelectionModal(true);
    navigate(location.pathname + location.search, { replace: true, state: {} });
  }, [location, game?.shop, game?.discs?.length, navigate]);
  const [pendingClassicsLaunch, setPendingClassicsLaunch] = useState<{
    discPath: string | undefined;
  } | null>(null);

  const { t } = useTranslation("game_details");

  useEffect(() => {
    const onOpenDiscSelection = (event: Event) => {
      const detail = (event as CustomEvent<{ objectId?: string }>).detail;
      if (!detail?.objectId || detail.objectId === game?.objectId) {
        if (game?.shop === "launchbox" && (game?.discs?.length ?? 0) > 1) {
          setShowDiscSelectionModal(true);
        }
      }
    };
    window.addEventListener(
      "hydra:openDiscSelection",
      onOpenDiscSelection as EventListener
    );
    return () => {
      window.removeEventListener(
        "hydra:openDiscSelection",
        onOpenDiscSelection as EventListener
      );
    };
  }, [game?.objectId, game?.shop, game?.discs?.length]);

  useEffect(() => {
    const onFavoriteToggled = () => {
      updateLibrary();
      updateGame();
    };

    const onGameRemoved = () => {
      updateLibrary();
      updateGame();
    };

    const onFilesRemoved = () => {
      updateLibrary();
      updateGame();
    };

    window.addEventListener(
      "hydra:game-favorite-toggled",
      onFavoriteToggled as EventListener
    );
    window.addEventListener(
      "hydra:game-removed-from-library",
      onGameRemoved as EventListener
    );
    window.addEventListener(
      "hydra:game-files-removed",
      onFilesRemoved as EventListener
    );

    return () => {
      window.removeEventListener(
        "hydra:game-favorite-toggled",
        onFavoriteToggled as EventListener
      );
      window.removeEventListener(
        "hydra:game-removed-from-library",
        onGameRemoved as EventListener
      );
      window.removeEventListener(
        "hydra:game-files-removed",
        onFilesRemoved as EventListener
      );
    };
  }, [updateLibrary, updateGame]);

  const bundled = findBundledGame({ shop, objectId, title: gameTitle });
  const bundledId = bundled?.objectId;
  const [bundledConfigured, setBundledConfigured] = useState(true);

  useEffect(() => {
    if (!bundledId) return;
    const refresh = () =>
      window.electron
        .getBundledGameSetup(bundledId)
        .then((setup) => setBundledConfigured(setup.installed));
    void refresh();
    window.addEventListener(BUNDLED_GAME_CONFIGURED_EVENT, refresh);
    return () =>
      window.removeEventListener(BUNDLED_GAME_CONFIGURED_EVENT, refresh);
  }, [bundledId]);

  const addGameToLibrary = async () => {
    setToggleLibraryGameDisabled(true);

    try {
      await window.electron.addGameToLibrary(
        shop,
        objectId!,
        gameTitle,
        shopDetails?.platform ?? null
      );

      updateLibrary();
      updateGame();
    } finally {
      setToggleLibraryGameDisabled(false);
    }
  };

  const toggleGameFavorite = async () => {
    setToggleLibraryGameDisabled(true);

    try {
      if (game?.favorite && objectId) {
        await window.electron
          .removeGameFromFavorites(shop, objectId)
          .then(() => {
            showSuccessToast(t("game_removed_from_favorites"));
          });
      } else {
        if (!objectId) return;

        await window.electron.addGameToFavorites(shop, objectId).then(() => {
          showSuccessToast(t("game_added_to_favorites"));
        });
      }

      updateLibrary();
      updateGame();
    } finally {
      setToggleLibraryGameDisabled(false);
    }
  };

  const toggleGamePinned = async () => {
    setToggleLibraryGameDisabled(true);

    try {
      if (game?.isPinned && objectId) {
        await window.electron.toggleGamePin(shop, objectId, false).then(() => {
          showSuccessToast(t("game_removed_from_pinned"));
        });
      } else {
        if (!objectId) return;

        await window.electron.toggleGamePin(shop, objectId, true).then(() => {
          showSuccessToast(t("game_added_to_pinned"));
        });
      }

      updateLibrary();
      updateGame();
    } finally {
      setToggleLibraryGameDisabled(false);
    }
  };

  const launchClassicsWithErrorHandling = async (
    discPath?: string,
    force?: boolean
  ): Promise<void> => {
    if (!game) return;
    try {
      await window.electron.openClassicsGame(
        game.shop,
        game.objectId,
        discPath,
        force
      );
    } catch (error) {
      handleClassicsLaunchError(error, {
        t,
        showErrorToast,
        showSuccessToast,
        navigate,
        onEmulatorAlreadyRunning: () => setPendingClassicsLaunch({ discPath }),
      });
    }
  };

  const openClassicsGame = async () => {
    if (!game) return;

    const discs = game.discs ?? [];

    if (
      discs.length === 0 ||
      (discs.length === 1 && game.selectedDiscPath !== null)
    ) {
      await launchClassicsWithErrorHandling();
      return;
    }

    if (game.dontAskDiscSelection && game.selectedDiscPath) {
      await launchClassicsWithErrorHandling(game.selectedDiscPath);
      return;
    }

    setShowDiscSelectionModal(true);
  };

  const handleDiscSelectionConfirm = async (
    discPath: string,
    dontAskAgain: boolean
  ) => {
    if (!game) return;
    setShowDiscSelectionModal(false);
    try {
      await window.electron.updateClassicsDisc(game.shop, game.objectId, {
        selectedDiscPath: discPath,
        dontAskDiscSelection: dontAskAgain,
      });
      updateGame();
    } catch (error) {
      // non-fatal; still try to launch
    }
    await launchClassicsWithErrorHandling(discPath);
  };

  const openGame = async () => {
    if (!game) return;

    if (game.shop === "launchbox") {
      await openClassicsGame();
      return;
    }

    // Bundled games start through their own launcher, so no exe is picked.
    if (game.executablePath || findBundledGame(game)) {
      window.electron.openGame(
        game.shop,
        game.objectId,
        game.executablePath ?? "",
        game.launchOptions
      );
      return;
    }

    const gameExecutablePath = await selectGameExecutable();
    if (gameExecutablePath)
      window.electron.openGame(
        game.shop,
        game.objectId,
        gameExecutablePath,
        game.launchOptions
      );
  };

  const closeGame = () => {
    if (game) window.electron.closeGame(game.shop, game.objectId);
  };

  const deleting = game ? isGameDeleting(game?.id) : false;

  const addGameToLibraryButton = (
    <Button
      theme="outline"
      disabled={toggleLibraryGameDisabled}
      onClick={addGameToLibrary}
      className="hero-panel-actions__action"
    >
      <PlusCircleIcon />
      {t("add_to_library")}
    </Button>
  );

  // A bundled game's "download" is its one-time setup: Chimaera fetches its launcher.
  const bundledInstallButton = (
    <Button
      theme="outline"
      disabled={toggleLibraryGameDisabled || deleting}
      onClick={async () => {
        if (!game) await addGameToLibrary();
        if (bundledId) openBundledGameSetup(bundledId);
      }}
      className="hero-panel-actions__action"
    >
      <DownloadIcon />
      {t("bundled_game_install")}
    </Button>
  );

  const showDownloadOptionsButton = (
    <Button
      onClick={() => setShowRepacksModal(true)}
      theme="outline"
      disabled={deleting}
      className="hero-panel-actions__action"
    >
      {t("open_download_options")}
    </Button>
  );

  const finishedDownloadButton = () => {
    if (!game || !finishedDownloadStep || finishedDownloadStep === "play") {
      return null;
    }

    if (finishedDownloadStep === "checking") {
      return (
        <Button theme="outline" disabled className="hero-panel-actions__action">
          {t("checking_files", { ns: "downloads" })}
        </Button>
      );
    }

    if (finishedDownloadStep === "extracting") {
      return (
        <Button theme="outline" disabled className="hero-panel-actions__action">
          {t("extracting")}
        </Button>
      );
    }

    if (finishedDownloadStep === "extract") {
      return (
        <Button
          onClick={async () => {
            await window.electron
              .extractGameDownload(game.shop, game.objectId)
              .catch((error) =>
                logger.error("Failed to start extraction", error)
              );
            updateLibrary();
          }}
          theme="outline"
          disabled={deleting}
          className="hero-panel-actions__action"
        >
          <FileArchive size={16} />
          {t("extract", { ns: "downloads" })}
        </Button>
      );
    }

    if (finishedDownloadStep === "installing") {
      return (
        <Button theme="outline" disabled className="hero-panel-actions__action">
          {t("installing", { ns: "downloads" })}
        </Button>
      );
    }

    if (finishedDownloadStep === "install") {
      return (
        <Button
          onClick={async () => {
            const opened = await window.electron
              .openGameInstaller(game.shop, game.objectId)
              .catch((error) => {
                logger.error("Failed to open game installer", error);
                return false;
              });

            if (!opened) {
              showErrorToast(t("installer_launch_failed", { ns: "downloads" }));
            }

            updateLibrary();
          }}
          theme="outline"
          disabled={deleting}
          className="hero-panel-actions__action"
        >
          <DownloadIcon />
          {t("install")}
        </Button>
      );
    }

    return (
      <Button
        onClick={async () => {
          const picked = await pickGameExecutable(game.shop, game.objectId);

          if (picked) updateGame();
        }}
        theme="outline"
        disabled={deleting}
        className="hero-panel-actions__action"
      >
        <FileIcon size={16} />
        {t("select_executable", { ns: "downloads" })}
      </Button>
    );
  };

  const gameActionButton = () => {
    if (isTransferring) {
      const percent = Math.round(transferProgress * 100);
      return (
        <Button
          theme="outline"
          className="hero-panel-actions__action"
          onClick={() => {
            setGameOptionsInitialCategory("locations");
            setShowGameOptionsModal(true);
          }}
        >
          Transferring {percent}%
        </Button>
      );
    }

    if (isGameRunning) {
      return (
        <Button
          onClick={closeGame}
          theme="outline"
          disabled={deleting}
          className="hero-panel-actions__action"
        >
          <XCircle size={18} />
          {t("close")}
        </Button>
      );
    }

    const isPlayableClassics =
      game?.shop === "launchbox" && (game?.discs?.length ?? 0) > 0;

    if (bundled && !bundledConfigured) return bundledInstallButton;

    // Copies of this game from other launchers: one Play button per install.
    const executables = game ? linkedExecutables(game) : [];
    if (executables.length > 1 && game) {
      return (
        <>
          {executables.map(({ launcher, executablePath }) => (
            <Button
              key={executablePath}
              onClick={() =>
                window.electron.openGame(
                  game.shop,
                  game.objectId,
                  executablePath,
                  game.launchOptions
                )
              }
              theme="outline"
              disabled={deleting || isGameRunning}
              className="hero-panel-actions__action"
              title={executablePath}
            >
              <PlayIcon />
              {launcher
                ? t("play_with", {
                    launcher: t(`launcher_${launcher}`, { ns: "library" }),
                  })
                : t("play")}
            </Button>
          ))}
        </>
      );
    }

    if (game?.executablePath || isPlayableClassics || findBundledGame(game)) {
      return (
        <Button
          onClick={openGame}
          theme="outline"
          disabled={deleting || isGameRunning}
          className="hero-panel-actions__action"
        >
          <PlayIcon />
          {t("play")}
        </Button>
      );
    }

    const finishedDownloadAction = finishedDownloadButton();
    if (finishedDownloadAction) return finishedDownloadAction;

    return (
      <Button
        onClick={() => setShowRepacksModal(true)}
        theme="outline"
        disabled={isGameDownloading}
        className={`hero-panel-actions__action ${repacks.length === 0 && !canDownloadOnSteam(game) ? "hero-panel-actions__action--disabled" : ""}`}
      >
        <DownloadIcon />
        {t("download")}
      </Button>
    );
  };

  if (repacks.length && !game) {
    return (
      <>
        {addGameToLibraryButton}
        {showDownloadOptionsButton}
      </>
    );
  }

  if (game) {
    return (
      <div className="hero-panel-actions__container">
        {gameActionButton()}
        {bundled?.launcherButton && bundledConfigured && !isGameRunning && (
          <Button
            onClick={() =>
              window.electron.openBundledGameLauncher(bundled.objectId)
            }
            theme="outline"
            disabled={deleting}
            className="hero-panel-actions__action"
          >
            {t("bundled_game_launcher")}
          </Button>
        )}
        <div className="hero-panel-actions__separator" />
        <Button
          onClick={toggleGameFavorite}
          theme="outline"
          disabled={deleting}
          className="hero-panel-actions__action"
        >
          {game.favorite ? <HeartFillIcon /> : <HeartIcon />}
        </Button>

        {userDetails && game.shop !== "custom" && (
          <Button
            onClick={toggleGamePinned}
            theme="outline"
            disabled={deleting}
            className="hero-panel-actions__action"
          >
            {game.isPinned ? <PinSlashIcon /> : <PinIcon />}
          </Button>
        )}

        <Button
          onClick={() => {
            setGameOptionsInitialCategory("general");
            setShowGameOptionsModal(true);
          }}
          theme="outline"
          disabled={deleting}
          className="hero-panel-actions__action"
        >
          <GearIcon />
          {t("options")}
        </Button>

        {game.shop === "launchbox" && (
          <DiscSelectionModal
            visible={showDiscSelectionModal}
            discs={game.discs ?? []}
            defaultDiscPath={game.selectedDiscPath ?? null}
            defaultDontAsk={Boolean(game.dontAskDiscSelection)}
            onClose={() => setShowDiscSelectionModal(false)}
            onConfirm={handleDiscSelectionConfirm}
          />
        )}

        <ConfirmationModal
          visible={pendingClassicsLaunch !== null}
          title={t("rpcs3_already_running_title")}
          descriptionText={t("rpcs3_already_running_description")}
          confirmButtonLabel={t("rpcs3_already_running_confirm")}
          cancelButtonLabel={t("cancel")}
          onClose={() => setPendingClassicsLaunch(null)}
          onConfirm={() => {
            const pending = pendingClassicsLaunch;
            setPendingClassicsLaunch(null);
            if (pending) {
              void launchClassicsWithErrorHandling(pending.discPath, true);
            }
          }}
        />
      </div>
    );
  }

  if (bundled) {
    return (
      <>
        {addGameToLibraryButton}
        {bundledInstallButton}
      </>
    );
  }

  return addGameToLibraryButton;
}
