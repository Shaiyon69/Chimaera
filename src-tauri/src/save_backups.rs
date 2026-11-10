//! Save backups (port of `local-save-backups.ts`, `ludusavi.ts`, `ludusavi-bundle.ts`):
//! Ludusavi snapshots of a game's saves, kept as tar files plus a JSON sidecar in a
//! folder the user picks. The layout matches Electron, so its backups list and restore here.

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tauri::{AppHandle, Manager};

use crate::electron_import::electron_user_data;
use crate::library::iso_now;
use crate::store::Store;
use crate::data_location::DataDir;

const DIR_KEY: &str = "localSaveBackupsDir";
const PUBLIC_PROFILE: &str = "C:/Users/Public";

fn app_data(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().chimaera_data_dir().map_err(|e| e.to_string())
}

fn electron_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(electron_user_data(&app.path().config_dir().map_err(|e| e.to_string())?))
}

/// The Tauri `name` folder, unless only the Electron build's one exists; that one keeps its files in view.
fn default_dir(app_data: &Path, electron: &Path, name: &str) -> PathBuf {
    let own = app_data.join(name);
    let old = electron.join(name);
    if !own.exists() && old.exists() { old } else { own }
}

/// `default_dir` for this app's data folders.
pub fn data_dir(app: &AppHandle, name: &str) -> Result<PathBuf, String> {
    Ok(default_dir(&app_data(app)?, &electron_dir(app)?, name))
}

/// `getSaveBackupsDir`.
pub fn backups_dir(app: &AppHandle) -> Result<PathBuf, String> {
    match app.state::<Store>().get("", DIR_KEY)?.as_str().filter(|d| !d.is_empty()) {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => data_dir(app, "save-backups"),
    }
}

pub fn set_backups_dir(store: &Store, dir: &str) -> Result<(), String> {
    store.put("", DIR_KEY, &json!(dir))
}

/// `${shop}-${objectId}` with each run of characters Windows forbids replaced by `_`.
pub fn game_folder_name(shop: &str, object_id: &str) -> String {
    let (mut out, mut in_run) = (String::new(), false);
    for c in format!("{shop}-{object_id}").chars() {
        let forbidden = "<>:\"/\\|?*".contains(c);
        if !forbidden {
            out.push(c);
        } else if !in_run {
            out.push('_');
        }
        in_run = forbidden;
    }
    out
}

pub fn game_dir(app: &AppHandle, shop: &str, object_id: &str) -> Result<PathBuf, String> {
    Ok(backups_dir(app)?.join(game_folder_name(shop, object_id)))
}

/// Newest first; a tar without a readable sidecar is skipped.
pub fn list_in(dir: &Path) -> Vec<Value> {
    let mut backups: Vec<Value> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let file = entry.file_name().to_string_lossy().into_owned();
            if !file.ends_with(".tar") {
                return None;
            }
            let mut meta: Value = serde_json::from_str(&fs::read_to_string(entry.path().with_extension("json")).ok()?).ok()?;
            meta.as_object_mut()?.insert("file".into(), json!(file));
            Some(meta)
        })
        .collect();
    backups.sort_by(|a, b| b["createdAt"].as_str().unwrap_or_default().cmp(a["createdAt"].as_str().unwrap_or_default()));
    backups
}

/// A listed backup's path; refuses names that would leave the game's folder.
pub fn backup_path(dir: &Path, file: &str) -> Result<PathBuf, String> {
    let plain = Path::new(file).file_name().is_some_and(|name| name == file);
    if !plain || file.contains(['/', '\\']) || !file.ends_with(".tar") {
        return Err("save-backup/invalid-file".into());
    }
    Ok(dir.join(file))
}

/// Copies `source` in as `<createdAt>.tar` and writes its sidecar.
pub fn store_in(dir: &Path, source: &Path, mut meta: Value) -> Result<Value, String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let file = format!("{}.tar", meta["createdAt"].as_str().unwrap_or_default().replace([':', '.'], "-"));
    let target = dir.join(&file);
    fs::copy(source, &target).map_err(|e| e.to_string())?;
    meta["file"] = json!(file);
    meta["sizeBytes"] = json!(fs::metadata(&target).map_err(|e| e.to_string())?.len());
    let text = serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?;
    fs::write(target.with_extension("json"), text).map_err(|e| e.to_string())?;
    Ok(meta)
}

pub fn delete_in(dir: &Path, file: &str) -> Result<(), String> {
    let target = backup_path(dir, file)?;
    for path in [target.with_extension("json"), target] {
        if let Err(e) = fs::remove_file(&path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(e.to_string());
            }
        }
    }
    Ok(())
}

