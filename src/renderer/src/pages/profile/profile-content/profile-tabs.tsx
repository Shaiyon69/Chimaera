import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";
import "./profile-content.scss";

export type ProfileTabType = "library" | "souvenirs";

interface ProfileTabsProps {
  activeTab: ProfileTabType;
  souvenirsCount: number;
  onTabChange: (tab: ProfileTabType) => void;
}

export function ProfileTabs({
  activeTab,
  souvenirsCount,
  onTabChange,
}: Readonly<ProfileTabsProps>) {
  const { t } = useTranslation("user_profile");

  return (
    <div className="profile-content__tabs">
      <div className="profile-content__tab-wrapper">
        <button
          type="button"
          className={`profile-content__tab ${activeTab === "library" ? "profile-content__tab--active" : ""}`}
          onClick={() => onTabChange("library")}
        >
          {t("library")}
        </button>
        {activeTab === "library" && (
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
      <div className="profile-content__tab-wrapper">
        <button
          type="button"
          className={`profile-content__tab ${activeTab === "souvenirs" ? "profile-content__tab--active" : ""}`}
          onClick={() => onTabChange("souvenirs")}
        >
          {t("souvenirs")}
          {souvenirsCount > 0 && (
            <span className="profile-content__tab-badge">{souvenirsCount}</span>
          )}
        </button>
        {activeTab === "souvenirs" && (
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
    </div>
  );
}
