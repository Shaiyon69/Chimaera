//! A finished download's folder, its installer and its source archive. Ported from
//! src/main/helpers/find-game-installer.ts and the redistributable lists in
//! src/main/helpers/game-executable-ranking.ts.

use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::store::Store;

const MAX_INSTALLER_DEPTH: usize = 2;

pub(crate) const REDIST_DIRECTORIES: &[&str] = &[
    "_commonredist", "commonredist", "steamworks shared", "redist", "redists", "_redist", "directx", "dotnet", "dotnetfx", "vcredist", "openal", "physx",
    "prerequisites", "prereq",
];
const REDIST_FILE_PREFIXES: &[&str] = &[
    "vcredist", "vc_redist", "dotnetfx", "dxsetup", "dxwebsetup", "directx", "oalinst", "xnafx", "physx", "ue3prereqsetup", "ue4prereqsetup", "ue5prereqsetup",
    "ue3redist",
];
const SHARED_INSTALLER_PREFIXES: &[&str] = &["uplayinstaller", "pbsvc", "pbsetup", "setup_battleye", "beservice", "social-club-setup", "easyanticheat", "dauservicesetup"];
const SECONDARY_WORDS: &[&str] = &["dlc", "patch", "update", "addon", "soundtrack", "ost"];

/// The user's download folder setting, else the system Downloads folder.
pub fn downloads_path(store: &Store, default: &Path) -> PathBuf {
    store.get("", "userPreferences").ok().and_then(|p| p["downloadsPath"].as_str().map(PathBuf::from)).unwrap_or_else(|| default.to_path_buf())
}

fn download_root(download: &Value, default_downloads: &Path) -> PathBuf {
    download["downloadPath"].as_str().map(PathBuf::from).unwrap_or_else(|| default_downloads.to_path_buf())
}

/// getDownloadFolderPath: where the download's files went, when it has a folder name.
pub fn folder_path(download: &Value, default_downloads: &Path) -> Option<PathBuf> {
    download["folderName"].as_str().map(|name| download_root(download, default_downloads).join(name))
}

/// findSourceArchiveFile: the archive the game was extracted from, if it is still a file
/// inside the download folder.
pub fn source_archive(download: &Value, default_downloads: &Path) -> Option<PathBuf> {
    let name = download["sourceArchiveName"].as_str().filter(|n| !n.is_empty())?;
    // Stricter than Electron's path.relative check: any `..` or root in the name is refused.
    if !Path::new(name).components().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    let archive = download_root(download, default_downloads).join(name);
    if Some(&archive) == folder_path(download, default_downloads).as_ref() {
        return None;
    }
    archive.is_file().then_some(archive)
}

/// findDownloadInstaller: the download folder's installer, when it has exactly one best pick.
pub fn download_installer(download: &Value, default_downloads: &Path) -> Option<PathBuf> {
    if download.is_null() || cfg!(target_os = "macos") {
        return None;
    }
    find_installer(&folder_path(download, default_downloads).filter(|f| f.is_dir())?)
}

fn installer_priority(name: &str) -> Option<u8> {
    let name = name.to_lowercase();
    if name == "setup.exe" {
        Some(0)
    } else if (name.starts_with("setup") || name.starts_with("install")) && name.ends_with(".exe") {
        Some(1)
    } else {
        None
    }
}

pub(crate) fn is_third_party_installer(relative: &Path) -> bool {
    let segments: Vec<String> = relative.iter().map(|s| s.to_string_lossy().to_lowercase()).collect();
    let Some((basename, folders)) = segments.split_last() else { return false };
    REDIST_FILE_PREFIXES.iter().chain(SHARED_INSTALLER_PREFIXES).any(|p| basename.starts_with(p)) || folders.iter().any(|f| REDIST_DIRECTORIES.contains(&f.as_str()))
}

fn is_secondary(name: &str) -> bool {
    name.to_lowercase().split(['_', '-', '.', ' ']).any(|word| SECONDARY_WORDS.contains(&word))
}

/// (depth, name priority, is DLC/patch, name length): lower is better.
type Rank = (usize, u8, bool, usize);

