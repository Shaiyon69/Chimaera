import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { SelectField, TextField } from "@renderer/components";
import type { LibraryGame } from "@types";

// Virtual-key codes OptiScaler accepts for its menu key; names are not translated.
const KEY_NAMES: Record<string, string> = {
  "0x2D": "Insert",
  "0x24": "Home",
  "0x23": "End",
  "0x2E": "Delete",
  "0x21": "Page Up",
  "0x22": "Page Down",
  ...Object.fromEntries(
    Array.from({ length: 12 }, (_, i) => [
      `0x${(0x70 + i).toString(16).toUpperCase()}`,
      `F${i + 1}`,
    ])
  ),
  "-1": "None",
};

type Field = Awaited<
  ReturnType<typeof window.electron.getOptiscalerConfig>
>[number];

export function OptiscalerConfig({
  game,
  native,
}: Readonly<{ game: LibraryGame; native: boolean }>) {
  const { t } = useTranslation("game_details");
  const [fields, setFields] = useState<Field[]>([]);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setFields(
        await window.electron.getOptiscalerConfig(game.shop, game.objectId)
      );
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, [game.shop, game.objectId]);

  useEffect(() => {
    void load();
  }, [load]);

  const save = async (key: string, value: string) => {
    setError(null);
    try {
      await window.electron.setOptiscalerConfig(
        game.shop,
        game.objectId,
        key,
        value
      );
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
    void load();
  };

  if (!fields.length && !error) return null;

  const optionLabel = (key: string, option: string) => {
    if (option === "auto" || option === "true" || option === "false")
      return t(`optiscaler_opt_${option}`);
    if (key === "ShortcutKey") return KEY_NAMES[option] ?? option;
    if (key === "FpsOverlayType") return t(`optiscaler_fps_${option}`);
    return option;
  };

  const shown = fields.filter(
    // RDNA4 runs FSR4 natively, so the INT8 fallback is meaningless there.
    (f) => !(native && f.key === "Fsr4ForceEnableInt8")
  );
  const groups = [...new Set(shown.map((f) => f.group))];

  return (
    <div className="optiscaler__config">
      <h4>{t("optiscaler_config_title")}</h4>
      {groups.map((group) => (
        <div key={group} className="optiscaler__config-group">
          <h5>{t(`optiscaler_group_${group}`)}</h5>
          <div className="optiscaler__config-grid">
            {shown
              .filter((f) => f.group === group)
              .map((field) => {
                const label = t(`optiscaler_cfg_${field.key}`);
                if (field.type === "select")
                  return (
                    <SelectField
                      key={field.key}
                      label={label}
                      value={field.value}
                      options={field.options.map((o) => ({
                        key: o,
                        value: o,
                        label: optionLabel(field.key, o),
                      }))}
                      onChange={(e) => void save(field.key, e.target.value)}
                    />
                  );
                return (
                  <TextField
                    key={field.key}
                    label={label}
                    type="number"
                    theme="dark"
                    min={field.min}
                    max={field.max}
                    step={field.step}
                    defaultValue={field.value}
                    onBlur={(e) => {
                      if (e.target.value !== field.value)
                        void save(field.key, e.target.value);
                    }}
                  />
                );
              })}
          </div>
        </div>
      ))}
      {error && <p className="optiscaler__error">{error}</p>}
    </div>
  );
}
