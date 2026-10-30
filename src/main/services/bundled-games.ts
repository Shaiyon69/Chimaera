import type {
  GameShop,
  ShopAssets,
  ShopDetails,
  SteamAchievement,
} from "@types";
import {
  bloodborneAchievements,
  bloodborneShopAssets,
  bloodborneShopDetails,
  getBloodborneSetup,
  launchBloodborne,
  openBloodborneLauncher,
} from "./bloodborne/bloodborne";
import { downloadBloodbornePort } from "./bloodborne/bloodborne-port-installer";
import {
  downloadHorizon,
  getHorizonSetup,
  horizonShopAssets,
  horizonShopDetails,
  launchHorizon,
  openHorizonLauncher,
} from "./horizon/horizon";

interface BundledGameService {
  installed: () => boolean;
  download: () => Promise<void>;
  openLauncher: () => void;
  launch: (shop: GameShop, objectId: string) => Promise<null>;
  shopAssets: () => ShopAssets;
  shopDetails: () => ShopDetails;
  achievements?: () => SteamAchievement[];
}

/** Main-process side of BUNDLED_GAMES in @shared, by objectId. */
export const bundledGameServices: Record<string, BundledGameService> = {
  bloodborne: {
    installed: () => getBloodborneSetup().portInstalled,
    download: downloadBloodbornePort,
    openLauncher: openBloodborneLauncher,
    launch: launchBloodborne,
    shopAssets: bloodborneShopAssets,
    shopDetails: bloodborneShopDetails,
    achievements: bloodborneAchievements,
  },
  horizon: {
    installed: () => getHorizonSetup().installed,
    download: downloadHorizon,
    openLauncher: openHorizonLauncher,
    launch: launchHorizon,
    shopAssets: horizonShopAssets,
    shopDetails: horizonShopDetails,
  },
};

export const bundledGameService = (objectId: string) =>
  Object.hasOwn(bundledGameServices, objectId)
    ? bundledGameServices[objectId]
    : null;
