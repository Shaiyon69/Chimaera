//! File and folder helpers behind the renderer's pickers and settings:
//! src/main/events/misc/file-system.ts, show-open-dialog.ts, open-folder.ts
//! and src/main/events/hardware/ in the Electron build.

use std::cmp::Ordering;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};
use tauri::{AppHandle, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

/// Absolute and with `..` resolved, so a path cannot climb back into a blocked folder.
fn normalize(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Windows paths compare without case.
fn fold(path: &Path) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    } else {
        path.to_path_buf()
    }
}

/// The picker browses every drive for game executables, so only the app's own
/// data (the SQLite store, imported session) is off limits.
pub fn assert_not_app_data(app_data: &Path, target: &str) -> Result<(), String> {
    if fold(&normalize(Path::new(target))).starts_with(fold(&normalize(app_data))) {
        return Err("Path not allowed".into());
    }
    Ok(())
}

/// `Intl.Collator(undefined, { numeric: true, sensitivity: "base" })`, close enough
/// for file names: digit runs compare as numbers, letters without case.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut run = String::new();
                    while let Some(c) = it.next_if(char::is_ascii_digit) {
                        run.push(c);
                    }
                    run
                };
                let (x, y) = (take(&mut a), take(&mut b));
                let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                let order = x.len().cmp(&y.len()).then_with(|| x.cmp(y));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                a.next();
                b.next();
            }
        }
    }
}

pub fn read_directory(dir: &str) -> Result<Value, String> {
    let mut entries: Vec<Value> = fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            // fs::metadata follows symlinks, like Node's stat.
            let meta = fs::metadata(&path).ok();
            let file_type = entry.file_type().ok();
            let dir_candidate = meta.as_ref().map(|m| m.is_dir()).or(file_type.map(|t| t.is_dir())).unwrap_or(false);
            let mac_app = dir_candidate && name.to_lowercase().ends_with(".app");
            let is_dir = dir_candidate && !mac_app;
            let is_file = meta.as_ref().map(|m| m.is_file()).or(file_type.map(|t| t.is_file())).unwrap_or(false) || mac_app;
            // Electron takes the text after the last dot, so ".bashrc" has extension "bashrc".
            let extension = match name.rsplit_once('.') {
                Some((_, ext)) if is_file => ext.to_lowercase(),
                _ => String::new(),
            };
            let size = if is_file { meta.map(|m| m.len()).unwrap_or(0) } else { 0 };
            json!({
                "name": name,
                "path": path.to_string_lossy(),
                "isDirectory": is_dir,
                "isFile": is_file,
                "extension": extension,
                "size": size,
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        let (a_dir, b_dir) = (a["isDirectory"].as_bool(), b["isDirectory"].as_bool());
        b_dir.cmp(&a_dir).then_with(|| natural_cmp(a["name"].as_str().unwrap_or_default(), b["name"].as_str().unwrap_or_default()))
    });
    Ok(Value::Array(entries))
}

pub fn path_info(path: &str) -> Value {
    match fs::metadata(path) {
        Ok(meta) => json!({ "exists": true, "isDirectory": meta.is_dir(), "isFile": meta.is_file() }),
        Err(_) => json!({ "exists": false, "isDirectory": false, "isFile": false }),
    }
}

pub fn list_drives() -> Value {
    if cfg!(windows) {
        let drives: Vec<String> = (b'A'..=b'Z').map(|letter| format!("{}:\\", letter as char)).filter(|root| Path::new(root).exists()).collect();
        json!(drives)
    } else {
        json!(["/"])
    }
}

/// `getAvailableDrives`: `{ root, label, free, total }` per mounted disk. Windows labels
/// read `C:`; Linux labels are the mount point.
pub fn available_drives() -> Value {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let drives: Vec<Value> = disks
        .iter()
        .filter(|disk| disk.total_space() > 0)
        .map(|disk| {
            let root = disk.mount_point().to_string_lossy().into_owned();
            let label = if cfg!(windows) { root.chars().take(2).collect() } else { root.clone() };
            json!({ "root": root, "label": label, "free": disk.available_space(), "total": disk.total_space() })
        })
        .collect();
    json!(drives)
}

/// `{ free, total }` of the disk holding `target`, measured at its nearest existing
/// folder so a download path that does not exist yet still reports its drive.
pub fn disk_usage(target: &str) -> Value {
    let target = normalize(Path::new(target));
    let Some(existing) = target.ancestors().find(|p| p.exists()) else {
        return Value::Null;
    };
    let existing = fold(existing);
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .iter()
        .filter(|disk| existing.starts_with(fold(disk.mount_point())))
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
        .filter(|disk| disk.total_space() > 0)
        .map_or(Value::Null, |disk| json!({ "free": disk.available_space(), "total": disk.total_space() }))
}

