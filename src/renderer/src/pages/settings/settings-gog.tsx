import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { Button, Modal } from "@renderer/components";
import { useDate, useToast } from "@renderer/hooks";
import { LinkExternalIcon, PersonIcon, SyncIcon } from "@primer/octicons-react";
import { Gamepad2 } from "lucide-react";
import { logger } from "@renderer/logger";
import type {
  GogIntegrationStatus,
  GogSyncFinishedPayload,
  GogSyncState,
} from "@types";
import { SettingsIntegrationCard } from "./settings-integration-card";

import "./settings-steam.scss";

const DISCONNECTED: GogIntegrationStatus = { connected: false };
const ICON_SIZE = 14;
const AVATAR_FALLBACK_ICON_SIZE = 28;

const isReconnectMessage = (message?: string) =>
  message === "gog-session-required" || message === "gog-account-mismatch";

export function SettingsGog() {
  const { showSuccessToast, showErrorToast } = useToast();
  const { formatDateTime } = useDate();
  const { t } = useTranslation("settings");

  const [isLoading, setIsLoading] = useState(true);
  const [isDisconnecting, setIsDisconnecting] = useState(false);
  const [integration, setIntegration] =
    useState<GogIntegrationStatus>(DISCONNECTED);
  const [avatarError, setAvatarError] = useState(false);
  const [showDisconnectModal, setShowDisconnectModal] = useState(false);
  const [syncState, setSyncState] = useState<GogSyncState>({ status: "idle" });

  const account = integration.connected ? integration : null;
  const isSyncing =
    syncState.status === "running" || syncState.status === "cancelling";
  const needsReconnect =
    syncState.status === "idle" && syncState.requiresReconnect === true;

  const errorMessage = useCallback(
    (message?: string) => {
      if (message === "gog-session-required")
        return t("gog_error_session_required");
      if (message === "gog-account-mismatch")
        return t("gog_error_account_mismatch");
      if (message === "gog-rate-limited") return t("gog_error_rate_limited");
      return t("gog_sync_failed");
    },
    [t]
  );

  const refreshStatus = useCallback(async () => {
    try {
      setIntegration(await window.electron.getGogIntegrationStatus());
    } catch (error) {
      logger.error(error);
      setIntegration(DISCONNECTED);
    } finally {
      setIsLoading(false);
    }
  }, []);

  const handleSync = useCallback(async () => {
    try {
      setSyncState(await window.electron.startGogSync());
    } catch (error) {
      showErrorToast(
        errorMessage(error instanceof Error ? error.message : undefined)
      );
    }
  }, [errorMessage, showErrorToast]);

  useEffect(() => {
    void refreshStatus();
    void window.electron.getGogSyncState().then(setSyncState);
  }, [refreshStatus]);

  useEffect(() => {
    setAvatarError(false);
  }, [account?.avatarUrl]);

  useEffect(() => {
    const offConnected = window.electron.onGogConnected(() => {
      setSyncState({ status: "idle" });
      showSuccessToast(t("gog_account_linked"));
      void refreshStatus();
      void handleSync();
    });
    const offError = window.electron.onGogConnectError(() =>
      showErrorToast(t("gog_connect_error"))
    );
    const offProgress = window.electron.onGogSyncProgress(setSyncState);
    const offFinished = window.electron.onGogSyncFinished(
      (payload: GogSyncFinishedPayload) => {
        setSyncState(
          !payload.ok && isReconnectMessage(payload.message)
            ? { status: "idle", requiresReconnect: true }
            : { status: "idle" }
        );
        if (payload.ok) {
          setIntegration(payload.status);
          if (payload.origin === "manual")
            showSuccessToast(t("gog_sync_success"));
        } else if (
          payload.origin === "manual" &&
          payload.message !== "gog-sync-aborted"
        ) {
          showErrorToast(errorMessage(payload.message));
        }
      }
    );

    return () => {
      offConnected();
      offError();
      offProgress();
      offFinished();
    };
  }, [
    errorMessage,
    handleSync,
    refreshStatus,
    showErrorToast,
    showSuccessToast,
    t,
  ]);

  const handleConnect = async () => {
    try {
      await window.electron.startGogOAuth();
    } catch {
      showErrorToast(t("gog_connect_error"));
    }
  };

  const handleDisconnect = async (deleteImportedData: boolean) => {
    setShowDisconnectModal(false);
    setIsDisconnecting(true);
    try {
      await window.electron.disconnectGog(deleteImportedData);
      showSuccessToast(t("gog_account_unlinked"));
      await refreshStatus();
    } catch {
      showErrorToast(t("gog_disconnect_error"));
    } finally {
      setIsDisconnecting(false);
    }
  };

  const renderProgress = () => {
    if (isDisconnecting) {
      return (
        <output className="settings-integration-card__progress">
          <div className="settings-integration-card__progress-header">
            <span>{t("gog_disconnecting")}</span>
          </div>
          <div className="settings-integration-card__progress-track">
            <div className="settings-integration-card__progress-fill settings-integration-card__progress-fill--indeterminate" />
          </div>
        </output>
      );
    }
    if (needsReconnect) {
      return (
        <p className="settings-integration-card__message">
          {t("gog_error_session_required")}
        </p>
      );
    }
    if (!isSyncing) return null;

    const running = syncState.status === "running" ? syncState : null;
    const percentage =
      running && running.gamesFound > 0
        ? Math.round((running.gamesProcessed / running.gamesFound) * 100)
        : null;

    return (
      <div className="settings-integration-card__progress" role="status">
        <div className="settings-integration-card__progress-header">
          <span>{t(`gog_sync_phase_${running?.phase ?? "starting"}`)}</span>
          {percentage !== null && running ? (
            <span className="settings-integration-card__progress-count">
              {t("steam_sync_progress", {
                processed: running.gamesProcessed,
                found: running.gamesFound,
              })}
            </span>
          ) : null}
        </div>
        <div className="settings-integration-card__progress-track">
          <div
            className={`settings-integration-card__progress-fill ${
              percentage === null
                ? "settings-integration-card__progress-fill--indeterminate"
                : ""
            }`}
            style={
              percentage === null ? undefined : { width: `${percentage}%` }
            }
          />
        </div>
      </div>
    );
  };

  const renderBody = () => {
    if (!account) {
      return (
        <p className="settings-integration-card__description">
          {t("gog_integration_description")}
        </p>
      );
    }

    return (
      <>
        <div className="settings-integration-card__profile">
          <div className="settings-integration-card__avatar">
            {account.avatarUrl && !avatarError ? (
              <img
                src={account.avatarUrl}
                alt={account.username}
                onError={() => setAvatarError(true)}
              />
            ) : (
              <PersonIcon size={AVATAR_FALLBACK_ICON_SIZE} />
            )}
          </div>
          <div className="settings-integration-card__account">
            <span className="settings-integration-card__username">
              {account.username}
            </span>
            <span className="settings-integration-card__meta">
              {account.lastSyncedAt
                ? t("steam_last_synced", {
                    date: formatDateTime(account.lastSyncedAt),
                  })
                : t("steam_never_synced")}
            </span>
          </div>
        </div>
        {renderProgress()}
      </>
    );
  };

  const renderActions = () => {
    if (!account) {
      return (
        <Button onClick={handleConnect}>
          <LinkExternalIcon size={ICON_SIZE} />
          {t("integration_connect")}
        </Button>
      );
    }

    let primary = (
      <Button theme="outline" onClick={handleSync} disabled={isDisconnecting}>
        <SyncIcon size={ICON_SIZE} />
        {t("integration_sync")}
      </Button>
    );
    if (needsReconnect) {
      primary = (
        <Button onClick={handleConnect} disabled={isDisconnecting}>
          <LinkExternalIcon size={ICON_SIZE} />
          {t("integration_reconnect")}
        </Button>
      );
    } else if (isSyncing) {
      primary = (
        <Button
          theme="outline"
          onClick={() => window.electron.cancelGogSync()}
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
          onClick={() => setShowDisconnectModal(true)}
          disabled={isDisconnecting || isSyncing}
        >
          {t("integration_disconnect")}
        </Button>
      </>
    );
  };

  let status = t("gog_status_disconnected");
  let tone: "neutral" | "success" | "warning" = "neutral";
  if (needsReconnect) {
    status = t("steam_status_reconnect_required");
    tone = "warning";
  } else if (account) {
    status = t("steam_status_connected");
    tone = "success";
  }

  return (
    <>
      <SettingsIntegrationCard
        title={t("gog")}
        logo={<Gamepad2 size={20} />}
        status={status}
        statusTone={tone}
        actions={renderActions()}
        loading={isLoading}
      >
        {renderBody()}
      </SettingsIntegrationCard>

      <Modal
        visible={showDisconnectModal}
        onClose={() => setShowDisconnectModal(false)}
        clickOutsideToClose={!isDisconnecting}
        title={t("gog_disconnect_title")}
        description={t("gog_disconnect_description")}
      >
        <div className="settings-steam__modal-actions">
          <Button
            theme="outline"
            onClick={() => void handleDisconnect(false)}
            disabled={isDisconnecting}
          >
            {t("gog_disconnect_keep")}
          </Button>
          <Button
            theme="danger"
            onClick={() => void handleDisconnect(true)}
            disabled={isDisconnecting}
          >
            {t("steam_delete_confirm_button")}
          </Button>
        </div>
      </Modal>
    </>
  );
}
