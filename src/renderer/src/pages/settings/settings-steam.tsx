import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { Button, Modal } from "@renderer/components";
import { useDate, useToast } from "@renderer/hooks";
import { LinkExternalIcon, PersonIcon, SyncIcon } from "@primer/octicons-react";
import { isSteamReconnectRequired } from "@shared";
import { logger } from "@renderer/logger";
import type {
  SteamIntegrationStatus,
  SteamSyncFinishedPayload,
  SteamSyncState,
} from "@types";
import SteamLogo from "@renderer/assets/steam-logo.svg?react";
import { SettingsIntegrationCard } from "./settings-integration-card";
import { getSteamProgressPresentation } from "./settings-integration-progress";
import { getSteamIntegrationPresentation } from "./settings-steam-state";
import {
  getPendingSteamDisconnect,
  runSteamDisconnect,
} from "./steam-disconnect-state";

import "./settings-steam.scss";

const DISCONNECTED_STATUS: SteamIntegrationStatus = {
  connected: false,
  snapshotPreserved: false,
};

const STATUS_ICON_SIZE = 14;
const AVATAR_FALLBACK_ICON_SIZE = 28;

export function SettingsSteam() {
  const { showSuccessToast, showErrorToast } = useToast();
  const { formatDateTime } = useDate();
  const { t } = useTranslation("settings");

  const [isLoading, setIsLoading] = useState(true);
  const [isDisconnecting, setIsDisconnecting] = useState(() =>
    Boolean(getPendingSteamDisconnect())
  );
  const [integration, setIntegration] = useState<SteamIntegrationStatus>(
    () => getPendingSteamDisconnect()?.integration ?? DISCONNECTED_STATUS
  );
  const [avatarError, setAvatarError] = useState(false);
  const [showDeleteDataModal, setShowDeleteDataModal] = useState(false);
  const [syncState, setSyncState] = useState<SteamSyncState>({
    status: "idle",
  });

  const presentation = getSteamIntegrationPresentation(integration, syncState);
  const steamAccount = integration.connected ? integration : null;
  const isSyncing =
    syncState.status === "running" || syncState.status === "cancelling";
  const needsReconnect = presentation.viewState === "reconnect-required";

  useEffect(() => {
    setAvatarError(false);
  }, [steamAccount?.username, steamAccount?.avatarUrl]);

  const getSteamSyncErrorMessage = useCallback(
    (message?: string) => {
      if (message?.endsWith("steam-profile-private")) {
        return t("steam_error_private_profile");
      }

      if (message?.endsWith("steam-upstream-unavailable")) {
        return t("steam_error_steam_unavailable");
      }

      if (message?.endsWith("steam-rate-limited")) {
        return t("steam_error_rate_limited");
      }

      if (message === "steam-session-required") {
        return t("steam_error_session_required");
      }

      if (message === "steam-account-mismatch") {
        return t("steam_error_account_mismatch");
      }

      return t("steam_sync_failed");
    },
    [t]
  );

  const refreshStatus = useCallback(async () => {
    try {
      setIntegration(await window.electron.getSteamIntegrationStatus());
    } catch (error) {
      logger.error(error);
      setIntegration(DISCONNECTED_STATUS);
    } finally {
      setIsLoading(false);
    }
  }, []);

  const handleSync = useCallback(async () => {
    try {
      setSyncState(await window.electron.startSteamSync());
    } catch (error) {
      const message = error instanceof Error ? error.message : undefined;
      if (isSteamReconnectRequired(message)) {
        setSyncState({ status: "idle", requiresReconnect: true });
      }
      showErrorToast(getSteamSyncErrorMessage(message));
    }
  }, [getSteamSyncErrorMessage, showErrorToast]);

  useEffect(() => {
    void refreshStatus();
    void window.electron.getSteamSyncState().then(setSyncState);
  }, [refreshStatus]);

  useEffect(() => {
    const pendingDisconnect = getPendingSteamDisconnect();
    if (!pendingDisconnect) return;

    let cancelled = false;

    void pendingDisconnect.promise
      .catch(() => {})
      .finally(() => {
        if (cancelled) return;

        setIsDisconnecting(false);
        void refreshStatus();
      });

    return () => {
      cancelled = true;
    };
  }, [refreshStatus]);

  useEffect(() => {
    const unsubscribeConnected = window.electron.onSteamConnected(() => {
      setSyncState({ status: "idle" });
      showSuccessToast(t("steam_account_linked"));
      void refreshStatus();
      void handleSync();
    });
    const unsubscribeError = window.electron.onSteamConnectError(() =>
      showErrorToast(t("steam_connect_error"))
    );

    return () => {
      unsubscribeConnected();
      unsubscribeError();
    };
  }, [handleSync, refreshStatus, showErrorToast, showSuccessToast, t]);

  useEffect(() => {
    const unsubscribeProgress =
      window.electron.onSteamSyncProgress(setSyncState);
    const unsubscribeFinished = window.electron.onSteamSyncFinished(
      (payload: SteamSyncFinishedPayload) => {
        setSyncState(
          !payload.ok && isSteamReconnectRequired(payload.message)
            ? { status: "idle", requiresReconnect: true }
            : { status: "idle" }
        );

        if (payload.ok) {
          setIntegration(payload.status);
          if (payload.origin === "manual") {
            showSuccessToast(t("steam_sync_success"));
          }
          return;
        }

        if (
          payload.origin === "manual" &&
          payload.message !== "steam-sync-aborted"
        ) {
          showErrorToast(getSteamSyncErrorMessage(payload.message));
        }
      }
    );

    return () => {
      unsubscribeProgress();
      unsubscribeFinished();
    };
  }, [getSteamSyncErrorMessage, showErrorToast, showSuccessToast, t]);

  const handleConnect = async () => {
    try {
      await window.electron.startSteamOAuth();
    } catch {
      showErrorToast(t("steam_connect_error"));
    }
  };

  const handleDisconnect = async () => {
    setShowDeleteDataModal(false);
    setIsDisconnecting(true);

    try {
      await runSteamDisconnect(integration, () =>
        window.electron.disconnectSteam(true)
      );

      showSuccessToast(t("steam_account_unlinked"));
      await refreshStatus();
    } catch {
      showErrorToast(t("steam_disconnect_error"));
    } finally {
      setIsDisconnecting(false);
    }
  };

  const renderBody = () => {
    if (isLoading) {
      return <p>{t("steam_loading")}</p>;
    }

    if (!steamAccount) {
      return (
        <p className="settings-integration-card__description">
          {t("steam_integration_description")}
        </p>
      );
    }

    const progress = getSteamProgressPresentation(syncState);

    return (
      <>
        <div className="settings-integration-card__profile">
          <div className="settings-integration-card__avatar">
            {steamAccount.avatarUrl && !avatarError ? (
              <img
                src={steamAccount.avatarUrl}
                alt={steamAccount.username}
                onError={() => setAvatarError(true)}
              />
            ) : (
              <PersonIcon size={AVATAR_FALLBACK_ICON_SIZE} />
            )}
          </div>

          <div className="settings-integration-card__account">
            <span className="settings-integration-card__username">
              {steamAccount.username}
            </span>
            <span className="settings-integration-card__meta">
              {steamAccount.lastSyncedAt
                ? t("steam_last_synced", {
                    date: formatDateTime(steamAccount.lastSyncedAt),
                  })
                : t("steam_never_synced")}
            </span>
          </div>
        </div>

        {isDisconnecting ? (
          <output className="settings-integration-card__progress">
            <div className="settings-integration-card__progress-header">
              <span>{t("steam_disconnecting")}</span>
            </div>
            <div className="settings-integration-card__progress-track">
              <div className="settings-integration-card__progress-fill settings-integration-card__progress-fill--indeterminate" />
            </div>
          </output>
        ) : needsReconnect ? (
          <p className="settings-integration-card__message">
            {t("steam_error_session_required")}
          </p>
        ) : isSyncing ? (
          <div className="settings-integration-card__progress" role="status">
            <div className="settings-integration-card__progress-header">
              <span>{t(progress?.labelKey ?? "steam_syncing")}</span>
              {progress?.showCount && syncState.status === "running" ? (
                <span className="settings-integration-card__progress-count">
                  {t("steam_sync_progress", {
                    processed: syncState.gamesProcessed,
                    found: syncState.gamesFound,
                  })}
                </span>
              ) : null}
            </div>
            <div className="settings-integration-card__progress-track">
              <div
                className={`settings-integration-card__progress-fill ${
                  progress?.mode === "determinate"
                    ? ""
                    : "settings-integration-card__progress-fill--indeterminate"
                }`}
                style={
                  progress?.mode === "determinate"
                    ? { width: `${progress.percentage}%` }
                    : undefined
                }
              />
            </div>
          </div>
        ) : null}
      </>
    );
  };

  const renderActions = () => {
    if (isLoading) return null;

    if (!steamAccount) {
      return (
        <Button onClick={handleConnect}>
          <LinkExternalIcon size={STATUS_ICON_SIZE} />
          {t("integration_connect")}
        </Button>
      );
    }

    let primary = (
      <Button theme="outline" onClick={handleSync} disabled={isDisconnecting}>
        <SyncIcon size={STATUS_ICON_SIZE} />
        {t("integration_sync")}
      </Button>
    );

    if (needsReconnect) {
      primary = (
        <Button onClick={handleConnect} disabled={isDisconnecting}>
          <LinkExternalIcon size={STATUS_ICON_SIZE} />
          {t("integration_reconnect")}
        </Button>
      );
    } else if (isSyncing) {
      primary = (
        <Button
          theme="outline"
          onClick={() => window.electron.cancelSteamSync()}
          disabled={syncState.status === "cancelling"}
        >
          {t("steam_sync_cancel")}
        </Button>
      );
    }

    return (
      <>
        {primary}
        <Button
          theme="danger"
          onClick={() => setShowDeleteDataModal(true)}
          disabled={isDisconnecting || isSyncing}
        >
          {t("integration_disconnect")}
        </Button>
      </>
    );
  };

  return (
    <>
      <SettingsIntegrationCard
        title={t("steam")}
        logo={<SteamLogo />}
        status={t(presentation.statusKey)}
        statusTone={presentation.statusTone}
        actions={renderActions()}
        loading={isLoading}
      >
        {renderBody()}
      </SettingsIntegrationCard>

      <Modal
        visible={showDeleteDataModal}
        onClose={() => setShowDeleteDataModal(false)}
        clickOutsideToClose={!isDisconnecting}
        title={t("steam_delete_confirm_title")}
        description={t("steam_delete_confirm_description")}
      >
        <div className="settings-steam__modal-actions">
          <Button
            theme="outline"
            onClick={() => setShowDeleteDataModal(false)}
            disabled={isDisconnecting}
          >
            {t("cancel")}
          </Button>
          <Button
            theme="danger"
            onClick={() => void handleDisconnect()}
            disabled={isDisconnecting}
          >
            {t("steam_delete_confirm_button")}
          </Button>
        </div>
      </Modal>
    </>
  );
}