fn collect(root: &Path, dir: &Path, depth: usize, out: &mut Vec<(Rank, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let (path, name) = (entry.path(), entry.file_name().to_string_lossy().into_owned());
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            if depth < MAX_INSTALLER_DEPTH && !REDIST_DIRECTORIES.contains(&name.to_lowercase().as_str()) {
                collect(root, &path, depth + 1, out);
            }
            continue;
        }
        let Some(priority) = installer_priority(&name).filter(|_| kind.is_file()) else { continue };
        if is_third_party_installer(path.strip_prefix(root).unwrap_or(&path)) {
            continue;
        }
        out.push(((depth, priority, is_secondary(&name), name.chars().count()), path));
    }
}

/// findGameInstaller: the shallowest, best-named installer; a tie means no pick.
pub fn find_installer(folder: &Path) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    collect(folder, folder, 0, &mut candidates);
    candidates.sort_by_key(|(rank, _)| *rank);
    match candidates.as_slice() {
        [(best, path), (next, _), ..] if best != next => Some(path.clone()),
        [(_, path)] => Some(path.clone()),
        _ => None,
    }
}

/// readArchiveVolume: (pattern, base name) of a split-archive volume name. Matches
/// `x.partN.rar`, `x.7z|zip|rar.NNN` and `x.rar|rNN`, in that order, case-insensitive.
fn archive_volume(name: &str) -> Option<(u8, String)> {
    let lower = name.to_ascii_lowercase();
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    // to_ascii_lowercase keeps byte offsets, so `lower` indices slice `name`.
    let base = |end: usize| if cfg!(windows) { lower[..end].to_string() } else { name[..end].to_string() };
    if let Some(i) = lower.strip_suffix(".rar").and_then(|stem| stem.rfind(".part").filter(|&i| digits(&stem[i + 5..]))) {
        return Some((0, base(i)));
    }
    let i = lower.rfind('.')?;
    let ext = &lower[i + 1..];
    if ext.len() == 3 && digits(ext) && [".7z", ".zip", ".rar"].iter().any(|e| lower[..i].ends_with(e)) {
        return Some((1, base(i)));
    }
    (ext == "rar" || (ext.len() == 3 && ext.starts_with('r') && digits(&ext[1..]))).then(|| (2, base(i)))
}

/// listArchiveVolumes: every volume of the archive's split set, else the archive alone.
pub fn archive_volumes(archive: &Path) -> Vec<PathBuf> {
    let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (Some(volume), Some(dir)) = (archive_volume(&name), archive.parent()) else { return vec![archive.to_path_buf()] };
    let mut volumes: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && archive_volume(&e.file_name().to_string_lossy()).as_ref() == Some(&volume))
        .map(|e| e.path())
        .collect();
    if volumes.is_empty() {
        volumes.push(archive.to_path_buf());
    }
    volumes
}

/// fs.rm with force and maxRetries 5: Windows often holds a just-closed file for a moment.
pub fn remove_path(path: &Path) -> std::io::Result<()> {
    let mut attempt = 0;
    loop {
        let result = match fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
            Ok(_) => fs::remove_file(path),
            Err(e) => Err(e),
        };
        match result {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) if attempt < 5 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(200 * attempt));
            }
            other => return other,
        }
    }
}

/// rom-path-utils normalizePath: one separator style, lowercase on Windows.
fn norm_path(p: &Path) -> PathBuf {
    let s = p.components().collect::<PathBuf>().to_string_lossy().into_owned();
    PathBuf::from(if cfg!(windows) { s.to_lowercase() } else { s })
}

/// rom-path-utils isWithin: `child` is `parent` or below it.
fn is_within(child: &str, parent: &Path) -> bool {
    norm_path(Path::new(child)).starts_with(norm_path(parent))
}

