import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@renderer/components";

type Entry = Awaited<
  ReturnType<typeof window.electron.getGraphicsCache>
>[number];

const formatSize = (bytes: number) =>
  bytes >= 1 << 20
    ? `${(bytes / (1 << 20)).toFixed(1)} MB`
    : `${Math.max(1, Math.round(bytes / 1024))} KB`;

export function SettingsGraphicsCache() {
  const { t } = useTranslation("settings");
  const [entries, setEntries] = useState<Entry[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setEntries(await window.electron.getGraphicsCache());
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const remove = async (id: string) => {
    setError(null);
    try {
      await window.electron.deleteGraphicsCache(id);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
    void load();
  };

  return (
    <section className="settings-optiscaler__card">
      <div className="settings-optiscaler__card-header">
        <h3 className="settings-optiscaler__card-title">
          {t("graphics_cache_title")}
        </h3>
        <p className="settings-optiscaler__hint">{t("graphics_cache_hint")}</p>
      </div>
      {entries?.length === 0 && (
        <p className="settings-optiscaler__hint">{t("graphics_cache_empty")}</p>
      )}
      {entries?.map((e) => (
        <div key={e.id} className="settings-optiscaler__cache-row">
          <span>
            {t(`graphics_cache_tool_${e.tool}`)} {e.version}
          </span>
          <span className="settings-optiscaler__hint">
            {formatSize(e.size)}
          </span>
          <Button theme="outline" onClick={() => void remove(e.id)}>
            {t("graphics_cache_delete")}
          </Button>
        </div>
      ))}
      {error && <p className="settings-optiscaler__warning">{error}</p>}
    </section>
  );
}
