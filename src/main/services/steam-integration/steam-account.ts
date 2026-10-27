import { db, levelKeys } from "@main/level";

import type { SteamAccount } from "./steam-account-core";

export const getSteamAccount = async (): Promise<SteamAccount | null> =>
  (await db.get<string, SteamAccount>(levelKeys.steamAccount, {
    valueEncoding: "json",
  })) ?? null;

export const saveSteamAccount = (account: SteamAccount) =>
  db.put(levelKeys.steamAccount, account, { valueEncoding: "json" });

export const clearSteamAccount = () => db.del(levelKeys.steamAccount);
