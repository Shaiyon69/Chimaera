import { useContext } from "react";
import { useTranslation } from "react-i18next";

import {
  useAppSelector,
  useDate,
  useDownload,
  useProtonDB,
} from "@renderer/hooks";
import { Link } from "@renderer/components/link/link";
import {
  ProtonDBBadge,
  toProtonTier,
} from "@renderer/components/protondb-badge/protondb-badge";

import { HeroPanelActions } from "./hero-panel-actions";
import { HeroPanelPlaytime } from "./hero-panel-playtime";

import { gameDetailsContext } from "@renderer/context";
import "./hero-panel.scss";

export function HeroPanel() {
  const { t } = useTranslation("game_details");

  const { formatDate } = useDate();

  const { game, repacks, shop, objectId } = useContext(gameDetailsContext);
  const protonTier = toProtonTier(
    useProtonDB(shop, objectId ?? undefined).data?.tier
  );

  const { lastPacket } = useDownload();

  const extraction = useAppSelector((state) => state.download.extraction);

  const isGameDownloading =
    game?.download?.status === "active" && lastPacket?.gameId === game?.id;

  const isExtracting = extraction?.visibleId === game?.id;

  const getInfo = () => {
    if (!game) {
      const [latestRepack] = repacks;

      if (latestRepack) {
        const lastUpdate = latestRepack.uploadDate
          ? formatDate(latestRepack.uploadDate!)
          : "";
        const repacksCount = repacks.length;

        return (
          <>
            <p>{t("updated_at", { updated_at: lastUpdate })}</p>
            <p>{t("download_options", { count: repacksCount })}</p>
          </>
        );
      }

      return <p>{t("no_downloads")}</p>;
    }

    return <HeroPanelPlaytime />;
  };

  const showProgressBar =
    (game?.download?.status === "active" && game?.download?.progress < 1) ||
    game?.download?.status === "paused";

  const showExtractionProgressBar = isExtracting;

  return (
    <div className="hero-panel__container">
      <div className="hero-panel">
        <div className="hero-panel__content">{getInfo()}</div>
        <div className="hero-panel__actions">
          {protonTier && (
            <Link
              to={`https://www.protondb.com/app/${objectId}`}
              className="hero-panel__protondb"
              aria-label={t("view_on_protondb")}
            >
              <ProtonDBBadge badge={protonTier} />
            </Link>
          )}
          <HeroPanelActions />
        </div>

        {showProgressBar && (
          <progress
            max={1}
            value={
              isGameDownloading
                ? lastPacket?.progress
                : game?.download?.progress
            }
            className={`hero-panel__progress-bar ${
              game?.download?.status === "paused"
                ? "hero-panel__progress-bar--disabled"
                : ""
            }`}
          />
        )}

        {showExtractionProgressBar && (
          <progress
            max={1}
            value={extraction?.progress ?? 0}
            className="hero-panel__progress-bar hero-panel__progress-bar--extraction"
          />
        )}
      </div>
    </div>
  );
}
