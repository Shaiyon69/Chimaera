import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { Button, TextField } from "@renderer/components";
import { setUserPreferences } from "@renderer/features";
import { useAppDispatch, useAppSelector } from "@renderer/hooks";
import { levelDBService } from "@renderer/services/leveldb.service";
import type { UserPreferences } from "@types";

import { SettingsGraphicsCache } from "./optiscaler-settings-cache";
import { SettingsOptiscalerProfiles } from "./optiscaler-settings-profiles";

import "./settings-optiscaler.scss";

type DllKey = "optiscalerFsr4ExactPath" | "optiscalerFsr4LossyPath";

/** OptiScaler-wide settings, shown in the Library OptiScaler window (moved from Settings). */
export function OptiscalerSettings() {
  const { t } = useTranslation("settings");
  const dispatch = useAppDispatch();
  // Same as the Settings page's context: save, then refresh the store from disk.
  const updateUserPreferences = async (values: Partial<UserPreferences>) => {
    await window.electron.updateUserPreferences(values);
    const saved = await levelDBService.get("userPreferences", null, "json");
    dispatch(setUserPreferences(saved as UserPreferences | null));
  };
  const prefs = useAppSelector((state) => state.userPreferences.value);

  const variant = prefs?.optiscalerFsr4Variant ?? "exact";
  const exactPath =
    prefs?.optiscalerFsr4ExactPath ?? prefs?.optiscalerFsr4DllPath ?? "";
  const lossyPath = prefs?.optiscalerFsr4LossyPath ?? "";
  const gpuPref = prefs?.optiscalerFsr4Gpu ?? "auto";

  const [gpu, setGpu] = useState<{
    detected: string;
    effective: string;
    autoDlls: { exact: string | null; lossy: string | null };
  }>();
  useEffect(() => {
    setGpu(undefined);
    void window.electron.getOptiscalerGpu().then(setGpu);
  }, [gpuPref]);
  const isNative = (gpu?.effective ?? gpuPref) === "native";

  const chooseDll = async (key: DllKey) => {
    const { filePaths } = await window.electron.showOpenDialog({
      properties: ["openFile"],
      filters: [{ name: "DLL", extensions: ["dll"] }],
    });
    if (!filePaths.length) return;
    await updateUserPreferences({ [key]: filePaths[0] });
  };

  const dllField = (key: DllKey, label: string, value: string) => {
    const auto =
      gpu?.autoDlls[key === "optiscalerFsr4ExactPath" ? "exact" : "lossy"];
    let shown = value || auto;
    if (!shown)
      shown = gpu
        ? t("optiscaler_fsr4_missing")
        : t("optiscaler_fsr4_downloading");
    const suffix = value
      ? t("optiscaler_fsr4_custom")
      : t("optiscaler_fsr4_auto");
    return (
      <TextField
        label={`${label} ${suffix}`}
        value={shown}
        readOnly
        theme="dark"
        rightContent={
          <>
            <Button type="button" onClick={() => void chooseDll(key)}>
              {t("optiscaler_fsr4_choose")}
            </Button>
            <Button
              type="button"
              theme="outline"
              disabled={!value}
              onClick={() =>
                void updateUserPreferences(
                  key === "optiscalerFsr4ExactPath"
                    ? { [key]: null, optiscalerFsr4DllPath: null }
                    : { [key]: null }
                )
              }
            >
              {t("optiscaler_fsr4_clear")}
            </Button>
          </>
        }
      />
    );
  };

  return (
    <div className="settings-optiscaler">
      <section className="settings-optiscaler__card">
        <div className="settings-optiscaler__card-header">
          <h3 className="settings-optiscaler__card-title">
            {t("optiscaler_channel_title")}
          </h3>
          <p className="settings-optiscaler__hint">
            {t("optiscaler_channel_hint")}
          </p>
        </div>
        <div className="settings-optiscaler__variant" role="radiogroup">
          {([false, true] as const).map((nightly) => {
            const active = (prefs?.optiscalerNightly ?? false) === nightly;
            return (
              <button
                key={String(nightly)}
                type="button"
                role="radio"
                aria-checked={active}
                className={`settings-optiscaler__variant-option${
                  active ? " settings-optiscaler__variant-option--active" : ""
                }`}
                onClick={() =>
                  void updateUserPreferences({ optiscalerNightly: nightly })
                }
              >
                {t(
                  nightly
                    ? "optiscaler_channel_nightly"
                    : "optiscaler_channel_stable"
                )}
              </button>
            );
          })}
        </div>
      </section>

      <section className="settings-optiscaler__card">
        <div className="settings-optiscaler__card-header">
          <h3 className="settings-optiscaler__card-title">
            {t("optiscaler_gpu_title")}
          </h3>
          <p className="settings-optiscaler__hint">
            {t("optiscaler_gpu_hint")}
          </p>
        </div>
        <div className="settings-optiscaler__variant" role="radiogroup">
          {(["auto", "native", "desktop", "igpu"] as const).map((g) => (
            <button
              key={g}
              type="button"
              role="radio"
              aria-checked={gpuPref === g}
              className={`settings-optiscaler__variant-option${
                gpuPref === g
                  ? " settings-optiscaler__variant-option--active"
                  : ""
              }`}
              onClick={() =>
                void updateUserPreferences({ optiscalerFsr4Gpu: g })
              }
            >
              {g === "auto"
                ? t("optiscaler_gpu_auto", {
                    gpu: gpu ? t(`optiscaler_gpu_${gpu.detected}`) : "…",
                  })
                : t(`optiscaler_gpu_${g}`)}
            </button>
          ))}
        </div>
      </section>

      {!isNative && (
        <>
          <section className="settings-optiscaler__card">
            <div className="settings-optiscaler__card-header">
              <h3 className="settings-optiscaler__card-title">
                {t("optiscaler_fsr4_dlls")}
              </h3>
              <p className="settings-optiscaler__hint">
                {t("optiscaler_fsr4_hint")}
              </p>
            </div>
            {dllField(
              "optiscalerFsr4ExactPath",
              t("optiscaler_fsr4_exact_path"),
              exactPath
            )}
            {dllField(
              "optiscalerFsr4LossyPath",
              t("optiscaler_fsr4_lossy_path"),
              lossyPath
            )}
          </section>

          <section className="settings-optiscaler__card">
            <h3 className="settings-optiscaler__card-title">
              {t("optiscaler_fsr4_variant")}
            </h3>
            <div className="settings-optiscaler__variant" role="radiogroup">
              {(["exact", "lossy"] as const).map((v) => (
                <button
                  key={v}
                  type="button"
                  role="radio"
                  aria-checked={variant === v}
                  className={`settings-optiscaler__variant-option${
                    variant === v
                      ? " settings-optiscaler__variant-option--active"
                      : ""
                  }`}
                  onClick={() =>
                    void updateUserPreferences({ optiscalerFsr4Variant: v })
                  }
                >
                  {t(`optiscaler_fsr4_${v}`)}
                </button>
              ))}
            </div>
            {variant === "lossy" && (
              <p className="settings-optiscaler__warning">
                {t("optiscaler_fsr4_lossy_warning")}
              </p>
            )}
          </section>
        </>
      )}

      <SettingsOptiscalerProfiles />
      <SettingsGraphicsCache />
    </div>
  );
}
