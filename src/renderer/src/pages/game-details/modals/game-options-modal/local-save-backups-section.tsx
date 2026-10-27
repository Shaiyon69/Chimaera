import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { GameShop } from "@types";

import { Button, CheckboxField } from "@renderer/components";
import { useDate } from "@renderer/hooks";

import "./local-save-backups-section.scss";

interface LocalSaveBackupsSectionProps {
  shop: GameShop;
  objectId: string;
}

export function LocalSaveBackupsSection({
  shop,
  objectId,
}: Readonly<LocalSaveBackupsSectionProps>) {
  const { t } = useTranslation("game_details");
  const { formatDateTime } = useDate();
  const [backups, setBackups] = useState<LocalSaveBackup[]>([]);
  const [driveBackups, setDriveBackups] = useState<DriveSaveBackup[]>([]);
  const [drive, setDrive] = useState<GoogleDriveStatus>({
    connected: false,
    email: null,
  });
  const [autoSync, setAutoSync] = useState(false);
  const [folder, setFolder] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setBackups(await window.electron.listLocalSaveBackups(shop, objectId));
    setFolder(await window.electron.getSaveBackupsDir());

    const status = await window.electron.getGoogleDriveStatus();
    setDrive(status);
    if (!status.connected) {
      setDriveBackups([]);
      return;
    }

    const game = await window.electron.getGameByObjectId(shop, objectId);
    setAutoSync(!!game?.automaticCloudSync);
    // Drive can be unreachable; the local list above must still show.
    setDriveBackups(
      await window.electron
        .listDriveSaveBackups(shop, objectId)
        .catch(() => [] as DriveSaveBackup[])
    );
  }, [shop, objectId]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const run = async (action: () => Promise<unknown>, done?: string) => {
    setBusy(true);
    setError(null);
    setNotice(null);
    setConfirm(null);
    try {
      await action();
      if (done) setNotice(done);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
      void refresh();
    }
  };

  // Restore overwrites the current saves and delete is final: both take two clicks.
  const twoStep = (key: string, action: () => void) => {
    if (confirm !== key) {
      setConfirm(key);
      return;
    }
    action();
  };

  const describe = (b: LocalSaveBackup) =>
    `${b.label} · ${formatDateTime(b.createdAt)} · ${(b.sizeBytes / 1024 / 1024).toFixed(1)} MB${b.hostname ? ` · ${b.hostname}` : ""}`;

  return (
    <div className="local-save-backups">
      <h3>{t("save_backups_title")}</h3>
      <p>{t("save_backups_hint", { folder })}</p>

      <div className="local-save-backups__actions">
        <Button
          theme="primary"
          disabled={busy}
          onClick={() =>
            void run(
              () => window.electron.createLocalSaveBackup(shop, objectId),
              t("save_backups_created")
            )
          }
        >
          {t("save_backups_create")}
        </Button>
        <Button
          theme="outline"
          disabled={busy}
          onClick={() => void run(() => window.electron.selectSaveBackupsDir())}
        >
          {t("save_backups_change_folder")}
        </Button>
        <Button
          theme="outline"
          disabled={busy}
          onClick={() =>
            void window.electron.openLocalSaveBackupsFolder(shop, objectId)
          }
        >
          {t("save_backups_open_folder")}
        </Button>
      </div>

      {backups.length === 0 && <p>{t("save_backups_empty")}</p>}
      <ul className="local-save-backups__list">
        {backups.map((b) => (
          <li key={b.file} className="local-save-backups__row">
            <span className="local-save-backups__label">{describe(b)}</span>
            {drive.connected && (
              <Button
                theme="outline"
                disabled={busy}
                onClick={() =>
                  void run(
                    () =>
                      window.electron.uploadSaveBackupToDrive(
                        shop,
                        objectId,
                        b.file
                      ),
                    t("save_backups_drive_uploaded")
                  )
                }
              >
                {t("save_backups_drive_upload")}
              </Button>
            )}
            <Button
              theme="outline"
              disabled={busy}
              onClick={() =>
                twoStep(
                  `restore:${b.file}`,
                  () =>
                    void run(
                      () =>
                        window.electron.restoreLocalSaveBackup(
                          shop,
                          objectId,
                          b.file
                        ),
                      t("save_backups_restored")
                    )
                )
              }
            >
              {confirm === `restore:${b.file}`
                ? t("save_backups_restore_confirm")
                : t("save_backups_restore")}
            </Button>
            <Button
              theme="danger"
              disabled={busy}
              onClick={() =>
                twoStep(
                  `delete:${b.file}`,
                  () =>
                    void run(() =>
                      window.electron.deleteLocalSaveBackup(
                        shop,
                        objectId,
                        b.file
                      )
                    )
                )
              }
            >
              {confirm === `delete:${b.file}`
                ? t("save_backups_delete_confirm")
                : t("save_backups_delete")}
            </Button>
          </li>
        ))}
      </ul>

      <h3>{t("save_backups_drive_title")}</h3>
      <p>{t("save_backups_drive_hint")}</p>

      <div className="local-save-backups__actions">
        {drive.connected ? (
          <>
            <span className="local-save-backups__label">
              {drive.email
                ? t("save_backups_drive_connected", { email: drive.email })
                : t("save_backups_drive_connected_no_email")}
            </span>
            <Button
              theme="outline"
              disabled={busy}
              onClick={() =>
                void run(() => window.electron.disconnectGoogleDrive())
              }
            >
              {t("save_backups_drive_disconnect")}
            </Button>
          </>
        ) : (
          <Button
            theme="outline"
            disabled={busy}
            onClick={() =>
              void run(() =>
                window.electron.connectGoogleDrive().catch((err: Error) => {
                  throw new Error(
                    err.message.includes("google-drive/not-configured")
                      ? t("save_backups_drive_not_configured")
                      : err.message
                  );
                })
              )
            }
          >
            {t("save_backups_drive_connect")}
          </Button>
        )}
      </div>

      {drive.connected && (
        <>
          <CheckboxField
            label={t("save_backups_drive_auto")}
            checked={autoSync}
            disabled={busy}
            onChange={(e) => {
              const enabled = e.target.checked;
              setAutoSync(enabled);
              void run(() =>
                window.electron.setAutomaticCloudSync(shop, objectId, enabled)
              );
            }}
          />

          {driveBackups.length === 0 && <p>{t("save_backups_drive_empty")}</p>}
          <ul className="local-save-backups__list">
            {driveBackups.map((b) => (
              <li key={b.driveFileId} className="local-save-backups__row">
                <span className="local-save-backups__label">{describe(b)}</span>
                <Button
                  theme="outline"
                  disabled={busy}
                  onClick={() =>
                    void run(
                      () =>
                        window.electron.downloadDriveSaveBackup(
                          shop,
                          objectId,
                          b.driveFileId
                        ),
                      t("save_backups_drive_downloaded")
                    )
                  }
                >
                  {t("save_backups_drive_download")}
                </Button>
                <Button
                  theme="danger"
                  disabled={busy}
                  onClick={() =>
                    twoStep(
                      `drive-delete:${b.driveFileId}`,
                      () =>
                        void run(() =>
                          window.electron.deleteDriveSaveBackup(b.driveFileId)
                        )
                    )
                  }
                >
                  {confirm === `drive-delete:${b.driveFileId}`
                    ? t("save_backups_delete_confirm")
                    : t("save_backups_delete")}
                </Button>
              </li>
            ))}
          </ul>
        </>
      )}

      {notice && <p>{notice}</p>}
      {error && <p className="local-save-backups__error">{error}</p>}
    </div>
  );
}