/// Wine prefix the saves live in. Windows has none.
// shortcut: Linux (Wine prefix user profile from user.reg) is refused; port it with the Linux launch path.
fn wine_prefix() -> Result<Option<String>, String> {
    if cfg!(windows) { Ok(None) } else { Err("save-backup/linux-not-ported".into()) }
}

fn home_dir(app: &AppHandle) -> Result<String, String> {
    Ok(normalize(&app.path().home_dir().map_err(|e| e.to_string())?.to_string_lossy()))
}

fn normalize(path: &str) -> String {
    path.replace('\\', "/")
}

/// The Ludusavi binary bundled with the app, and its config folder (seeded on first use).
fn ludusavi(app: &AppHandle) -> Result<(PathBuf, PathBuf), String> {
    let resources = app.path().resource_dir().map_err(|e| e.to_string())?.join("ludusavi");
    let binary = resources.join(if cfg!(windows) { "ludusavi.exe" } else { "ludusavi" });
    let config = app_data(app)?.join("ludusavi");
    let config_file = config.join("config.yaml");
    if !config_file.exists() {
        fs::create_dir_all(&config).map_err(|e| e.to_string())?;
        // Electron's copy carries the user's custom save paths.
        let electron = electron_dir(app)?.join("ludusavi").join("config.yaml");
        let seed = if electron.exists() { electron } else { resources.join("config.yaml") };
        fs::copy(seed, &config_file).map_err(|e| e.to_string())?;
    }
    Ok((binary, config))
}

/// `ludusavi backup <objectId> --api --force`; its JSON report.
fn run_ludusavi(binary: &Path, config: &Path, object_id: &str, backup_path: Option<&Path>, wine_prefix: Option<&str>, preview: bool) -> Result<Value, String> {
    let mut command = Command::new(binary);
    command.arg("--config").arg(config).args(["backup", object_id, "--api", "--force"]);
    if preview {
        command.arg("--preview");
    }
    if let Some(path) = backup_path {
        command.arg("--path").arg(path);
    }
    if let Some(prefix) = wine_prefix {
        command.args(["--wine-prefix", prefix]);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let output = command.output().map_err(|e| format!("ludusavi: {e}"))?;
    if !output.status.success() {
        return Err(format!("ludusavi exited with {}: {}", output.status, String::from_utf8_lossy(&output.stderr).trim()));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("ludusavi output: {e}"))
}

/// `getGameSaveFolder`: folder of the first save file Ludusavi finds, or null.
pub async fn save_folder(app: &AppHandle, object_id: &str) -> Value {
    let found = async {
        let (binary, config) = ludusavi(app)?;
        let id = object_id.to_string();
        let report = tauri::async_runtime::spawn_blocking(move || run_ludusavi(&binary, &config, &id, None, None, true)).await.map_err(|e| e.to_string())??;
        Ok::<_, String>(first_save_folder(&report, object_id))
    };
    match found.await {
        Ok(folder) => folder.map_or(Value::Null, Value::String),
        Err(e) => {
            eprintln!("[getGameSaveFolder] {e}");
            Value::Null
        }
    }
}

fn first_save_folder(report: &Value, object_id: &str) -> Option<String> {
    let file = report["games"][object_id]["files"].as_object()?.keys().next()?;
    Some(file.rsplit_once(['/', '\\']).map_or("", |(dir, _)| dir).to_string())
}

/// "Backup from <date>" in the user's language, dated like `formatDate` (MM-dd-yyyy for en, else dd/MM/yyyy).
// shortcut: the UTC date, not local time.
fn backup_label(language: &str, created_at: &str, automatic: bool) -> String {
    let (y, m, d) = (&created_at[0..4], &created_at[5..7], &created_at[8..10]);
    let date = if language == "en" { format!("{m}-{d}-{y}") } else { format!("{d}/{m}/{y}") };
    let key = if automatic { "automatic_backup_from" } else { "backup_from" };
    crate::i18n::tr(language, "game_details", key, &[("date", &date)])
}

/// `createLocalSaveBackup`: Ludusavi backup, packed into a tar, stored with its metadata.
pub async fn create(app: &AppHandle, shop: &str, object_id: &str, label: Option<&str>, automatic: bool) -> Result<Value, String> {
    let prefix = wine_prefix()?;
    let (binary, config) = ludusavi(app)?;
    let temp = app_data(app)?.join("Backups");
    let backup_root = temp.join(format!("{shop}-{object_id}"));
    let tar_path = temp.join(format!("{}.tar", uuid::Uuid::new_v4()));
    let id = object_id.to_string();
    let (root, tar_out, wine) = (backup_root.clone(), tar_path.clone(), prefix.clone());
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let _ = fs::remove_dir_all(&root);
        run_ludusavi(&binary, &config, &id, Some(&root), wine.as_deref(), false)?;
        pack(&root, &tar_out)
    })
    .await
    .map_err(|e| e.to_string())??;

    let created_at = iso_now();
    let language = app.state::<Store>().get("", "language")?.as_str().unwrap_or("en").to_string();
    let meta = json!({
        "label": label.filter(|l| !l.is_empty()).map_or_else(|| backup_label(&language, &created_at, automatic), str::to_string),
        "createdAt": created_at,
        "homeDir": home_dir(app)?,
        "winePrefixPath": prefix,
        "hostname": sysinfo::System::host_name().unwrap_or_default(),
    });
    let stored = store_in(&game_dir(app, shop, object_id)?, &tar_path, meta);
    let _ = fs::remove_file(&tar_path);
    stored
}