pub fn can_write(dir: &str) -> bool {
    let probe = Path::new(dir).join(".hydra-write-test");
    fs::write(&probe, b"").and_then(|_| fs::remove_file(&probe)).is_ok()
}

/// shell.openPath runs files, so like Electron this only opens folders, creating them first.
pub fn prepare_folder(folder: &str) -> Result<(), String> {
    let path = Path::new(folder);
    if !path.is_absolute() {
        return Err("openFolder requires an absolute path".into());
    }
    if path.exists() && !path.is_dir() {
        return Err("openFolder target is not a directory".into());
    }
    fs::create_dir_all(path).map_err(|e| e.to_string())
}

/// Electron's `dialog.showOpenDialog` for the options the renderer uses:
/// title, defaultPath, filters and the openFile / openDirectory / multiSelections properties.
pub async fn open_dialog(app: &AppHandle, window: &WebviewWindow, options: &Value) -> Result<Value, String> {
    let has = |name: &str| options["properties"].as_array().is_some_and(|p| p.iter().any(|v| v == name));
    let (folders, multiple) = (has("openDirectory"), has("multiSelections"));

    let mut dialog = app.dialog().file().set_parent(window);
    if let Some(title) = options["title"].as_str() {
        dialog = dialog.set_title(title);
    }
    if let Some(default) = options["defaultPath"].as_str().filter(|p| !p.is_empty()) {
        let default = Path::new(default);
        if default.is_dir() {
            dialog = dialog.set_directory(default);
        } else if let Some(parent) = default.parent().filter(|p| p.is_dir()) {
            dialog = dialog.set_directory(parent);
        }
    }
    for filter in options["filters"].as_array().into_iter().flatten() {
        let extensions: Vec<&str> = filter["extensions"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        dialog = dialog.add_filter(filter["name"].as_str().unwrap_or_default(), &extensions);
    }

    // The blocking pickers must stay off the main thread.
    let picked = tauri::async_runtime::spawn_blocking(move || match (folders, multiple) {
        (true, true) => dialog.blocking_pick_folders(),
        (true, false) => dialog.blocking_pick_folder().map(|p| vec![p]),
        (false, true) => dialog.blocking_pick_files(),
        (false, false) => dialog.blocking_pick_file().map(|p| vec![p]),
    })
    .await
    .map_err(|e| e.to_string())?;

    let paths: Vec<String> = picked.unwrap_or_default().into_iter().filter_map(|p| p.into_path().ok()).map(|p| p.to_string_lossy().into_owned()).collect();
    Ok(json!({ "canceled": paths.is_empty(), "filePaths": paths }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_app_data_and_climbing_back_into_it() {
        let app_data = std::env::temp_dir().join("chimaera-app-data");
        let inside = app_data.join("chimaera.db");
        let climbed = app_data.join("..").join("chimaera-app-data").join("x");
        assert!(assert_not_app_data(&app_data, &app_data.to_string_lossy()).is_err());
        assert!(assert_not_app_data(&app_data, &inside.to_string_lossy()).is_err());
        assert!(assert_not_app_data(&app_data, &climbed.to_string_lossy()).is_err());
        assert!(assert_not_app_data(&app_data, &std::env::temp_dir().join("chimaera-app-data-other").to_string_lossy()).is_ok());
    }

    #[test]
    fn sorts_names_like_a_numeric_collator() {
        let mut names = vec!["disc10", "Disc2", "disc1", "alpha", "Beta"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["alpha", "Beta", "disc1", "Disc2", "disc10"]);
    }

    #[test]
    fn lists_folders_first_with_dot_extensions() {
        let dir = std::env::temp_dir().join(format!("chimaera-fs-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(dir.join("zfolder")).unwrap();
        fs::write(dir.join("Game.EXE"), b"abc").unwrap();
        fs::write(dir.join(".bashrc"), b"").unwrap();
        let entries = read_directory(&dir.to_string_lossy()).unwrap();
        let names: Vec<&str> = entries.as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["zfolder", ".bashrc", "Game.EXE"]);
        assert_eq!(entries[2]["extension"], "exe");
        assert_eq!(entries[2]["size"], 3);
        assert_eq!(entries[1]["extension"], "bashrc");
        assert!(can_write(&dir.to_string_lossy()));
        assert!(!dir.join(".hydra-write-test").exists());
        assert_eq!(path_info(&dir.join("missing").to_string_lossy())["exists"], false);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn measures_a_missing_path_at_its_drive() {
        let missing = std::env::temp_dir().join("chimaera-not-there").join("deeper");
        let usage = disk_usage(&missing.to_string_lossy());
        assert!(usage["total"].as_u64().unwrap() > 0);
    }
}
