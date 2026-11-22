export type EpicIntegrationStatus =
  | { connected: false }
  | {
      connected: true;
      accountId: string;
      displayName: string;
      connectedAt: string;
      lastSyncedAt: string | null;
    };

export type EpicSyncPhase = "starting" | "library" | "merging" | "achievements";

export type EpicSyncState =
  | { status: "idle"; requiresReconnect?: boolean; reason?: string }
  | { status: "cancelling" }
  | {
      status: "running";
      phase: EpicSyncPhase;
      gamesFound: number;
      gamesProcessed: number;
    };

export type EpicSyncFinishedPayload =
  | { ok: true; status: EpicIntegrationStatus; origin: "manual" | "startup" }
  | { ok: false; message: string; origin: "manual" | "startup" };