/// deleteGameFolder: removes the download's files (and, if asked, its source archive),
/// forgets the download, and unlinks what lived in that folder from the game.
// shortcut: the download queue and RetroArch counts are not resynced after a delete; resync them here if counts go stale.
pub fn delete_game_folder(store: &Store, default_downloads: &Path, key: &str, delete_source_archive: bool) -> Result<(), String> {
    let download = store.get("downloads", key)?;
    if download.is_null() {
        return Ok(());
    }
    let folder = folder_path(&download, default_downloads);
    if let Some(folder) = &folder {
        remove_path(folder).map_err(|e| format!("Could not delete {}: {e}", folder.display()))?;
        let mut meta = folder.clone().into_os_string();
        meta.push(".meta");
        remove_path(Path::new(&meta)).map_err(|e| e.to_string())?;
    }
    if delete_source_archive {
        for volume in source_archive(&download, default_downloads).map(|a| archive_volumes(&a)).unwrap_or_default() {
            if let Err(e) = remove_path(&volume) {
                eprintln!("[deleteGameFolder] failed to delete source archive {}: {e}", volume.display());
            }
        }
    }
    store.del("downloads", key)?;

    crate::games::update(store, key, |game| {
        game.insert("installerSizeInBytes".into(), Value::Null);
        let executable_removed = match (&folder, game.get("executablePath").and_then(Value::as_str)) {
            (Some(folder), Some(exe)) => is_within(exe, folder),
            _ => true,
        };
        if executable_removed {
            for field in ["executablePath", "installedSizeInBytes"] {
                game.insert(field.into(), Value::Null);
            }
            game.insert("automaticCloudSync".into(), Value::Bool(false));
        }
        let (Some(folder), Some(Value::Array(discs))) = (&folder, game.get("discs")) else { return };
        let remaining: Vec<Value> = discs.iter().filter(|d| !d["path"].as_str().is_some_and(|p| is_within(p, folder))).cloned().collect();
        if remaining.len() != discs.len() {
            let selected = game.get("selectedDiscPath").cloned().unwrap_or(Value::Null);
            if !remaining.iter().any(|d| d["path"] == selected) {
                game.insert("selectedDiscPath".into(), Value::Null);
            }
            game.insert("discs".into(), Value::Array(remaining));
        }
    })?;
    Ok(())
}