/// Tars the folder's contents under `./`, like node-tar with `cwd` and `["."]`.
fn pack(dir: &Path, tar_path: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut builder = tar::Builder::new(fs::File::create(tar_path).map_err(|e| e.to_string())?);
    builder.append_dir_all(".", dir).map_err(|e| e.to_string())?;
    builder.into_inner().map(|_| ()).map_err(|e| e.to_string())
}

/// `restoreLocalSaveBackup`: unpack the tar and move each save back where the game reads it.
pub async fn restore(app: &AppHandle, shop: &str, object_id: &str, file: &str) -> Result<(), String> {
    let prefix = wine_prefix()?;
    let dir = game_dir(app, shop, object_id)?;
    let tar_path = backup_path(&dir, file)?;
    let backup = list_in(&dir).into_iter().find(|b| b["file"] == file).ok_or("save-backup/not-found")?;
    let extract_dir = app_data(app)?.join("Backups").join(format!("{shop}-{object_id}"));
    let user_profile = home_dir(app)?;
    let id = object_id.to_string();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let _ = fs::remove_dir_all(&extract_dir);
        fs::create_dir_all(&extract_dir).map_err(|e| e.to_string())?;
        // tar's unpack skips entries that would land outside extract_dir.
        tar::Archive::new(fs::File::open(&tar_path).map_err(|e| e.to_string())?).unpack(&extract_dir).map_err(|e| e.to_string())?;
        restore_mapping(
            &extract_dir.join(&id),
            &normalize(backup["homeDir"].as_str().unwrap_or_default()),
            &user_profile,
            prefix.as_deref(),
            backup["winePrefixPath"].as_str(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

fn add_prefix(windows_path: &str, prefix: Option<&str>) -> String {
    match prefix {
        Some(p) => Path::new(p).join(windows_path.replacen("C:", "drive_c", 1)).to_string_lossy().into_owned(),
        None => windows_path.to_string(),
    }
}

/// Port of `restoreLudusaviBackup`: Ludusavi's `mapping.yaml` lists each saved file by its
/// original path; the backup's profile folder is swapped for this machine's.
fn restore_mapping(game_backup: &Path, home_dir: &str, user_profile: &str, prefix: Option<&str>, artifact_prefix: Option<&str>) -> Result<(), String> {
    let text = fs::read_to_string(game_backup.join("mapping.yaml")).map_err(|e| format!("mapping.yaml: {e}"))?;
    let mapping: Value = serde_yaml::from_str(&text).map_err(|e| format!("mapping.yaml: {e}"))?;
    let drives = mapping["drives"].as_object().cloned().unwrap_or_default();
    let artifact_root = artifact_prefix.map(|p| if p.ends_with('/') { p.to_string() } else { format!("{p}/") });

    for backup in mapping["backups"].as_array().into_iter().flatten() {
        for key in backup["files"].as_object().into_iter().flatten().map(|(k, _)| k) {
            // "C:/Users/..." is stored under "drive-C/Users/...".
            let stored = drives.iter().fold(key.clone(), |p, (drive_key, drive)| p.replacen(drive.as_str().unwrap_or_default(), drive_key, 1));
            let source = game_backup.join(stored);
            let mut destination = key.clone();
            if let Some(root) = &artifact_root {
                destination = destination.replacen(root.as_str(), "", 1);
            }
            let destination = destination
                .replacen("drive_c", "C:", 1)
                .replacen(home_dir, &add_prefix(user_profile, prefix), 1)
                .replacen(PUBLIC_PROFILE, &add_prefix(PUBLIC_PROFILE, prefix), 1);
            let destination = Path::new(&destination);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            if destination.exists() {
                fs::remove_file(destination).map_err(|e| e.to_string())?;
            }
            // rename fails across drives; the save may live on another one than the app data.
            if fs::rename(&source, destination).is_err() {
                fs::copy(&source, destination).map_err(|e| format!("restore {}: {e}", source.display()))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chimaera-save-backups-{name}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn folder_names_match_electron() {
        assert_eq!(game_folder_name("steam", "620"), "steam-620");
        assert_eq!(game_folder_name("custom", "a<>b:c/d"), "custom-a_b_c_d");
        assert_eq!(game_folder_name("custom", "a_<b"), "custom-a__b");
    }

    #[test]
    fn backup_names_cannot_leave_the_folder() {
        let dir = Path::new("C:/backups/steam-620");
        assert!(backup_path(dir, "2026-10-31T10-00-00-000Z.tar").is_ok());
        for bad in ["../x.tar", "a/x.tar", "a\\x.tar", "x.json", ".."] {
            assert!(backup_path(dir, bad).is_err(), "{bad}");
        }
        // A drive prefix only exists on Windows; elsewhere `C:x.tar` is a plain name.
        assert_eq!(backup_path(dir, "C:x.tar").is_err(), cfg!(windows));
    }

    #[test]
    fn store_list_delete_round_trip() {
        let dir = temp_dir("store");
        let source = dir.join("source.tar");
        fs::write(&source, b"tar bytes").unwrap();
        let games = dir.join("steam-620");
        let older = store_in(&games, &source, json!({ "label": "a", "createdAt": "2026-10-30T10:00:00.000Z", "homeDir": "C:/Users/me" })).unwrap();
        store_in(&games, &source, json!({ "label": "b", "createdAt": "2026-10-31T10:00:00.000Z", "homeDir": "C:/Users/me" })).unwrap();
        assert_eq!(older["file"], "2026-10-30T10-00-00-000Z.tar");
        assert_eq!(older["sizeBytes"], 9);

        let listed = list_in(&games);
        assert_eq!(listed.iter().map(|b| b["label"].as_str().unwrap()).collect::<Vec<_>>(), ["b", "a"]);

        delete_in(&games, older["file"].as_str().unwrap()).unwrap();
        assert_eq!(list_in(&games).len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn labels_follow_format_date() {
        assert_eq!(backup_label("en", "2026-10-31T10:00:00.000Z", false), "Backup from 10-31-2026");
        assert_eq!(backup_label("xx", "2026-10-31T10:00:00.000Z", true), "Automatic backup from 31/10/2026");
    }

    #[test]
    fn save_folder_is_the_first_files_parent() {
        let report = json!({ "games": { "620": { "files": { "C:/Users/me/Saved Games/Portal 2/save.sav": {} } } } });
        assert_eq!(first_save_folder(&report, "620").as_deref(), Some("C:/Users/me/Saved Games/Portal 2"));
        assert_eq!(first_save_folder(&json!({ "games": {} }), "620"), None);
    }

    #[test]
    fn restore_moves_saves_into_this_machines_profile() {
        let root = temp_dir("restore");
        let game_backup = root.join("backup").join("620");
        let saved = game_backup.join("drive-C/Users/old/AppData/Game/save.dat");
        fs::create_dir_all(saved.parent().unwrap()).unwrap();
        fs::write(&saved, b"save").unwrap();
        fs::write(
            game_backup.join("mapping.yaml"),
            "name: '620'\ndrives:\n  drive-C: 'C:'\nbackups:\n  - name: .\n    when: 2026-10-31T10:00:00Z\n    files:\n      C:/Users/old/AppData/Game/save.dat:\n        hash: abc\n        size: 4\n    registry:\n      hash: ~\n",
        )
        .unwrap();

        let profile = normalize(&root.join("home").to_string_lossy());
        restore_mapping(&game_backup, "C:/Users/old", &profile, None, None).unwrap();
        assert_eq!(fs::read(root.join("home/AppData/Game/save.dat")).unwrap(), b"save");
        assert!(!saved.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pack_then_unpack_keeps_the_tree() {
        let root = temp_dir("pack");
        fs::create_dir_all(root.join("in/620/drive-C")).unwrap();
        fs::write(root.join("in/620/mapping.yaml"), b"x").unwrap();
        pack(&root.join("in"), &root.join("out.tar")).unwrap();
        tar::Archive::new(fs::File::open(root.join("out.tar")).unwrap()).unpack(root.join("back")).unwrap();
        assert_eq!(fs::read(root.join("back/620/mapping.yaml")).unwrap(), b"x");
        fs::remove_dir_all(root).unwrap();
    }
}
