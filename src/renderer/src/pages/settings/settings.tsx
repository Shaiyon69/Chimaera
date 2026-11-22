import { useTranslation } from "react-i18next";
import {
  SettingsContextConsumer,
  SettingsContextProvider,
} from "@renderer/context";
import { SettingsProfileData } from "./settings-profile-data";
import { useUserDetails } from "@renderer/hooks";
import { Fragment, useMemo } from "react";
import "./settings.scss";
import type { CSSProperties, ReactNode } from "react";
import {
  Bell,
  Download,
  Gamepad2,
  Layers,
  Link2,
  Palette,
  Play,
  Plug,
  ShieldCheck,
  SlidersHorizontal,
  Tv,
} from "lucide-react";
import { SettingsContextGeneral } from "./settings-context-general";
import { SettingsAppearance } from "./appearance";
import { SettingsContextDownloads } from "./settings-context-downloads";
import { SettingsContextDownloadSources } from "./settings-context-download-sources";
import { SettingsContextNotifications } from "./settings-context-notifications";
import { SettingsContextContentGameplay } from "./settings-context-content-gameplay";
import { SettingsContextIntegrations } from "./settings-context-integrations";
import { SettingsContextCompatibility } from "./settings-context-compatibility";
import { SettingsContextBigPicture } from "./settings-context-big-picture";
import { SettingsContextEmulation } from "./emulation/settings-context-emulation";

// Each section's icon has its own colour (palette blues, gold, teal, slate, soft pink, error
// red; no green, orange or purple), so the list scans by colour as well as by name.
const tile = (
  Icon: (props: { size?: number; strokeWidth?: number }) => ReactNode,
  tint: string
) => (
  <span
    className="settings__sidebar-glyph"
    style={{ "--tint": tint } as CSSProperties}
  >
    <Icon size={17} strokeWidth={2} />
  </span>
);

export default function Settings() {
  const { t } = useTranslation("settings");

  const { userDetails } = useUserDetails();

  const categories = useMemo(
    () => [
      {
        id: "general" as const,
        label: t("general"),
        icon: tile(SlidersHorizontal, "#9fb3c8"),
        group: "general" as const,
      },
      {
        id: "appearance" as const,
        label: t("appearance"),
        icon: tile(Palette, "#e8c77d"),
        group: "general" as const,
      },
      {
        id: "notifications" as const,
        label: t("notifications"),
        icon: tile(Bell, "#e5676f"),
        group: "general" as const,
      },
      {
        id: "account_privacy" as const,
        label: t("profile_data_title"),
        icon: tile(ShieldCheck, "#7fa7e0"),
        group: "general" as const,
      },
      {
        id: "big_picture" as const,
        label: t("big_picture"),
        icon: tile(Tv, "#adc6ff"),
        group: "general" as const,
      },
      {
        id: "downloads" as const,
        label: t("downloads"),
        icon: tile(Download, "#4fd1b0"),
        group: "library" as const,
      },
      {
        id: "download_sources" as const,
        label: t("download_sources"),
        icon: tile(Link2, "#9fb3c8"),
        group: "library" as const,
      },
      {
        id: "content_gameplay" as const,
        label: t("content_gameplay"),
        icon: tile(Play, "#e8c77d"),
        group: "library" as const,
      },
      {
        id: "integrations" as const,
        label: t("integrations"),
        icon: tile(Plug, "#adc6ff"),
        group: "library" as const,
      },
      {
        id: "compatibility" as const,
        label: t("compatibility"),
        icon: tile(Layers, "#e0e0e0"),
        group: "games" as const,
      },
      {
        id: "emulation" as const,
        label: t("emulation"),
        icon: tile(Gamepad2, "#f2a7c3"),
        group: "classics" as const,
      },
    ],
    [t, userDetails]
  );

  return (
    <SettingsContextProvider>
      <SettingsContextConsumer>
        {({ currentCategoryId, setCurrentCategoryId, appearance }) => {
          const currentCategory =
            categories.find((category) => category.id === currentCategoryId) ??
            categories[0];
          const selectedCategoryId = currentCategory.id;

          const renderCategory = () => {
            if (selectedCategoryId === "general") {
              return <SettingsContextGeneral />;
            }

            if (selectedCategoryId === "appearance") {
              return <SettingsAppearance appearance={appearance} />;
            }

            if (selectedCategoryId === "downloads") {
              return <SettingsContextDownloads />;
            }

            if (selectedCategoryId === "download_sources") {
              return <SettingsContextDownloadSources />;
            }

            if (selectedCategoryId === "notifications") {
              return <SettingsContextNotifications />;
            }

            if (selectedCategoryId === "content_gameplay") {
              return <SettingsContextContentGameplay />;
            }

            if (selectedCategoryId === "integrations") {
              return <SettingsContextIntegrations />;
            }

            if (selectedCategoryId === "compatibility") {
              return <SettingsContextCompatibility />;
            }

            if (selectedCategoryId === "big_picture") {
              return <SettingsContextBigPicture />;
            }

            if (selectedCategoryId === "emulation") {
              return <SettingsContextEmulation />;
            }

            return <SettingsProfileData />;
          };

          return (
            <section className="settings__container">
              <div className="settings__content">
                <aside className="settings__sidebar">
                  {categories.map((category, index) => {
                    const prevGroup =
                      index > 0 ? categories[index - 1].group : undefined;
                    const currentGroup = category.group;
                    const showGroupHeader = currentGroup !== prevGroup;

                    return (
                      <Fragment key={category.id}>
                        {showGroupHeader && (
                          <div className="settings__sidebar-group">
                            <span className="settings__sidebar-group-label">
                              {t(`${currentGroup}_group`)}
                            </span>
                            <div className="settings__sidebar-divider" />
                          </div>
                        )}
                        <button
                          type="button"
                          className={`settings__sidebar-button ${
                            currentCategory.id === category.id
                              ? "settings__sidebar-button--active"
                              : ""
                          }`}
                          onClick={() => setCurrentCategoryId(category.id)}
                        >
                          <span className="settings__sidebar-button-icon">
                            {category.icon}
                          </span>
                          <span className="settings__sidebar-button-label">
                            {category.label}
                          </span>
                        </button>
                      </Fragment>
                    );
                  })}
                </aside>

                <div className="settings__panel">
                  {selectedCategoryId !== "emulation" && (
                    <h2>{currentCategory.label}</h2>
                  )}
                  {renderCategory()}
                </div>
              </div>
            </section>
          );
        }}
      </SettingsContextConsumer>
    </SettingsContextProvider>
  );
}
