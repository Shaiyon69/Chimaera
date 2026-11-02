import { useCallback, useEffect, useId, useState } from "react";
import { useTranslation } from "react-i18next";

import {
  Badge,
  Button,
  CheckboxField,
  Link,
  SelectField,
} from "@renderer/components";
import type { LibraryGame } from "@types";

import { ConfirmBar, Disclosure, Notice, StatusLine } from "./graphics-ui";
import { OptiscalerConfig } from "./optiscaler-config";
import { VersionSelect } from "./version-select";
import {
  OptiscalerProfilePicker,
  type OptiscalerProfile,
} from "./optiscaler-profiles";

import "./optiscaler-section.scss";

type Status = Awaited<ReturnType<typeof window.electron.getOptiscalerStatus>>;
type Confirm = "install" | "uninstall" | null;

const COMPAT_URL =
  "https://github.com/optiscaler/OptiScaler/wiki/Compatibility-List";

export function OptiscalerSection({
  game,
  revision,
  onChanged,
}: Readonly<{ game: LibraryGame; revision: number; onChanged: () => void }>) {
  const { t } = useTranslation("game_details");
  const headingId = useId();
  const [status, setStatus] = useState<Status | null>(null);
  const [proxy, setProxy] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<Confirm>(null);
  const [profiles, setProfiles] = useState<OptiscalerProfile[]>([]);
  const [profile, setProfile] = useState("default");
  const [version, setVersion] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  const [leftovers, setLeftovers] = useState<string[] | null>(null);

  const loadProfiles = useCallback(async () => {
    setProfiles(await window.electron.getOptiscalerProfiles());
  }, []);

  useEffect(() => {
    void loadProfiles();
  }, [loadProfiles]);

  const refresh = useCallback(async () => {
    try {
      const s = await window.electron.getOptiscalerStatus(
        game.shop,
        game.objectId,
        proxy ?? undefined
      );
      setStatus(s);
      setProxy(
        (current) =>
          current ?? s.proxy ?? (s.api === "vulkan" ? "winmm.dll" : "dxgi.dll")
      );
    } catch (err) {
      setStatus(null);
      setError(err instanceof Error ? err.message : String(err));
    }
  }, [game.shop, game.objectId, proxy]);

  useEffect(() => {
    void refresh();
  }, [refresh, game.executablePath, revision]);

  /** Runs one action; `label` is what the status line says meanwhile. */
  const run = async (label: string, action: () => Promise<unknown>) => {
    if (busy) return;
    setBusy(label);
    setError(null);
    setNotice(null);
    setConfirm(null);
    try {
      await action();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(null);
      onChanged();
    }
  };

  const applyProfile = async () => {
    const { skipped } = await window.electron.applyOptiscalerProfile(
      game.shop,
      game.objectId,
      profile
    );
    if (skipped.length)
      setNotice(t("optiscaler_profile_skipped", { keys: skipped.join(", ") }));
  };

  const doInstall = () =>
    void run(t("optiscaler_busy_install"), async () => {
      const { version: installed } = await window.electron.installOptiscaler(
        game.shop,
        game.objectId,
        proxy ?? "dxgi.dll",
        version || undefined
      );
      if (profile !== "default") await applyProfile();
      setNotice((skipped) =>
        [t("optiscaler_installed_notice", { version: installed }), skipped]
          .filter(Boolean)
          .join(" ")
      );
    });

  // Anti-cheat injection can get the account banned; a foreign proxy DLL gets moved to backup: ask first.
  const install = () =>
    status?.anticheat || status?.proxyConflict
      ? setConfirm("install")
      : doInstall();

  const saveProfile = (name: string) =>
    void run(t("optiscaler_busy_save"), async () => {
      const fields = await window.electron.getOptiscalerConfig(
        game.shop,
        game.objectId
      );
      // Only what differs from OptiScaler's defaults, so a profile stays portable.
      const values = Object.fromEntries(
        fields.filter((f) => f.value !== "auto").map((f) => [f.key, f.value])
      );
      await window.electron.saveOptiscalerProfile(null, name, values);
      await loadProfiles();
      setNotice(t("optiscaler_profile_saved", { name }));
    });

  const findLeftovers = () =>
    void run(t("optiscaler_busy_scan"), async () => {
      const { files } = await window.electron.cleanupOptiscaler(
        game.shop,
        game.objectId,
        false
      );
      setLeftovers(files);
    });

  const moveLeftovers = () =>
    void run(t("optiscaler_busy_cleanup"), async () => {
      const { files } = await window.electron.cleanupOptiscaler(
        game.shop,
        game.objectId,
        true
      );
      setLeftovers(null);
      setNotice(t("optiscaler_cleanup_done", { count: files.length }));
    });

  if (!status && !error) return null;

  const blockedByDxvk = !!status?.dxvkInstalled && !status.installed;
  const reshadeClash = !!status?.reshade && proxy === status.reshade;
  const hasLeftovers = !!status?.leftovers;
  const installBlocked = blockedByDxvk || reshadeClash || hasLeftovers;

  let installLabel = t("optiscaler_install");
  if (status?.installed)
    installLabel = status.updateAvailable
      ? t("optiscaler_update", { version: status.latestTag })
      : t("optiscaler_reinstall");

  const fsr4Label =
    status?.fsr4Gpu === "native"
      ? t("optiscaler_fsr4_native")
      : t("optiscaler_fsr4_lite", {
          gpu: t(`optiscaler_gpu_${status?.fsr4Gpu ?? "desktop"}`),
          variant: t(`optiscaler_fsr4_${status?.fsr4Variant ?? "exact"}`, {
            ns: "settings",
          }),
        });

  return (
    <section className="optiscaler" aria-labelledby={headingId}>
      <div className="optiscaler__head">
        <h3 id={headingId}>{t("optiscaler_title")}</h3>
        {status && (
          <span
            className={`optiscaler__state${status.installed ? " optiscaler__state--on" : ""}`}
          >
            {status.installed
              ? t("optiscaler_installed", {
                  version: status.version,
                  proxy: status.proxy,
                })
              : t("optiscaler_not_installed")}
          </span>
        )}
      </div>
      <p className="optiscaler__hint">{t("optiscaler_lead")}</p>

      {status && (
        <>
          <div className="optiscaler__badges">
            <Badge>{t("optiscaler_api", { api: status.api })}</Badge>
            <Badge>{fsr4Label}</Badge>
            {status.loaded && (
              <Badge>
                {t(
                  status.loaded === "live"
                    ? "optiscaler_fsr4_live"
                    : "optiscaler_loaded"
                )}
              </Badge>
            )}
          </div>

          {blockedByDxvk && (
            <Notice tone="warn">
              <p>{t("optiscaler_blocked_dxvk")}</p>
            </Notice>
          )}
          {reshadeClash && (
            <Notice tone="warn">
              <p>
                {t("optiscaler_reshade_conflict", { proxy: status.reshade })}
              </p>
            </Notice>
          )}
          {hasLeftovers && (
            <Notice tone="warn">
              <p>{t("optiscaler_cleanup_hint")}</p>
              {leftovers === null ? (
                <div className="graphics-notice__actions">
                  <Button theme="outline" onClick={findLeftovers}>
                    {t("optiscaler_cleanup_find")}
                  </Button>
                </div>
              ) : (
                <>
                  <ul className="optiscaler__files">
                    {leftovers.map((f) => (
                      <li key={f}>{f}</li>
                    ))}
                  </ul>
                  <div className="graphics-notice__actions">
                    <Button
                      theme="danger"
                      aria-disabled={!leftovers.length}
                      onClick={() => leftovers.length && moveLeftovers()}
                    >
                      {t("optiscaler_cleanup_move", {
                        count: leftovers.length,
                      })}
                    </Button>
                  </div>
                </>
              )}
            </Notice>
          )}
          {!status.fsr4Available && (
            <p className="optiscaler__hint">{t("optiscaler_no_fsr4")}</p>
          )}

          {confirm === "install" && (
            <ConfirmBar
              confirmLabel={t("optiscaler_install_anyway")}
              onConfirm={doInstall}
              onCancel={() => setConfirm(null)}
            >
              {status.anticheat && (
                <p>
                  {t("optiscaler_anticheat_warning", {
                    name: status.anticheat,
                  })}
                </p>
              )}
              {status.proxyConflict && (
                <p>
                  {t("optiscaler_proxy_conflict_warning", {
                    proxy: status.proxyConflict,
                  })}
                </p>
              )}
            </ConfirmBar>
          )}
          {confirm === "uninstall" && (
            <ConfirmBar
              tone="warn"
              confirmLabel={t("optiscaler_uninstall")}
              onConfirm={() =>
                void run(t("optiscaler_busy_uninstall"), () =>
                  window.electron.uninstallOptiscaler(game.shop, game.objectId)
                )
              }
              onCancel={() => setConfirm(null)}
            >
              <p>{t("optiscaler_uninstall_confirm")}</p>
            </ConfirmBar>
          )}

          {!confirm && (
            <div className="optiscaler__actions">
              {/* aria-disabled keeps focus on the button while busy; disabled would drop it. */}
              <Button
                theme="primary"
                aria-disabled={!!busy || installBlocked}
                onClick={() => !busy && !installBlocked && install()}
              >
                {installLabel}
              </Button>
              {status.installed && (
                <Button
                  theme="outline"
                  aria-disabled={!!busy}
                  onClick={() => !busy && setConfirm("uninstall")}
                >
                  {t("optiscaler_uninstall")}
                </Button>
              )}
            </div>
          )}
          <StatusLine text={busy ?? notice} />

          <Disclosure summary={t("optiscaler_install_options")}>
            <SelectField
              label={t("optiscaler_proxy_label")}
              value={proxy ?? ""}
              disabled={!!busy}
              options={status.proxies.map((p) => ({
                key: p,
                value: p,
                label: p,
              }))}
              onChange={(e) => {
                setProxy(e.target.value);
                setConfirm(null);
              }}
            />
            <p className="optiscaler__hint">{t("optiscaler_proxy_hint")}</p>
            <VersionSelect
              tool="optiscaler"
              value={version}
              disabled={!!busy}
              onChange={setVersion}
            />
            <OptiscalerProfilePicker
              profiles={profiles}
              value={profile}
              installed={status.installed}
              busy={!!busy}
              onChange={setProfile}
              onApply={() =>
                void run(t("optiscaler_busy_apply"), async () => {
                  await applyProfile();
                  setNotice(
                    (skipped) => skipped ?? t("optiscaler_profile_applied")
                  );
                })
              }
              onSave={saveProfile}
            />
          </Disclosure>

          {status.installed && (
            <Disclosure summary={t("optiscaler_advanced")}>
              <CheckboxField
                label={t("optiscaler_optipatcher")}
                checked={status.optipatcher}
                onChange={(e) =>
                  void run(t("optiscaler_busy_optipatcher"), () =>
                    window.electron.setOptiPatcher(
                      game.shop,
                      game.objectId,
                      e.target.checked
                    )
                  )
                }
              />
              <p className="optiscaler__hint">
                {t("optiscaler_optipatcher_hint")}
              </p>
              <OptiscalerConfig
                game={game}
                native={status.fsr4Gpu === "native"}
                revision={revision}
                disabled={!!busy}
              />
            </Disclosure>
          )}

          <p className="optiscaler__hint">
            {t("optiscaler_compat_hint")}{" "}
            <Link to={COMPAT_URL}>{t("optiscaler_compat_link")}</Link>
          </p>
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
