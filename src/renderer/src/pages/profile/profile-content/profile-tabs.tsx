import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";
import "./profile-content.scss";

export type ProfileTabType = "library" | "achievements" | "activity";

// Souvenirs tab hidden for now; its code stays for a later return.
const TABS: { id: ProfileTabType; labelKey: string }[] = [
  { id: "library", labelKey: "games" },
  { id: "achievements", labelKey: "achievements_tab" },
  { id: "activity", labelKey: "activity" },
];

interface ProfileTabsProps {
  activeTab: ProfileTabType;
  onTabChange: (tab: ProfileTabType) => void;
}

export function ProfileTabs({
  activeTab,
  onTabChange,
}: Readonly<ProfileTabsProps>) {
  const { t } = useTranslation("user_profile");

  return (
    <div className="profile-content__tabs" role="tablist">
      {TABS.map(({ id, labelKey }) => (
        <div key={id} className="profile-content__tab-wrapper">
          <button
            type="button"
            role="tab"
            aria-selected={activeTab === id}
            className={`profile-content__tab ${activeTab === id ? "profile-content__tab--active" : ""}`}
            onClick={() => onTabChange(id)}
          >
            {t(labelKey)}
          </button>
          {activeTab === id && (
            <motion.div
              className="profile-content__tab-underline"
              layoutId="tab-underline"
              transition={{
                type: "spring",
                stiffness: 300,
                damping: 30,
              }}
            />
          )}
        </div>
      ))}
    </div>
  );
}
