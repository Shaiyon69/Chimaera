import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { Button, TextField } from "@renderer/components";
import type { OptiscalerProfile } from "@renderer/pages/game-details/modals/game-options-modal/optiscaler-profiles";

export function SettingsOptiscalerProfiles() {
  const { t } = useTranslation("settings");
  const [profiles, setProfiles] = useState<OptiscalerProfile[]>([]);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    const all = await window.electron.getOptiscalerProfiles();
    setProfiles(all.filter((p) => !p.builtin));
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const act = async (action: () => Promise<unknown>) => {
    setError(null);
    try {
      await action();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
    void load();
  };

  return (
    <section className="settings-optiscaler__card">
      <div className="settings-optiscaler__card-header">
        <h3 className="settings-optiscaler__card-title">
          {t("optiscaler_profiles_title")}
        </h3>
        <p className="settings-optiscaler__hint">
          {t("optiscaler_profiles_hint")}
        </p>
      </div>
      {!profiles.length && (
        <p className="settings-optiscaler__hint">
          {t("optiscaler_profiles_empty")}
        </p>
      )}
      {profiles.map((p) => (
        <TextField
          key={p.id}
          theme="dark"
          defaultValue={p.name ?? ""}
          maxLength={60}
          label={t("optiscaler_profiles_settings", {
            count: Object.keys(p.values).length,
          })}
          onBlur={(e) => {
            const name = e.target.value.trim();
            if (name && name !== p.name)
              void act(() =>
                window.electron.saveOptiscalerProfile(p.id, name, p.values)
              );
          }}
          rightContent={
            <Button
              theme="danger"
              onClick={() => {
                if (confirmDelete !== p.id) {
                  setConfirmDelete(p.id);
                  return;
                }
                setConfirmDelete(null);
                void act(() => window.electron.deleteOptiscalerProfile(p.id));
              }}
            >
              {confirmDelete === p.id
                ? t("optiscaler_profiles_delete_confirm")
                : t("optiscaler_profiles_delete")}
            </Button>
          }
        />
      ))}
      {error && <p className="settings-optiscaler__warning">{error}</p>}
    </section>
  );
}
