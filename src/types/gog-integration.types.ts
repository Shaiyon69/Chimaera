export type GogIntegrationStatus =
  | { connected: false }
  | {
      connected: true;
      userId: string;
      username: string;
      avatarUrl: string | null;
      connectedAt: string;
      lastSyncedAt: string | null;
    };

export type GogSyncPhase =
  | "starting"
  | "library"
  | "artwork"
  | "merging"
  | "achievements";

export type GogSyncState =
  | { status: "idle"; requiresReconnect?: boolean }
  | { status: "cancelling" }
  | {
      status: "running";
      phase: GogSyncPhase;
      gamesFound: number;
      gamesProcessed: number;
    };

export type GogSyncFinishedPayload =
  | { ok: true; status: GogIntegrationStatus; origin: "manual" | "startup" }
  | { ok: false; message: string; origin: "manual" | "startup" };
