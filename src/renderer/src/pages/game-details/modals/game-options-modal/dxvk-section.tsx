import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { Badge, Button, SelectField } from "@renderer/components";
import type { LibraryGame } from "@types";

// Same layout as the OptiScaler section above it; reuses its styles.
import "./optiscaler-section.scss";

type Status = Awaited<ReturnType<typeof window.electron.getDxvkStatus>>;
type Build = "dxvk" | "gplasync";

export function DxvkSection({
  game,
  revision,
  onChanged,
}: Readonly<{ game: LibraryGame; revision: number; onChanged: () => void }>) {
  const { t } = useTranslation("game_details");
  const [status, setStatus] = useState<Status | null>(null);
  const [build, setBuild] = useState<Build | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmUninstall, setConfirmUninstall] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const s = await window.electron.getDxvkStatus(game.shop, game.objectId);
      setStatus(s);
      setBuild((current) => current ?? s.build ?? "gplasync");
    } catch (err) {
      setStatus(null);
      setError(err instanceof Error ? err.message : String(err));
    }
  }, [game.shop, game.objectId]);

  useEffect(() => {
    void refresh();
  }, [refresh, game.executablePath, revision]);

  const run = async (action: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    setConfirmUninstall(false);
    try {
      await action();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
      onChanged();
    }
  };

  const uninstall = () => {
    if (!confirmUninstall) {
      setConfirmUninstall(true);
      return;
    }
    void run(() => window.electron.uninstallDxvk(game.shop, game.objectId));
  };

  if (!status && !error) return null;

  let installLabel = t("dxvk_install");
  if (status?.installed)
    installLabel =
      status.updateAvailable && status.build === build
        ? t("dxvk_update", { version: status.latestTag })
        : t("dxvk_reinstall");

  return (
    <div className="optiscaler">
      <h3>{t("dxvk_title")}</h3>
      {status && (
        <>
          <div className="optiscaler__badges">
            <Badge>
              {status.installed
                ? t("dxvk_installed", {
                    version: status.version,
                    build: t(`dxvk_build_${status.build ?? "dxvk"}`),
                  })
                : t("dxvk_not_installed")}
            </Badge>
            <Badge>{t("optiscaler_api", { api: status.api })}</Badge>
            {status.bits && (
              <Badge>{t("dxvk_bits", { bits: status.bits })}</Badge>
            )}
            {status.loaded && <Badge>{t("dxvk_loaded")}</Badge>}
          </div>
          <p>{t("dxvk_hint")}</p>
          {status.blockedBy ? (
            <p className="optiscaler__warning">
              {t(`dxvk_blocked_${status.blockedBy}`, { api: status.api })}
            </p>
          ) : (
            <>
              <SelectField
                label={t("dxvk_build_label")}
                value={build ?? ""}
                disabled={busy}
                options={(["gplasync", "dxvk"] as const).map((b) => ({
                  key: b,
                  value: b,
                  label: t(`dxvk_build_${b}`),
                }))}
                onChange={(e) => setBuild(e.target.value as Build)}
              />
              {confirmUninstall && (
                <p className="optiscaler__warning">
                  {t("dxvk_uninstall_confirm")}
                </p>
              )}
              <div className="optiscaler__actions">
                <Button
                  theme="primary"
                  disabled={busy || !build}
                  onClick={() =>
                    void run(() =>
                      window.electron.installDxvk(
                        game.shop,
                        game.objectId,
                        build ?? "gplasync"
                      )
                    )
                  }
                >
                  {installLabel}
                </Button>
                {status.installed && (
                  <Button theme="danger" disabled={busy} onClick={uninstall}>
                    {t("dxvk_uninstall")}
                  </Button>
                )}
              </div>
            </>
          )}
        </>
      )}
      {error && <p className="optiscaler__error">{error}</p>}
    </div>
  );
}
