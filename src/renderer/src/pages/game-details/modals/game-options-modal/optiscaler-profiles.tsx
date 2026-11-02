import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";

import { Button, SelectField, TextField } from "@renderer/components";

export type OptiscalerProfile = Awaited<
  ReturnType<typeof window.electron.getOptiscalerProfiles>
>[number];

export const profileName = (t: TFunction, p: OptiscalerProfile) =>
  p.builtin
    ? t(`optiscaler_profile_${p.id}`, { ns: "game_details" })
    : (p.name ?? p.id);

export function OptiscalerProfilePicker({
  profiles,
  value,
  installed,
  busy,
  onChange,
  onApply,
  onSave,
}: Readonly<{
  profiles: OptiscalerProfile[];
  value: string;
  installed: boolean;
  busy: boolean;
  onChange: (id: string) => void;
  onApply: () => void;
  onSave: (name: string) => void;
}>) {
  const { t } = useTranslation("game_details");
  const [name, setName] = useState<string | null>(null);
  const selected = profiles.find((p) => p.id === value);

  return (
    <div className="optiscaler__extras">
      <SelectField
        label={t("optiscaler_profile_label")}
        value={value}
        disabled={busy}
        options={profiles.map((p) => ({
          key: p.id,
          value: p.id,
          label: profileName(t, p),
        }))}
        onChange={(e) => onChange(e.target.value)}
      />
      {selected?.id === "amd-fg" && (
        <p className="optiscaler__hint">
          {t("optiscaler_profile_amd-fg_hint")}
        </p>
      )}
      {installed && (
        <div className="optiscaler__actions">
          <Button
            theme="outline"
            disabled={busy || value === "default"}
            onClick={onApply}
          >
            {t("optiscaler_profile_apply")}
          </Button>
          {name === null ? (
            <Button theme="outline" disabled={busy} onClick={() => setName("")}>
              {t("optiscaler_profile_save_current")}
            </Button>
          ) : (
            <>
              <TextField
                theme="dark"
                value={name}
                maxLength={60}
                placeholder={t("optiscaler_profile_name")}
                onChange={(e) => setName(e.target.value)}
              />
              <Button
                theme="primary"
                disabled={busy || !name.trim()}
                onClick={() => {
                  onSave(name.trim());
                  setName(null);
                }}
              >
                {t("optiscaler_profile_save")}
              </Button>
              <Button theme="outline" onClick={() => setName(null)}>
                {t("optiscaler_profile_cancel")}
              </Button>
            </>
          )}
        </div>
      )}
    </div>
  );
}
