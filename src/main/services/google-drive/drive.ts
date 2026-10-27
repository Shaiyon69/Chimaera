import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import axios from "axios";
import type { GameShop } from "@types";
import {
  createLocalSaveBackup,
  listLocalSaveBackups,
  localSaveBackupPath,
  storeLocalSaveBackup,
  type LocalSaveBackup,
} from "../local-save-backups";
import { getBackupLabel } from "../ludusavi-bundle";
import { logger } from "../logger";
import {
  backupsToPrune,
  buildMultipartBody,
  escapeQueryValue,
  FOLDER_MIME,
  gameFolderName,
  ROOT_FOLDER,
} from "./drive-core";
import { getGoogleAccessToken, getGoogleDriveStatus } from "./google-oauth";

const API = "https://www.googleapis.com/drive/v3/files";
const UPLOAD_API = "https://www.googleapis.com/upload/drive/v3/files";

export interface DriveSaveBackup extends LocalSaveBackup {
  driveFileId: string;
}

interface DriveFile {
  id: string;
  name: string;
  description?: string;
  size?: string;
}

const authed = async () => ({
  Authorization: `Bearer ${await getGoogleAccessToken()}`,
});

const listFiles = async (q: string) => {
  const { data } = await axios.get<{ files: DriveFile[] }>(API, {
    headers: await authed(),
    params: {
      q: `${q} and trashed=false`,
      fields: "files(id,name,description,size)",
      pageSize: 200,
    },
  });
  return data.files;
};

const ensureFolder = async (name: string, parentId: string) => {
  const [existing] = await listFiles(
    `name='${escapeQueryValue(name)}' and mimeType='${FOLDER_MIME}' and '${parentId}' in parents`
  );
  if (existing) return existing.id;

  const { data } = await axios.post<{ id: string }>(
    API,
    { name, mimeType: FOLDER_MIME, parents: [parentId] },
    { headers: await authed(), params: { fields: "id" } }
  );
  return data.id;
};

const gameFolderId = async (shop: GameShop, objectId: string) =>
  ensureFolder(
    gameFolderName(shop, objectId),
    await ensureFolder(ROOT_FOLDER, "root")
  );

const toBackup = (f: DriveFile): DriveSaveBackup | null => {
  try {
    const meta = JSON.parse(f.description ?? "");
    return { ...meta, file: f.name, driveFileId: f.id };
  } catch {
    return null;
  }
};

export const listDriveSaveBackups = async (
  shop: GameShop,
  objectId: string
) => {
  const folder = await gameFolderId(shop, objectId);
  const files = await listFiles(
    `'${folder}' in parents and name contains '.tar'`
  );
  return files
    .flatMap((f) => toBackup(f) ?? [])
    .sort((a, b) => b.createdAt.localeCompare(a.createdAt));
};

/** Uploads a local backup; a backup already on Drive under the same name is left alone. */
export const uploadSaveBackupToDrive = async (
  shop: GameShop,
  objectId: string,
  file: string
) => {
  const local = (await listLocalSaveBackups(shop, objectId)).find(
    (b) => b.file === file
  );
  if (!local) throw new Error("save-backup/not-found");

  const remote = await listDriveSaveBackups(shop, objectId);
  if (remote.some((b) => b.file === file)) return;

  const folder = await gameFolderId(shop, objectId);
  const { file: _file, sizeBytes: _size, ...meta } = local;
  const boundary = crypto.randomUUID();
  const body = buildMultipartBody(
    { name: file, parents: [folder], description: JSON.stringify(meta) },
    await fs.promises.readFile(await localSaveBackupPath(shop, objectId, file)),
    boundary
  );

  await axios.post(UPLOAD_API, body, {
    headers: {
      ...(await authed()),
      "Content-Type": `multipart/related; boundary=${boundary}`,
    },
    params: { uploadType: "multipart", fields: "id" },
    maxBodyLength: Infinity,
  });
};

/** Downloads into the local backups folder, from where the normal restore applies it. */
export const downloadSaveBackupFromDrive = async (
  shop: GameShop,
  objectId: string,
  driveFileId: string
) => {
  const backup = (await listDriveSaveBackups(shop, objectId)).find(
    (b) => b.driveFileId === driveFileId
  );
  if (!backup) throw new Error("save-backup/not-found");

  const tmp = path.join(os.tmpdir(), `${crypto.randomUUID()}.tar`);
  try {
    const res = await axios.get(`${API}/${driveFileId}`, {
      headers: await authed(),
      params: { alt: "media" },
      responseType: "arraybuffer",
    });
    await fs.promises.writeFile(tmp, Buffer.from(res.data));

    const { file: _file, sizeBytes: _size, driveFileId: _id, ...meta } = backup;
    await storeLocalSaveBackup(shop, objectId, tmp, meta);
  } finally {
    await fs.promises.rm(tmp, { force: true });
  }
};

export const deleteDriveSaveBackup = async (driveFileId: string) => {
  await axios.delete(`${API}/${driveFileId}`, { headers: await authed() });
};

/** Runs after a game closes: snapshot, upload, then drop Drive copies past the limit. */
export const autoBackupToDrive = async (shop: GameShop, objectId: string) => {
  if (!getGoogleDriveStatus().connected) return;

  const backup = await createLocalSaveBackup(
    shop,
    objectId,
    getBackupLabel(true)
  );
  await uploadSaveBackupToDrive(shop, objectId, backup.file);

  const stale = backupsToPrune(
    (await listDriveSaveBackups(shop, objectId)).map((b) => ({
      id: b.driveFileId,
      createdAt: b.createdAt,
    }))
  );
  await Promise.all(
    stale.map((id) =>
      deleteDriveSaveBackup(id).catch((err) =>
        logger.error("Failed to prune Drive save backup", err)
      )
    )
  );
};