/// deleteArchive: deletes an extracted archive and clears the installer size of the
/// download whose folder it was. Unlike Electron, only files under a download folder.
pub fn delete_archive(store: &Store, default_downloads: &Path, file: &str) -> Result<bool, String> {
    let downloads = store.entries("downloads")?;
    let mut roots: Vec<PathBuf> = downloads.iter().filter_map(|(_, d)| d["downloadPath"].as_str().map(PathBuf::from)).collect();
    roots.push(downloads_path(store, default_downloads));
    // The renderer names the file, so refuse anything outside the download folders.
    if !Path::new(file).is_absolute() || Path::new(file).components().any(|c| c == Component::ParentDir) || !roots.iter().any(|r| is_within(file, r)) {
        return Ok(false);
    }
    if Path::new(file).is_file() {
        if let Err(e) = fs::remove_file(file) {
            eprintln!("Failed to delete archive: {file}: {e}");
            return Ok(false);
        }
    }
    let owner = downloads.iter().find(|(_, d)| match (d["downloadPath"].as_str(), d["folderName"].as_str()) {
        (Some(root), Some(name)) => norm_path(&Path::new(root).join(name)) == norm_path(Path::new(file)),
        _ => false,
    });
    if let Some((key, _)) = owner {
        crate::games::update(store, key, |game| {
            game.insert("installerSizeInBytes".into(), Value::Null);
        })?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chimaera-installer-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn picks_the_main_setup_over_redists_and_dlc() {
        let dir = temp_dir("pick");
        fs::create_dir_all(dir.join("_CommonRedist")).unwrap();
        fs::write(dir.join("_CommonRedist").join("setup.exe"), "").unwrap();
        fs::write(dir.join("vcredist_x64.exe"), "").unwrap();
        fs::write(dir.join("setup_dlc.exe"), "").unwrap();
        fs::write(dir.join("setup_game.exe"), "").unwrap();
        assert_eq!(find_installer(&dir), Some(dir.join("setup_game.exe")));
        fs::write(dir.join("setup.exe"), "").unwrap();
        assert_eq!(find_installer(&dir), Some(dir.join("setup.exe")));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_tie_is_no_pick() {
        let dir = temp_dir("tie");
        fs::write(dir.join("setup_a.exe"), "").unwrap();
        fs::write(dir.join("setup_b.exe"), "").unwrap();
        assert_eq!(find_installer(&dir), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn archive_volumes_match_electron_patterns() {
        assert_eq!(archive_volume("Game.part01.rar"), archive_volume("game.PART2.rar"));
        assert_eq!(archive_volume("game.7z.001").map(|v| v.0), Some(1));
        assert_eq!(archive_volume("game.r00").map(|v| v.0), Some(2));
        assert_eq!(archive_volume("game.rar").map(|v| v.0), Some(2));
        assert_eq!(archive_volume("game.zip"), None);
        assert_eq!(archive_volume("game.exe.001"), None);

        let dir = temp_dir("volumes");
        for f in ["g.part1.rar", "g.part2.rar", "h.part1.rar", "g.zip"] {
            fs::write(dir.join(f), "").unwrap();
        }
        let mut volumes = archive_volumes(&dir.join("g.part1.rar"));
        volumes.sort();
        assert_eq!(volumes, vec![dir.join("g.part1.rar"), dir.join("g.part2.rar")]);
        assert_eq!(archive_volumes(&dir.join("g.zip")), vec![dir.join("g.zip")]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn delete_game_folder_unlinks_only_what_lived_there() {
        let dir = temp_dir("delete");
        let folder = dir.join("game");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("game.exe"), "").unwrap();
        fs::write(dir.join("game.meta"), "").unwrap();
        fs::write(dir.join("game.zip"), "").unwrap();
        let store = Store::memory().unwrap();
        let root = dir.to_string_lossy();
        store.put("downloads", "steam:1", &json!({ "downloadPath": root, "folderName": "game", "sourceArchiveName": "game.zip" })).unwrap();
        let inside = folder.join("disc.iso").to_string_lossy().into_owned();
        store
            .put(
                "games",
                "steam:1",
                &json!({ "executablePath": folder.join("game.exe").to_string_lossy(), "installerSizeInBytes": 5, "automaticCloudSync": true,
                    "discs": [{ "path": inside }, { "path": "D:/other.iso" }], "selectedDiscPath": inside }),
            )
            .unwrap();

        delete_game_folder(&store, Path::new(""), "steam:1", true).unwrap();
        assert!(!folder.exists() && !dir.join("game.meta").exists() && !dir.join("game.zip").exists());
        assert!(store.get("downloads", "steam:1").unwrap().is_null());
        let game = store.get("games", "steam:1").unwrap();
        assert_eq!(game["executablePath"], Value::Null);
        assert_eq!(game["installerSizeInBytes"], Value::Null);
        assert_eq!(game["automaticCloudSync"], json!(false));
        assert_eq!(game["discs"], json!([{ "path": "D:/other.iso" }]));
        assert_eq!(game["selectedDiscPath"], Value::Null);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn delete_archive_refuses_files_outside_download_folders() {
        let dir = temp_dir("delarchive");
        fs::write(dir.join("game"), "").unwrap();
        let store = Store::memory().unwrap();
        store.put("downloads", "steam:1", &json!({ "downloadPath": dir.to_string_lossy(), "folderName": "game" })).unwrap();
        store.put("games", "steam:1", &json!({ "installerSizeInBytes": 5 })).unwrap();
        let outside = std::env::temp_dir().join(format!("chimaera-outside-{}", std::process::id()));
        fs::write(&outside, "").unwrap();

        assert!(!delete_archive(&store, &dir.join("nowhere"), &outside.to_string_lossy()).unwrap());
        assert!(outside.exists());
        assert!(delete_archive(&store, &dir.join("nowhere"), &dir.join("game").to_string_lossy()).unwrap());
        assert!(!dir.join("game").exists());
        assert_eq!(store.get("games", "steam:1").unwrap()["installerSizeInBytes"], Value::Null);
        fs::remove_file(outside).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn source_archive_stays_inside_the_download_folder() {
        let dir = temp_dir("archive");
        fs::write(dir.join("game.zip"), "").unwrap();
        let download = |name: &str| json!({ "downloadPath": dir.to_string_lossy(), "folderName": "game", "sourceArchiveName": name });
        assert_eq!(source_archive(&download("game.zip"), Path::new("")), Some(dir.join("game.zip")));
        assert_eq!(source_archive(&download("../game.zip"), Path::new("")), None);
        assert_eq!(source_archive(&download("game"), Path::new("")), None);
        fs::remove_dir_all(&dir).unwrap();
    }
}
