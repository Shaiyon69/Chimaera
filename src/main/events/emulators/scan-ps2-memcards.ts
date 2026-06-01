import { randomUUID } from "node:crypto";
import { promises as fs } from "node:fs";
import path from "node:path";

import { registerEvent } from "../register-event";
import { buildLocalLaunchboxAssetIndex } from "./memcard-local-assets";
import { WindowManager, emulators, logger } from "@main/services";
import type {
  LaunchboxShopDetailsAssetsResponse,
  Ps2Save,
} from "@main/services/emulators";
import { levelKeys, ps2MemoryCardSavesSublevel } from "@main/level";
import type {
  Ps2MemcardScanInput,
  Ps2MemcardScanProgress,
  Ps2MemoryCardSaveRecord,
} from "@types";

const MEMCARD_FILE_RE = /\.(ps2|mcd|mc2)$/i;

const inflight = new Map<string, { cancelled: boolean }>();

const fileExists = async (filePath: string): Promise<boolean> => {
  try {
    await fs.stat(filePath);
    return true;
  } catch {
    return false;
  }
};

// A manual selection may be a `.ps2` file or a folder of cards.
const expandManualPath = async (target: string): Promise<string[]> => {
  try {
    const stat = await fs.stat(target);
    if (stat.isDirectory()) {
      const entries = await fs.readdir(target);
      return entries
        .filter((name) => MEMCARD_FILE_RE.test(name))
        .map((name) => path.join(target, name));
    }
    return [target];
  } catch {
    return [];
  }
};

const runScan = async (
  input: Ps2MemcardScanInput,
  signal: { cancelled: boolean },
  emit: (payload: Ps2MemcardScanProgress) => void
): Promise<void> => {
  const config = await emulators.getEmulatorConfig("ps2");

  const cardFiles = new Set<string>();
  if (input.autoDetect) {
    for (const file of await emulators.resolvePs2MemcardFiles(
      config.executablePath
    )) {
      cardFiles.add(file);
    }
  }
  for (const manual of input.manualPaths ?? []) {
    for (const file of await expandManualPath(manual)) cardFiles.add(file);
  }
  const files = Array.from(cardFiles);

  // Phase A — parse each card and collect its save folders.
  const detected: Array<{
    cardFilePath: string;
    cardLabel: string;
    save: Ps2Save;
  }> = [];
  let processed = 0;
  for (const file of files) {
    if (signal.cancelled) {
      emit({
        type: "cancelled",
        cardCount: files.length,
        saveCount: detected.length,
      });
      return;
    }
    emit({
      type: "scan_progress",
      processed,
      total: files.length,
      currentCard: path.basename(file),
    });
    const info = await emulators.listSaves(file);
    if (info) {
      for (const save of info.saves) {
        detected.push({
          cardFilePath: file,
          cardLabel: path.basename(file),
          save,
        });
      }
    }
    processed += 1;
  }
  emit({
    type: "scan_progress",
    processed,
    total: files.length,
    currentCard: null,
  });

  // Phase B — resolve unique SKUs to LaunchBox metadata, then persist records.
  const skus = Array.from(
    new Set(
      detected
        .map((d) => d.save.sku)
        .filter((sku): sku is string => Boolean(sku))
    )
  );
  const lookup = await emulators.fetchShopDetailsForSkus(skus);
  const localIndex = await buildLocalLaunchboxAssetIndex();

  let matched = 0;
  let unmatched = 0;
  let index = 0;
  for (const { cardFilePath, cardLabel, save } of detected) {
    if (signal.cancelled) {
      emit({
        type: "cancelled",
        cardCount: files.length,
        saveCount: detected.length,
      });
      return;
    }
    // Remote shop-details first; fall back to the local library when it misses.
    let assets: LaunchboxShopDetailsAssetsResponse | null = null;
    if (save.sku) {
      const norm = emulators.normalizeSku(save.sku);
      const entry = lookup.get(norm);
      assets = entry
        ? emulators.mapEntryToAssets(entry)
        : (localIndex.get(norm) ?? null);
    }
    if (assets) matched += 1;
    else unmatched += 1;

    const record: Ps2MemoryCardSaveRecord = {
      cardFilePath,
      cardLabel,
      folderName: save.folderName,
      sku: save.sku ?? null,
      objectId: assets?.objectId ?? null,
      shop: assets ? "launchbox" : null,
      title: assets?.title || null,
      iconUrl: assets?.iconUrl ?? null,
      libraryImageUrl: assets?.libraryImageUrl ?? null,
      libraryHeroImageUrl: assets?.libraryHeroImageUrl ?? null,
      logoImageUrl: assets?.logoImageUrl ?? null,
      fileCount: save.fileCount,
      sizeBytes: save.sizeBytes,
      createdAt: save.createdSecs * 1000,
      modifiedAt: save.modifiedSecs * 1000,
      detectedAt: Date.now(),
    };
    await ps2MemoryCardSavesSublevel.put(
      levelKeys.ps2MemoryCardSave(cardFilePath, save.folderName),
      record
    );

    index += 1;
    emit({
      type: "match_progress",
      processed: index,
      total: detected.length,
      currentSave: save.folderName,
      status: assets ? "matched" : "unmatched",
      matched,
      unmatched,
    });
  }

  // Prune stale records so removed/renamed cards and deleted save folders drop
  // off the list automatically: a save folder that vanished from a card we just
  // scanned, or any record whose card file no longer exists on disk.
  const detectedKeys = new Set(
    detected.map((d) =>
      levelKeys.ps2MemoryCardSave(d.cardFilePath, d.save.folderName)
    )
  );
  const staleKeys: string[] = [];
  for (const [key, rec] of await ps2MemoryCardSavesSublevel.iterator().all()) {
    if (cardFiles.has(rec.cardFilePath)) {
      if (!detectedKeys.has(key)) staleKeys.push(key);
    } else if (!(await fileExists(rec.cardFilePath))) {
      staleKeys.push(key);
    }
  }
  for (const key of staleKeys) {
    await ps2MemoryCardSavesSublevel.del(key);
  }

  emit({
    type: "done",
    cardCount: files.length,
    saveCount: detected.length,
    matched,
    unmatched,
  });
};

const scanPs2Memcards = async (
  _event: Electron.IpcMainInvokeEvent,
  input: Ps2MemcardScanInput
) => {
  const requestId = randomUUID();
  const signal = { cancelled: false };
  inflight.set(requestId, signal);
  const channel = `on-ps2-memcard-scan-progress-${requestId}`;
  const emit = (payload: Ps2MemcardScanProgress) => {
    WindowManager.mainWindow?.webContents.send(channel, payload);
  };

  void (async () => {
    try {
      await runScan(input, signal, emit);
    } catch (err) {
      logger.error("PS2 memory card scan failed", err);
      emit({
        type: "error",
        message: err instanceof Error ? err.message : String(err),
      });
    } finally {
      inflight.delete(requestId);
    }
  })();

  return { requestId };
};

const cancelPs2MemcardScan = async (
  _event: Electron.IpcMainInvokeEvent,
  requestId: string
) => {
  const signal = inflight.get(requestId);
  if (signal) signal.cancelled = true;
};

registerEvent("scanPs2Memcards", scanPs2Memcards);
registerEvent("cancelPs2MemcardScan", cancelPs2MemcardScan);
