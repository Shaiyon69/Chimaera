import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { Button, Modal } from "@renderer/components";
import { useDate, useToast } from "@renderer/hooks";
import { LinkExternalIcon, PersonIcon, SyncIcon } from "@primer/octicons-react";
import { Gamepad2 } from "lucide-react";
import { logger } from "@renderer/logger";
import type {
  EpicIntegrationStatus,
  EpicSyncFinishedPayload,
  EpicSyncState,
} from "@types";
import { SettingsIntegrationCard } from "./settings-integration-card";

import "./settings-steam.scss";

const DISCONNECTED: EpicIntegrationStatus = { connected: false };
const ICON_SIZE = 14;
const AVATAR_FALLBACK_ICON_SIZE = 28;

const isReconnectMessage = (message?: string) =>
  message === "epic-session-required" || message === "epic-account-mismatch";

export function SettingsEpic() {
  const { showSuccessToast, showErrorToast } = useToast();
  const { formatDateTime } = useDate();
  const { t } = useTranslation("settings");

  const [isLoading, setIsLoading] = useState(true);
  const [isDisconnecting, setIsDisconnecting] = useState(false);
  const [integration, setIntegration] =
    useState<EpicIntegrationStatus>(DISCONNECTED);
  const [showDisconnectModal, setShowDisconnectModal] = useState(false);
  const [syncState, setSyncState] = useState<EpicSyncState>({ status: "idle" });

  const account = integration.connected ? integration : null;
  const isSyncing =
    syncState.status === "running" || syncState.status === "cancelling";
  const needsReconnect =
    syncState.status === "idle" && syncState.requiresReconnect === true;

  const errorMessage = useCallback(
    (message?: string) => {
      if (message === "epic-session-required")
        return t("epic_error_session_required");
      if (message === "epic-account-mismatch")
        return t("epic_error_account_mismatch");
      if (message === "epic-rate-limited") return t("epic_error_rate_limited");
      return t("epic_sync_failed");
    },
    [t]
  );

  const refreshStatus = useCallback(async () => {
    try {
      setIntegration(await window.electron.getEpicConnection());
    } catch (error) {
      logger.error(error);
      setIntegration(DISCONNECTED);
    } finally {
      setIsLoading(false);
    }
  }, []);

  const handleSync = useCallback(async () => {
    try {
      setSyncState(await window.electron.startEpicSync());
    } catch (error) {
      showErrorToast(
        errorMessage(error instanceof Error ? error.message : undefined)
      );
    }
  }, [errorMessage, showErrorToast]);

  useEffect(() => {
    void refreshStatus();
    void window.electron.getEpicSyncState().then(setSyncState);
  }, [refreshStatus]);

  useEffect(() => {
    const offConnected = window.electron.onEpicConnected(() => {
      setSyncState({ status: "idle" });
      showSuccessToast(t("epic_account_linked"));
      void refreshStatus();
      void handleSync();
    });
    const offError = window.electron.onEpicConnectError(() =>
      showErrorToast(t("epic_connect_error"))
    );
    const offProgress = window.electron.onEpicSyncProgress(setSyncState);
    const offFinished = window.electron.onEpicSyncFinished(
      (payload: EpicSyncFinishedPayload) => {
        setSyncState(
          !payload.ok && isReconnectMessage(payload.message)
            ? {
                status: "idle",
                requiresReconnect: true,
                reason: payload.message,
              }
            : { status: "idle" }
        );
        if (payload.ok) {
          setIntegration(payload.status);
          if (payload.origin === "manual")
            showSuccessToast(t("epic_sync_success"));
        } else if (
          payload.origin === "manual" &&
          payload.message !== "epic-sync-aborted"
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
      await window.electron.startEpicAuth();
    } catch {
      showErrorToast(t("epic_connect_error"));
    }
  };

  const handleDisconnect = async (deleteImportedData: boolean) => {
    setShowDisconnectModal(false);
    setIsDisconnecting(true);
    try {
      await window.electron.disconnectEpic(deleteImportedData);
      showSuccessToast(t("epic_account_unlinked"));
      await refreshStatus();
    } catch {
      showErrorToast(t("epic_disconnect_error"));
    } finally {
      setIsDisconnecting(false);
    }
  };

  const renderProgress = () => {
    if (isDisconnecting) {
      return (
        <output className="settings-integration-card__progress">
          <div className="settings-integration-card__progress-header">
            <span>{t("epic_disconnecting")}</span>
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
          {errorMessage(
            syncState.status === "idle" ? syncState.reason : undefined
          )}
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
          <span>{t(`epic_sync_phase_${running?.phase ?? "starting"}`)}</span>
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
          {t("epic_integration_description")}
        </p>
      );
    }

    return (
      <>
        <div className="settings-integration-card__profile">
          <div className="settings-integration-card__avatar">
            <PersonIcon size={AVATAR_FALLBACK_ICON_SIZE} />
          </div>
          <div className="settings-integration-card__account">
            <span className="settings-integration-card__username">
              {account.displayName}
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
          onClick={() => window.electron.cancelEpicSync()}
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

  let status = t("epic_status_disconnected");
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
        title={t("epic")}
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
        title={t("epic_disconnect_title")}
        description={t("epic_disconnect_description")}
      >
        <div className="settings-steam__modal-actions">
          <Button
            theme="outline"
            onClick={() => void handleDisconnect(false)}
            disabled={isDisconnecting}
          >
            {t("epic_disconnect_keep")}
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
