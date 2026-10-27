import { getSteamAccount } from "@main/services/steam-integration/steam-account";
import { toSteamIntegrationStatus } from "@main/services/steam-integration/steam-account-core";
import { registerEvent } from "../register-event";

const getSteamIntegrationStatus = async () =>
  toSteamIntegrationStatus(await getSteamAccount());

registerEvent("getSteamIntegrationStatus", getSteamIntegrationStatus);
