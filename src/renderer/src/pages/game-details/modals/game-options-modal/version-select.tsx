import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { SelectField } from "@renderer/components";

type Release = Awaited<
  ReturnType<typeof window.electron.getGraphicsReleases>
>[number];

/** Release picker; "" means the newest release (and keeps auto-updates on). */
export function VersionSelect({
  tool,
  value,
  disabled,
  onChange,
}: Readonly<{
  tool: "optiscaler" | "dxvk" | "gplasync";
  value: string;
  disabled: boolean;
  onChange: (tag: string) => void;
}>) {
  const { t } = useTranslation("game_details");
  const [releases, setReleases] = useState<Release[]>([]);

  useEffect(() => {
    setReleases([]);
    // Offline: only "latest" is offered, which falls back to the newest cached release.
    void window.electron
      .getGraphicsReleases(tool)
      .then(setReleases)
      .catch(() => setReleases([]));
  }, [tool]);

  return (
    <SelectField
      label={t("graphics_version_label")}
      value={value}
      disabled={disabled}
      options={[
        { key: "latest", value: "", label: t("graphics_version_latest") },
        ...releases.map((r) => ({
          key: r.tag,
          value: r.tag,
          label: r.prerelease
            ? t("graphics_version_prerelease", { tag: r.tag })
            : r.tag,
        })),
      ]}
      onChange={(e) => onChange(e.target.value)}
    />
  );
}
