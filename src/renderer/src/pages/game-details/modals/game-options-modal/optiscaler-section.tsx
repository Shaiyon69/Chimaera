import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import {
  Badge,
  Button,
  CheckboxField,
  SelectField,
} from "@renderer/components";
import type { LibraryGame } from "@types";

import { OptiscalerConfig } from "./optiscaler-config";
import { VersionSelect } from "./version-select";
import {
  OptiscalerProfilePicker,
  type OptiscalerProfile,
} from "./optiscaler-profiles";

import "./optiscaler-section.scss";

type Status = Awaited<ReturnType<typeof window.electron.getOptiscalerStatus>>;
type Confirm = "install" | "uninstall" | null;

export function OptiscalerSection({
  game,
  revision,
  onChanged,
}: Readonly<{ game: LibraryGame; revision: number; onChanged: () => void }>) {
  const { t } = useTranslation("game_details");
  const [status, setStatus] = useState<Status | null>(null);
  const [proxy, setProxy] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
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

  const run = async (action: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    setNotice(null);
    setConfirm(null);
    try {
      await action();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
      onChanged();
    }
  };

  const install = () => {
    // Anti-cheat injection can get the account banned; a foreign proxy DLL gets moved to backup: ask first.
    if ((status?.anticheat || status?.proxyConflict) && confirm !== "install") {
      setConfirm("install");
      return;
    }
    void run(async () => {
      await window.electron.installOptiscaler(
        game.shop,
        game.objectId,
        proxy ?? "dxgi.dll",
        version || undefined
      );
      if (profile !== "default") await applyProfile();
    });
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

  const saveProfile = (name: string) =>
    void run(async () => {
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
    });

  const uninstall = () => {
    if (confirm !== "uninstall") {
      setConfirm("uninstall");
      return;
    }
    void run(() =>
      window.electron.uninstallOptiscaler(game.shop, game.objectId)
    );
  };

  const findLeftovers = () =>
    void run(async () => {
      const { files } = await window.electron.cleanupOptiscaler(
        game.shop,
        game.objectId,
        false
      );
      setLeftovers(files);
    });

  const moveLeftovers = () =>
    void run(async () => {
      const { files } = await window.electron.cleanupOptiscaler(
        game.shop,
        game.objectId,
        true
      );
      setLeftovers(null);
      setNotice(t("optiscaler_cleanup_done", { count: files.length }));
    });

  if (!status && !error) return null;

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
    <div className="optiscaler">
      <h3>{t("optiscaler_title")}</h3>
      {status && (
        <>
          <div className="optiscaler__badges">
            <Badge>
              {status.installed
                ? t("optiscaler_installed", {
                    version: status.version,
                    proxy: status.proxy,
                  })
                : t("optiscaler_not_installed")}
            </Badge>
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
          <SelectField
            label={t("optiscaler_proxy_label")}
            value={proxy ?? ""}
            disabled={busy}
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
          <VersionSelect
            tool="optiscaler"
            value={version}
            disabled={busy}
            onChange={setVersion}
          />
          <OptiscalerProfilePicker
            profiles={profiles}
            value={profile}
            installed={status.installed}
            busy={busy}
            onChange={setProfile}
            onApply={() => void run(applyProfile)}
            onSave={saveProfile}
          />
          {!status.fsr4Available && (
            <p className="optiscaler__warning">{t("optiscaler_no_fsr4")}</p>
          )}
          {confirm === "install" && status.anticheat && (
            <p className="optiscaler__warning">
              {t("optiscaler_anticheat_warning", { name: status.anticheat })}
            </p>
          )}
          {confirm === "install" && status.proxyConflict && (
            <p className="optiscaler__warning">
              {t("optiscaler_proxy_conflict_warning", {
                proxy: status.proxyConflict,
              })}
            </p>
          )}
          {confirm === "uninstall" && (
            <p className="optiscaler__warning">
              {t("optiscaler_uninstall_confirm")}
            </p>
          )}
          {status.reshade && proxy === status.reshade && (
            <p className="optiscaler__warning">
              {t("optiscaler_reshade_conflict", { proxy: status.reshade })}
            </p>
          )}
          {status.leftovers && (
            <div className="optiscaler__extras">
              <p className="optiscaler__warning">
                {t("optiscaler_cleanup_hint")}
              </p>
              {leftovers === null ? (
                <div className="optiscaler__actions">
                  <Button
                    theme="outline"
                    disabled={busy}
                    onClick={findLeftovers}
                  >
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
                  <div className="optiscaler__actions">
                    <Button
                      theme="danger"
                      disabled={busy || !leftovers.length}
                      onClick={moveLeftovers}
                    >
                      {t("optiscaler_cleanup_move", {
                        count: leftovers.length,
                      })}
                    </Button>
                  </div>
                </>
              )}
            </div>
          )}
          {status.dxvkInstalled && !status.installed && (
            <p className="optiscaler__warning">
              {t("optiscaler_blocked_dxvk")}
            </p>
          )}
          <div className="optiscaler__actions">
            <Button
              theme="primary"
              disabled={
                busy ||
                (status.dxvkInstalled && !status.installed) ||
                (!!status.reshade && proxy === status.reshade)
              }
              onClick={install}
            >
              {installLabel}
            </Button>
            {status.installed && (
              <Button theme="danger" disabled={busy} onClick={uninstall}>
                {t("optiscaler_uninstall")}
              </Button>
            )}
          </div>
        </>
      )}
      {status?.installed && (
        <div className="optiscaler__extras">
          <CheckboxField
            label={t("optiscaler_optipatcher")}
            checked={status.optipatcher}
            disabled={busy}
            onChange={(e) =>
              void run(() =>
                window.electron.setOptiPatcher(
                  game.shop,
                  game.objectId,
                  e.target.checked
                )
              )
            }
          />
          <p className="optiscaler__hint">{t("optiscaler_optipatcher_hint")}</p>
        </div>
      )}
      {status?.installed && !busy && (
        <OptiscalerConfig game={game} native={status.fsr4Gpu === "native"} />
      )}
      {notice && <p className="optiscaler__hint">{notice}</p>}
      {error && <p className="optiscaler__error">{error}</p>}
    </div>
  );
}
