import { useCallback, useEffect, useId, useState } from "react";
import { useTranslation } from "react-i18next";

import { Badge, Button, SelectField } from "@renderer/components";
import type { LibraryGame } from "@types";

import { ConfirmBar, Disclosure, Notice, StatusLine } from "./graphics-ui";
import { VersionSelect } from "./version-select";

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
  const headingId = useId();
  const [status, setStatus] = useState<Status | null>(null);
  const [build, setBuild] = useState<Build | null>(null);
  const [version, setVersion] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
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

  const run = async (label: string, action: () => Promise<unknown>) => {
    if (busy) return;
    setBusy(label);
    setError(null);
    setNotice(null);
    setConfirmUninstall(false);
    try {
      await action();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(null);
      onChanged();
    }
  };

  if (!status && !error) return null;

  // Nothing to offer on DX12/Vulkan games: one quiet line instead of a whole section.
  if (status && !status.installed && status.blockedBy === "api")
    return (
      <section
        className="optiscaler optiscaler--compact"
        aria-labelledby={headingId}
      >
        <h3 id={headingId}>{t("dxvk_title")}</h3>
        <p className="optiscaler__hint">
          {t("dxvk_blocked_api", { api: status.api })}
        </p>
      </section>
    );

  let installLabel = t("dxvk_install");
  if (status?.installed)
    installLabel =
      status.updateAvailable && status.build === build
        ? t("dxvk_update", { version: status.latestTag })
        : t("dxvk_reinstall");

  const install = () =>
    void run(t("dxvk_busy_install"), async () => {
      const { version: installed } = await window.electron.installDxvk(
        game.shop,
        game.objectId,
        build ?? "gplasync",
        version || undefined
      );
      setNotice(t("dxvk_installed_notice", { version: installed }));
    });

  return (
    <section className="optiscaler" aria-labelledby={headingId}>
      <div className="optiscaler__head">
        <h3 id={headingId}>{t("dxvk_title")}</h3>
        {status && (
          <span
            className={`optiscaler__state${status.installed ? " optiscaler__state--on" : ""}`}
          >
            {status.installed
              ? t("dxvk_installed", {
                  version: status.version,
                  build: t(`dxvk_build_${status.build ?? "dxvk"}`),
                })
              : t("dxvk_not_installed")}
          </span>
        )}
      </div>
      <p className="optiscaler__hint">{t("dxvk_hint")}</p>

      {status && (
        <>
          <div className="optiscaler__badges">
            <Badge>{t("optiscaler_api", { api: status.api })}</Badge>
            {status.bits && (
              <Badge>{t("dxvk_bits", { bits: status.bits })}</Badge>
            )}
            {status.loaded && <Badge>{t("dxvk_loaded")}</Badge>}
          </div>

          {status.blockedBy ? (
            <Notice tone="warn">
              <p>
                {t(`dxvk_blocked_${status.blockedBy}`, { api: status.api })}
              </p>
            </Notice>
          ) : (
            <>
              {confirmUninstall ? (
                <ConfirmBar
                  tone="warn"
                  confirmLabel={t("dxvk_uninstall")}
                  onConfirm={() =>
                    void run(t("dxvk_busy_uninstall"), () =>
                      window.electron.uninstallDxvk(game.shop, game.objectId)
                    )
                  }
                  onCancel={() => setConfirmUninstall(false)}
                >
                  <p>{t("dxvk_uninstall_confirm")}</p>
                </ConfirmBar>
              ) : (
                <div className="optiscaler__actions">
                  {/* aria-disabled keeps focus on the button while busy. */}
                  <Button
                    theme="primary"
                    aria-disabled={!!busy}
                    onClick={install}
                  >
                    {installLabel}
                  </Button>
                  {status.installed && (
                    <Button
                      theme="outline"
                      aria-disabled={!!busy}
                      onClick={() => !busy && setConfirmUninstall(true)}
                    >
                      {t("dxvk_uninstall")}
                    </Button>
                  )}
                </div>
              )}
              <StatusLine text={busy ?? notice} />

              <Disclosure summary={t("dxvk_install_options")}>
                <SelectField
                  label={t("dxvk_build_label")}
                  value={build ?? ""}
                  disabled={!!busy}
                  options={(["gplasync", "dxvk"] as const).map((b) => ({
                    key: b,
                    value: b,
                    label: t(`dxvk_build_${b}`),
                  }))}
                  onChange={(e) => {
                    setBuild(e.target.value as Build);
                    setVersion("");
                  }}
                />
                {build && (
                  <VersionSelect
                    tool={build}
                    value={version}
                    disabled={!!busy}
                    onChange={setVersion}
                  />
                )}
              </Disclosure>
            </>
          )}
        </>
      )}
      {error && (
        <Notice tone="danger">
          <p>{t("graphics_error", { message: error })}</p>
        </Notice>
      )}
    </section>
  );
}
