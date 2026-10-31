//! A finished download's folder, its installer and its source archive. Ported from
//! src/main/helpers/find-game-installer.ts and the redistributable lists in
//! src/main/helpers/game-executable-ranking.ts.

use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::store::Store;

const MAX_INSTALLER_DEPTH: usize = 2;

const REDIST_DIRECTORIES: &[&str] = &[
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

fn is_third_party_installer(relative: &Path) -> bool {
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
