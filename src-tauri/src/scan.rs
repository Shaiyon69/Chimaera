//! `scanInstalledGames`, `cancelScanInstalledGames` and `addScannedGames`, ported from
//! src/main/events/library/scan-installed-games.ts and add-scanned-game.ts, with the
//! executable catalogue (services/game-executables*.ts), the ranking
//! (helpers/game-executable-ranking.ts) and the Steam install readers they use.
//! Chimaera runs in local mode, so Electron's remote createGame call is dropped.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::achievement_files::real_path;
use crate::games::{game_key, with_executable_path};
use crate::installer::{is_third_party_installer, REDIST_DIRECTORIES};
use crate::library::iso_now;
use crate::store::Store;

const SHOP: &str = "steam";
const CHUNK_SIZE: usize = 4;
const UNNAMED_PATH_CANDIDATE_LIMIT: usize = 40;
const ASSET_LOOKUP_CONCURRENCY: usize = 8;
const WINDOWS_GAME_DIRECTORIES: &[&str] = &[r"C:\Games", r"D:\Games"];
const SHARED_RUNTIME_FILES: &[&str] = &[
    "unitycrashhandler32.exe", "unitycrashhandler64.exe", "crashreportclient.exe", "crashpad_handler.exe", "unrealcefsubprocess.exe", "epicwebhelper.exe",
    "start_protected_game.exe", "javaw.exe", "java.exe", "dotnet.exe", "setup.exe",
];

#[derive(Clone, Debug, PartialEq)]
pub struct Known {
    pub exe: String,
    pub name: String,
}

/// objectId -> executables, plus the ids in JS `Object.keys` order (numeric ascending).
#[derive(Default)]
pub struct Catalogue {
    ids: Vec<String>,
    map: HashMap<String, Vec<Known>>,
}

impl Catalogue {
    fn get(&self, id: &str) -> Option<&[Known]> {
        self.map.get(id).map(Vec::as_slice)
    }

    fn iter(&self) -> impl Iterator<Item = (&String, &[Known])> {
        self.ids.iter().map(|id| (id, self.map[id].as_slice()))
    }
}

/// normalizeGameExecutableCatalog: keeps the executables this platform can run.
pub fn normalize_catalogue(raw: &Value, windows: bool) -> Catalogue {
    let mut catalogue = Catalogue::default();
    for (id, list) in raw.as_object().into_iter().flatten() {
        let executables: Vec<Known> = list
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| matches!(e["os"].as_str(), Some("win32")) || (!windows && e["os"].as_str() == Some("linux")))
            .filter_map(|e| {
                let lowered = e["name"].as_str()?.to_lowercase();
                let name = lowered.strip_prefix('>').unwrap_or(&lowered);
                let exe = name.rsplit('/').next().unwrap_or(name).to_string();
                Some(Known { exe, name: if windows { name.replace('/', "\\") } else { name.to_string() } })
            })
            .collect();
        if !executables.is_empty() {
            catalogue.ids.push(id.clone());
            catalogue.map.insert(id.clone(), executables);
        }
    }
    catalogue.ids.sort_by_key(|id| (id.parse::<u64>().unwrap_or(u64::MAX), id.clone()));
    catalogue
}

static CATALOGUE: Mutex<Option<Arc<Catalogue>>> = Mutex::new(None);

/// GameExecutables.ensureLoaded(true): fetched once, refetched while it fails or is empty.
async fn catalogue(store: &Store, version: &str) -> Option<Arc<Catalogue>> {
    if let Some(loaded) = CATALOGUE.lock().unwrap().clone() {
        return Some(loaded);
    }
    let payload = json!({ "method": "get", "url": "/catalogue/steam/executables", "options": { "needsAuth": false } });
    let raw = crate::hydra_api::call(store, version, &payload)
        .await
        .map_err(|e| eprintln!("[scan] failed to load game executable catalogue: {e}"))
        .ok()?;
    let loaded = Arc::new(normalize_catalogue(&raw, cfg!(windows)));
    if loaded.ids.is_empty() {
        return None;
    }
    *CATALOGUE.lock().unwrap() = Some(loaded.clone());
    Some(loaded)
}

// ---- Ranking (game-executable-ranking.ts) ----

fn segments(p: &str) -> Vec<String> {
    p.to_lowercase().split(['\\', '/']).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

fn basename(p: &str) -> Option<String> {
    segments(p).pop()
}

fn is_redist_dir(name: &str) -> bool {
    REDIST_DIRECTORIES.contains(&name.to_lowercase().as_str())
}

fn is_redist_path(relative: &str) -> bool {
    is_third_party_installer(Path::new(relative)) || basename(relative).is_some_and(|b| SHARED_RUNTIME_FILES.contains(&b.as_str()))
}

fn ends_with(path: &[String], suffix: &[String]) -> bool {
    suffix.len() <= path.len() && path[path.len() - suffix.len()..] == *suffix
}

fn suffixes(executables: &[Known]) -> Vec<Vec<String>> {
    executables.iter().map(|e| segments(&e.name)).filter(|s| s.len() > 1).collect()
}

fn named(candidate: &str, suffixes: &[Vec<String>]) -> bool {
    let segs = segments(candidate);
    suffixes.iter().any(|s| ends_with(&segs, s))
}

fn directory_of(p: &str) -> String {
    let segs = segments(p);
    segs[..segs.len().saturating_sub(1)].join("/")
}

/// rankExecutableCandidates with the "installation" scope.
pub fn rank_executable(files: &[String], executables: &[Known]) -> Option<String> {
    let mut indexes: HashMap<&str, usize> = HashMap::new();
    for (i, e) in executables.iter().enumerate() {
        indexes.entry(e.exe.as_str()).or_insert(i);
    }
    let candidates: Vec<&String> = files.iter().filter(|f| basename(f).is_some_and(|b| indexes.contains_key(b.as_str())) && !is_redist_path(f)).collect();
    if candidates.len() <= 1 {
        return candidates.first().map(|c| c.to_string());
    }
    let suffixes = suffixes(executables);
    let preferred: Vec<&String> = candidates.iter().copied().filter(|c| named(c, &suffixes)).collect();
    let pool = if preferred.is_empty() { candidates } else { preferred };
    if pool.len() == 1 {
        return Some(pool[0].clone());
    }
    let is_nested_copy = |p: &str, other: &str| {
        let (ps, os) = (segments(p), segments(other));
        p != other && os.len() < ps.len() && ends_with(&ps, &os)
    };
    let originals: Vec<&String> = pool.iter().copied().filter(|c| !pool.iter().any(|o| is_nested_copy(c, o))).collect();
    let remaining = if originals.is_empty() { pool } else { originals };
    if remaining.len() == 1 {
        return Some(remaining[0].clone());
    }
    let mut counts: HashMap<String, usize> = HashMap::new();
    for f in files {
        if let Some(b) = basename(f) {
            *counts.entry(b).or_default() += 1;
        }
    }
    let score = |p: &str| {
        let dir = directory_of(p);
        let unshared = files.iter().filter(|f| directory_of(f) == dir && basename(f).is_some_and(|b| counts[&b] == 1)).count();
        let index = basename(p).and_then(|b| indexes.get(b.as_str()).copied()).unwrap_or(usize::MAX);
        (-(unshared as i64), segments(p).len(), index)
    };
    let mut ranked: Vec<_> = remaining.iter().map(|p| (score(p), p.to_string())).collect();
    ranked.sort();
    (ranked[0].0 != ranked[1].0).then(|| ranked[0].1.clone())
}

fn installation_of(p: &str) -> String {
    let segs = segments(p);
    if segs.len() > 1 { segs[0].clone() } else { String::new() }
}

/// rankExecutableCandidates with the "library" scope: the pick must sit in one top folder.
pub fn rank_across_installations(files: &[String], executables: &[Known]) -> Option<String> {
    let names: HashSet<&str> = executables.iter().map(|e| e.exe.as_str()).collect();
    let candidates: Vec<&String> = files.iter().filter(|f| basename(f).is_some_and(|b| names.contains(b.as_str())) && !is_redist_path(f)).collect();
    if candidates.is_empty() {
        return None;
    }
    let suffixes = suffixes(executables);
    let named_installations: HashSet<String> = candidates.iter().filter(|c| named(c, &suffixes)).map(|c| installation_of(c)).collect();
    let installations = if named_installations.len() == 1 { named_installations } else { candidates.iter().map(|c| installation_of(c)).collect() };
    if installations.len() != 1 {
        return None;
    }
    let installation = installations.into_iter().next()?;
    let within: Vec<(&String, String)> = files
        .iter()
        .filter(|f| installation_of(f) == installation)
        .map(|f| (f, if installation.is_empty() { f.clone() } else { f[f.find(['\\', '/']).map_or(0, |i| i + 1)..].to_string() }))
        .collect();
    let scoped: Vec<String> = within.iter().map(|(_, s)| s.clone()).collect();
    let pick = rank_executable(&scoped, executables)?;
    within.iter().find(|(_, s)| *s == pick).map(|(o, _)| o.to_string())
}

// ---- Steam installs (services/steam.ts, steam-installation-core.ts) ----

/// getSteamLibraryFolders: Steam's own folder plus every library in libraryfolders.vdf.
pub fn library_folders(steam: &Path) -> Vec<PathBuf> {
    // The registry's SteamPath can be lowercase with forward slashes; the vdf spelling wins.
    let same = |a: &Path, b: &Path| if cfg!(windows) { a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase() } else { a == b };
    let mut all = vec![if cfg!(windows) { PathBuf::from(steam.to_string_lossy().replace('/', "\\")) } else { steam.to_path_buf() }];
    for manifest in ["steamapps/libraryfolders.vdf", "config/libraryfolders.vdf"] {
        let Ok(content) = fs::read_to_string(steam.join(manifest)) else { continue };
        let folders = parse_library_folders(&content);
        if folders.is_empty() {
            continue;
        }
        for folder in folders {
            match all.iter().position(|a| same(a, &folder)) {
                Some(i) => all[i] = folder,
                None => all.push(folder),
            }
        }
        break;
    }
    all
}

/// `"path" "<dir>"` and `"<n>" "<dir>"` pairs (old and new libraryfolders.vdf formats).
pub fn parse_library_folders(content: &str) -> Vec<PathBuf> {
    // Splitting on quotes puts quoted strings at odd indexes and the gaps between them at even ones.
    let parts: Vec<&str> = content.split('"').collect();
    let mut folders = Vec::new();
    for i in (1..parts.len().saturating_sub(2)).step_by(2) {
        let (key, gap, value) = (parts[i], parts[i + 1], parts[i + 2]);
        let keyed = key == "path" || (!key.is_empty() && key.bytes().all(|b| b.is_ascii_digit()));
        if keyed && gap.trim().is_empty() && !value.is_empty() {
            let path = PathBuf::from(value.replace(r"\\", r"\"));
            if path.is_absolute() && !folders.contains(&path) {
                folders.push(path);
            }
        }
    }
    folders
}

/// readManifestValue: the first `"key" "value"` pair, key matched case-insensitively.
pub fn manifest_value(content: &str, key: &str) -> Option<String> {
    let lowered = content.to_ascii_lowercase();
    let needle = format!("\"{}\"", key.to_ascii_lowercase());
    let mut from = 0;
    while let Some(at) = lowered[from..].find(&needle) {
        let rest = content[from + at + needle.len()..].trim_start();
        from += at + needle.len();
        let Some(body) = rest.strip_prefix('"') else { continue };
        let mut value = String::new();
        let mut chars = body.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(value),
                '\\' => value.push(chars.next()?),
                c => value.push(c),
            }
        }
        return None;
    }
    None
}

/// listInstalledSteamApps: fully installed apps whose folder exists, as (appId, folder).
pub fn installed_apps(libraries: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let mut apps: Vec<(String, PathBuf)> = Vec::new();
    for library in libraries {
        let Ok(entries) = fs::read_dir(library.join("steamapps")) else { continue };
        for entry in entries.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_file())) {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let Some(id) = name.strip_prefix("appmanifest_").and_then(|n| n.strip_suffix(".acf")) else { continue };
            if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) || apps.iter().any(|(a, _)| a == id) {
                continue;
            }
            let Ok(content) = fs::read_to_string(entry.path()) else { continue };
            let installed = manifest_value(&content, "StateFlags").and_then(|f| f.parse::<i64>().ok()).is_some_and(|f| f & 4 != 0);
            if manifest_value(&content, "appid").as_deref() != Some(id) || !installed {
                continue;
            }
            let Some(dir) = manifest_value(&content, "installdir").filter(|d| !d.is_empty()) else { continue };
            // Stricter than Electron's path.relative check: only plain folder names.
            if !Path::new(&dir).components().all(|c| matches!(c, Component::Normal(_))) {
                continue;
            }
            let folder = library.join("steamapps").join("common").join(dir);
            if folder.is_dir() {
                apps.push((id.to_string(), folder));
            }
        }
    }
    apps
}

// ---- Missing executables (services/clear-missing-executables.ts) ----

fn inside(path: &str, dir: &Path) -> bool {
    let (p, d) = (Path::new(path), dir);
    if cfg!(windows) {
        let (p, d) = (path.to_lowercase().replace('/', "\\"), d.to_string_lossy().to_lowercase().replace('/', "\\"));
        return p.starts_with(&format!("{}\\", d.trim_end_matches('\\')));
    }
    p != d && p.starts_with(d)
}

/// isExecutableMissingFromAvailableStorage: gone, and its drive or mount is present.
fn missing_from_available_storage(executable: &str) -> bool {
    let path = Path::new(executable);
    if !path.is_absolute() || path.exists() {
        return false;
    }
    if cfg!(windows) {
        return path.ancestors().last().is_some_and(Path::exists);
    }
    let Some(ancestor) = path.ancestors().skip(1).find(|a| a.exists()) else { return false };
    let a = ancestor.to_string_lossy();
    let parts: Vec<&str> = a.split('/').filter(|s| !s.is_empty()).collect();
    let mount_container = match parts.as_slice() {
        [] => true,
        [first] => ["media", "mnt", "Volumes"].contains(first),
        ["run", "media"] | ["media", _] => true,
        ["run", "media", _] => true,
        _ => false,
    };
    !mount_container && fs::read_dir(ancestor).is_ok_and(|mut e| e.next().is_some())
}

fn executable_gone(executable: &str, libraries: &[PathBuf]) -> bool {
    match libraries.iter().find(|l| inside(executable, &l.join("steamapps").join("common"))) {
        Some(library) => !Path::new(executable).exists() && library.join("steamapps").exists(),
        None => missing_from_available_storage(executable),
    }
}

/// clearMissingExecutables: drops executable paths whose file is gone from storage that is present.
fn clear_missing_executables(store: &Store, games: &mut [(String, Value)], libraries: &[PathBuf]) -> Result<(), String> {
    for (key, game) in games.iter_mut() {
        if matches!(game["shop"].as_str(), Some("custom" | "launchbox")) || game["isDeleted"].as_bool() == Some(true) {
            continue;
        }
        let Some(executable) = game["executablePath"].as_str().filter(|p| !p.is_empty()).map(str::to_string) else { continue };
        if !executable_gone(&executable, libraries) {
            continue;
        }
        let updated = crate::games::update(store, key, |g| {
            if g.get("executablePath").and_then(Value::as_str) == Some(&executable) {
                for field in ["executablePath", "executablePathUpdatedAt", "installedSizeInBytes"] {
                    g.insert(field.into(), Value::Null);
                }
            }
        })?;
        if let Some(updated) = updated.filter(|u| u["executablePath"].is_null()) {
            eprintln!("[scan] cleared missing executable for {key}: {executable}");
            *game = updated;
        }
    }
    Ok(())
}

// ---- Scanning ----

pub struct Scanned {
    dir: PathBuf,
    files: Vec<String>,
    by_name: BTreeMap<String, Vec<String>>,
}

fn walk(root: &Path, current: &Path, cancel: &AtomicBool, out: &mut Vec<String>) {
    if cancel.load(Ordering::Relaxed) {
        return;
    }
    let entries = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(e) => return eprintln!("[scan] error reading folder {}: {e}", current.display()),
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        let path = entry.path();
        if kind.is_dir() {
            if !is_redist_dir(&entry.file_name().to_string_lossy()) {
                walk(root, &path, cancel, out);
            }
        } else if kind.is_file() {
            let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned();
            if !is_redist_path(&relative) {
                out.push(relative);
            }
        }
    }
}

fn scan_directory(dir: &Path, names: &HashSet<String>, cancel: &AtomicBool) -> Scanned {
    let mut files = Vec::new();
    walk(dir, dir, cancel, &mut files);
    let mut by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for f in &files {
        if let Some(name) = basename(f).filter(|n| names.contains(n)) {
            by_name.entry(name).or_default().push(f.clone());
        }
    }
    Scanned { dir: dir.to_path_buf(), files, by_name }
}

/// resolveScanRoots: real paths, deduplicated, dropping any folder inside another.
fn scan_roots(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut resolved: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        if let Some(real) = real_path(dir).filter(|r| !resolved.contains(r)) {
            resolved.push(real);
        }
    }
    resolved.iter().filter(|d| !resolved.iter().any(|o| *d != o && d.starts_with(o))).cloned().collect()
}

fn resolve_within(executables: &[Known], scanned: &Scanned) -> Option<String> {
    let mut candidates: Vec<&String> = executables.iter().filter_map(|e| scanned.by_name.get(&e.exe)).flatten().collect();
    candidates.sort();
    candidates.dedup();
    let relative = match candidates.as_slice() {
        [] => return None,
        [only] => only.to_string(),
        _ => rank_across_installations(&scanned.files, executables)?,
    };
    Some(scanned.dir.join(relative).to_string_lossy().into_owned())
}

fn resolve_executable(executables: &[Known], scanned: &[Scanned]) -> Option<String> {
    scanned.iter().find_map(|s| resolve_within(executables, s))
}

fn normalize(p: &str) -> String {
    p.replace('\\', "/").to_lowercase()
}

fn comparable_segments(p: &str) -> Vec<String> {
    segments(p).into_iter().map(|s| s.chars().filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit()).collect::<String>()).filter(|s| !s.is_empty()).collect()
}

fn ends_with_known_folder(path: &str, executable_name: &str) -> bool {
    let (p, n) = (comparable_segments(path), comparable_segments(executable_name));
    n.len() >= 2 && ends_with(&p, &n)
}

fn names_the_folder(path: &str, title: &str) -> bool {
    let wanted: String = title.to_lowercase().chars().filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit()).collect();
    let segs = comparable_segments(path);
    !wanted.is_empty() && segs[..segs.len().saturating_sub(1)].contains(&wanted)
}

fn installation_folder_of(normalized: &str, roots: &[String]) -> Option<String> {
    let root = roots.iter().find(|r| normalized.starts_with(&format!("{r}/")))?;
    let folder = normalized[root.len() + 1..].split('/').next().filter(|f| !f.is_empty())?;
    Some(format!("{root}/{folder}"))
}

/// The scan state handed between the blocking file work and the async asset lookups.
struct Found {
    path_by_id: Vec<(String, String)>,
    ambiguous: Vec<(String, Vec<Choice>)>,
}

impl Found {
    fn has(&self, id: &str) -> bool {
        self.path_by_id.iter().any(|(i, _)| i == id)
    }
}

/// (executable path, candidate objectIds) pairs still waiting for a decision.
type Candidates = Vec<(String, Vec<String>)>;

/// partitionMatches: paths with one owner become additions, the rest wait for a title lookup.
fn partition(catalogue: &Catalogue, scanned: &[Scanned], library: &HashSet<String>, claimed: &HashSet<String>, cancel: &AtomicBool) -> (Vec<(String, String)>, Candidates) {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (id, executables) in catalogue.iter() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Some(path) = resolve_executable(executables, scanned) else { continue };
        match index.get(&path) {
            Some(&i) => groups[i].1.push(id.clone()),
            None => {
                index.insert(path.clone(), groups.len());
                groups.push((path, vec![id.clone()]));
            }
        }
    }
    let mut path_by_id = Vec::new();
    let mut undecided = Vec::new();
    for (path, ids) in groups {
        if claimed.contains(&normalize(&path)) {
            eprintln!("[scan] skipping {path}, another game already uses it");
            continue;
        }
        let owner = if ids.len() == 1 {
            Some(ids[0].clone())
        } else {
            let claiming: Vec<&String> = ids.iter().filter(|id| catalogue.get(id).is_some_and(|ex| ex.iter().any(|e| ends_with_known_folder(&path, &e.name)))).collect();
            (claiming.len() == 1).then(|| claiming[0].clone())
        };
        match owner {
            Some(id) if library.contains(&id) => eprintln!("[scan] skipping {path}, {id} is already in the library"),
            Some(id) => path_by_id.push((id, path)),
            None => undecided.push((path, ids)),
        }
    }
    (path_by_id, undecided)
}

/// collectUnnamedPaths: every known executable name found, with the games that ship it.
fn collect_unnamed(catalogue: &Catalogue, scanned: &[Scanned], library: &HashSet<String>, taken: &HashSet<String>) -> Candidates {
    let mut owners: HashMap<&str, Vec<String>> = HashMap::new();
    for (id, executables) in catalogue.iter() {
        let exes: HashSet<&str> = executables.iter().map(|e| e.exe.as_str()).collect();
        for exe in exes {
            owners.entry(exe).or_default().push(id.clone());
        }
    }
    let mut unnamed = Vec::new();
    let mut skipped = Vec::new();
    for s in scanned {
        for (name, relatives) in &s.by_name {
            let ids: Vec<String> = owners.get(name.as_str()).into_iter().flatten().filter(|id| !library.contains(*id)).cloned().collect();
            if ids.is_empty() {
                continue;
            }
            if ids.len() > UNNAMED_PATH_CANDIDATE_LIMIT {
                skipped.push(name.as_str());
                continue;
            }
            for relative in relatives {
                let path = s.dir.join(relative).to_string_lossy().into_owned();
                if !taken.contains(&normalize(&path)) {
                    unnamed.push((path, ids.clone()));
                }
            }
        }
    }
    if !skipped.is_empty() {
        eprintln!("[scan] not naming {} executable names claimed by more than {UNNAMED_PATH_CANDIDATE_LIMIT} games: {}", skipped.len(), skipped.join(", "));
    }
    unnamed
}

#[derive(Clone)]
struct Choice {
    object_id: String,
    title: String,
    icon_url: Value,
}

#[derive(Clone)]
enum Lookup {
    Found(Choice),
    Missing,
    Failed,
}

/// createChoiceResolver: one asset lookup per objectId, retried once, run in parallel batches.
struct Resolver {
    app: AppHandle,
    version: String,
    cache: HashMap<String, Lookup>,
}

impl Resolver {
    async fn prefetch(&mut self, ids: impl Iterator<Item = &String>) {
        let mut wanted: Vec<String> = ids.filter(|id| !self.cache.contains_key(*id)).cloned().collect();
        wanted.sort();
        wanted.dedup();
        for batch in wanted.chunks(ASSET_LOOKUP_CONCURRENCY) {
            let handles: Vec<_> = batch
                .iter()
                .map(|id| {
                    let (app, version, id) = (self.app.clone(), self.version.clone(), id.clone());
                    tauri::async_runtime::spawn(async move {
                        let store = app.state::<Store>();
                        let assets = match crate::library::get_game_assets(&store, &version, &id, SHOP, false).await {
                            Ok(a) => Ok(a),
                            Err(_) => crate::library::get_game_assets(&store, &version, &id, SHOP, false).await,
                        };
                        let lookup = match assets {
                            Ok(a) => match a["title"].as_str().filter(|t| !t.is_empty()) {
                                Some(title) => Lookup::Found(Choice { object_id: id.clone(), title: title.to_string(), icon_url: a["iconUrl"].clone() }),
                                None => Lookup::Missing,
                            },
                            Err(e) => {
                                eprintln!("[scan] failed to fetch assets for {id}: {e}");
                                Lookup::Failed
                            }
                        };
                        (id, lookup)
                    })
                })
                .collect();
            for handle in handles {
                if let Ok((id, lookup)) = handle.await {
                    self.cache.insert(id, lookup);
                }
            }
        }
    }

    /// readLookups: the titled choices, or None when any lookup failed.
    fn choices(&self, ids: &[String]) -> Option<Vec<Choice>> {
        let mut choices = Vec::new();
        for id in ids {
            match self.cache.get(id).unwrap_or(&Lookup::Failed) {
                Lookup::Found(choice) => choices.push(choice.clone()),
                Lookup::Missing => {}
                Lookup::Failed => return None,
            }
        }
        Some(choices)
    }
}

/// findGamesOutsideLibrary's lookup stages: undecided paths, then folders named after a game.
async fn name_outside_library(resolver: &mut Resolver, found: &mut Found, undecided: Candidates, library: &HashSet<String>) {
    for chunk in undecided.chunks(CHUNK_SIZE) {
        resolver.prefetch(chunk.iter().flat_map(|(_, ids)| ids)).await;
        for (path, ids) in chunk {
            let Some(choices) = resolver.choices(ids) else {
                eprintln!("[scan] skipping {path}, some of its {} candidate lookups failed", ids.len());
                continue;
            };
            let external: Vec<Choice> = choices.iter().filter(|c| !library.contains(&c.object_id)).cloned().collect();
            if choices.is_empty() || external.is_empty() {
                continue;
            }
            if choices.len() == 1 {
                if !found.has(&external[0].object_id) {
                    found.path_by_id.push((external[0].object_id.clone(), path.clone()));
                }
            } else {
                found.ambiguous.push((path.clone(), external));
            }
        }
    }
}

async fn name_by_folder(resolver: &mut Resolver, found: &mut Found, unnamed: Candidates) {
    for chunk in unnamed.chunks(CHUNK_SIZE) {
        resolver.prefetch(chunk.iter().flat_map(|(_, ids)| ids)).await;
        for (path, ids) in chunk {
            let Some(named) = resolver.choices(ids) else { continue };
            let choices: Vec<Choice> = named.into_iter().filter(|c| names_the_folder(path, &c.title)).collect();
            match choices.len() {
                0 => {}
                1 if found.has(&choices[0].object_id) => {}
                1 => found.path_by_id.push((choices[0].object_id.clone(), path.clone())),
                _ => found.ambiguous.push((path.clone(), choices)),
            }
        }
    }
}

/// addGameOutsideLibrary: adds (or revives) the Steam record and links its executable.
pub async fn add_game_outside_library(app: &AppHandle, version: &str, object_id: &str, executable: &str) -> Result<Option<Value>, String> {
    let store = app.state::<Store>();
    let assets = crate::library::get_game_assets(&store, version, object_id, SHOP, false).await.map_err(|e| format!("assets for {object_id}: {e}"))?;
    let Some(title) = assets["title"].as_str().filter(|t| !t.is_empty()) else {
        eprintln!("[scan] not adding {object_id}, no title for {executable}");
        return Ok(None);
    };
    let key = game_key(SHOP, object_id);
    let existing = store.get("games", &key)?;
    let kept = |field: &str, default: Value| if existing[field].is_null() { default } else { existing[field].clone() };
    let mut game = if existing.is_object() { existing.clone() } else { json!({}) };
    let fields = [
        ("title", kept("title", json!(title))),
        ("iconUrl", assets["iconUrl"].clone()),
        ("libraryHeroImageUrl", assets["libraryHeroImageUrl"].clone()),
        ("logoImageUrl", assets["logoImageUrl"].clone()),
        ("objectId", json!(object_id)),
        ("shop", json!(SHOP)),
        ("remoteId", kept("remoteId", Value::Null)),
        ("isDeleted", json!(false)),
        ("playTimeInMilliseconds", kept("playTimeInMilliseconds", json!(0))),
        ("lastTimePlayed", kept("lastTimePlayed", Value::Null)),
        ("addedToLibraryAt", kept("addedToLibraryAt", json!(iso_now()))),
        ("platform", kept("platform", Value::Null)),
    ];
    for (field, value) in fields {
        game[field] = value;
    }
    let game = with_executable_path(game, Some(executable));
    store.put("games", &key, &game)?;
    // shortcut: no achievement first-sync call; the achievement watcher picks the game up on its next pass.
    eprintln!("[scan] added {object_id} to the library: {executable}");
    Ok(Some(json!({ "title": game["title"], "executablePath": executable, "iconUrl": game["iconUrl"] })))
}

async fn add_games(app: &AppHandle, version: &str, picks: &[(String, String)]) -> Vec<Value> {
    let mut added = Vec::new();
    for (object_id, executable) in picks {
        match add_game_outside_library(app, version, object_id, executable).await {
            Ok(Some(game)) => added.push(game),
            Ok(None) => {}
            Err(e) => eprintln!("[scan] failed to add {object_id} to the library: {e}"),
        }
    }
    added
}

static INFLIGHT: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();

fn inflight() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    INFLIGHT.get_or_init(Default::default)
}

/// `cancelScanInstalledGames`.
pub fn cancel(request_id: &str) {
    if let Some(signal) = inflight().lock().unwrap().get(request_id) {
        signal.store(true, Ordering::Relaxed);
        eprintln!("[scan] cancelling scan {request_id}");
    }
}

/// What the blocking file pass found, before any asset lookup.
struct FilePass {
    scanned: Vec<Scanned>,
    linked: Vec<Value>,
    total: usize,
    library: HashSet<String>,
    claimed: HashSet<String>,
    steam_to_add: Vec<(String, String)>,
    path_by_id: Vec<(String, String)>,
    undecided: Candidates,
}

fn file_pass(app: &AppHandle, catalogue: &Catalogue, extra: Vec<PathBuf>, include_defaults: bool, add_to_library: bool, cancel: &AtomicBool) -> Result<FilePass, String> {
    let store = app.state::<Store>();
    let libraries = crate::steam_shortcuts::steam_path().map(|s| library_folders(&s)).unwrap_or_default();
    let names: HashSet<String> = catalogue.iter().flat_map(|(_, ex)| ex.iter().map(|e| e.exe.clone())).collect();

    // shortcut: Steam's appinfo.vdf is not read, so an installed app's executable comes from the
    // catalogue ranked inside its install folder; port steam-app-info-core.ts if that misses launch exes.
    let mut steam_executables: HashMap<String, String> = HashMap::new();
    let mut directories: Vec<PathBuf> = Vec::new();
    if include_defaults {
        directories.extend(WINDOWS_GAME_DIRECTORIES.iter().map(PathBuf::from));
        for (id, folder) in installed_apps(&libraries) {
            // Steam's shared redistributables (Steamworks Shared) are not games; appinfo's type check drops them in Electron.
            if folder.file_name().is_some_and(|n| is_redist_dir(&n.to_string_lossy())) {
                continue;
            }
            let pick = catalogue.get(&id).and_then(|executables| {
                let scanned = scan_directory(&folder, &names, cancel);
                rank_executable(&scanned.files, executables).map(|r| folder.join(r).to_string_lossy().into_owned())
            });
            match pick {
                Some(path) => {
                    steam_executables.insert(id, path);
                }
                None => directories.push(folder),
            }
        }
        eprintln!("[scan] resolved {} installed Steam games from the catalogue", steam_executables.len());
        let default = app.path().download_dir().map_err(|e| e.to_string())?;
        directories.push(crate::installer::downloads_path(&store, &default));
    }
    directories.extend(extra);

    let mut games: Vec<(String, Value)> = store.entries("games")?.into_iter().filter(|(_, g)| g["isDeleted"].as_bool() == Some(false) && g["shop"].as_str() != Some("custom")).collect();
    clear_missing_executables(&store, &mut games, &libraries)?;

    let mut scanned = Vec::new();
    for root in scan_roots(&directories) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let s = scan_directory(&root, &names, cancel);
        eprintln!("[scan] scanned {}: {} files, {} known executable names", s.dir.display(), s.files.len(), s.by_name.len());
        scanned.push(s);
    }

    let executable_of = |g: &Value| g["executablePath"].as_str().filter(|p| !p.is_empty()).map(normalize);
    let mut claimed: HashSet<String> = games.iter().filter_map(|(_, g)| executable_of(g)).collect();
    let to_link: Vec<&(String, Value)> = games.iter().filter(|(_, g)| executable_of(g).is_none()).collect();
    let mut linked = Vec::new();
    for (key, game) in &to_link {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let object_id = game["objectId"].as_str().unwrap_or_default();
        let steam_pick = steam_executables.get(object_id).filter(|p| game["shop"].as_str() == Some(SHOP) && !claimed.contains(&normalize(p))).cloned();
        let Some(path) = steam_pick.or_else(|| resolve_executable(catalogue.get(object_id)?, &scanned)) else { continue };
        store.put("games", key, &with_executable_path(game.clone(), Some(&path)))?;
        eprintln!("[scan] found executable for {object_id}: {path}");
        linked.push(json!({ "title": game["title"], "executablePath": path, "iconUrl": game["iconUrl"] }));
        claimed.insert(normalize(&path));
    }

    let mut library: HashSet<String> = games.iter().filter(|(_, g)| g["shop"].as_str() == Some(SHOP)).filter_map(|(_, g)| g["objectId"].as_str().map(str::to_string)).collect();
    let mut steam_to_add = Vec::new();
    if add_to_library {
        let mut sorted: Vec<_> = steam_executables.into_iter().collect();
        sorted.sort();
        for (id, path) in sorted {
            if library.contains(&id) || claimed.contains(&normalize(&path)) {
                continue;
            }
            library.insert(id.clone());
            claimed.insert(normalize(&path));
            steam_to_add.push((id, path));
        }
    }
    let (path_by_id, undecided) = if add_to_library && !cancel.load(Ordering::Relaxed) { partition(catalogue, &scanned, &library, &claimed, cancel) } else { Default::default() };
    Ok(FilePass { scanned, linked, total: to_link.len(), library, claimed, steam_to_add, path_by_id, undecided })
}

/// `scanInstalledGames(additionalDirectories, includeDefaultDirectories, addGamesToLibrary, requestId)`.
pub async fn scan(app: &AppHandle, extra: Vec<PathBuf>, include_defaults: bool, add_to_library: bool, request_id: Option<String>) -> Result<Value, String> {
    let signal = Arc::new(AtomicBool::new(false));
    if let Some(id) = &request_id {
        inflight().lock().unwrap().insert(id.clone(), signal.clone());
    }
    let result = run_scan(app, extra, include_defaults, add_to_library, signal).await;
    if let Some(id) = &request_id {
        inflight().lock().unwrap().remove(id);
    }
    result
}

async fn run_scan(app: &AppHandle, extra: Vec<PathBuf>, include_defaults: bool, add_to_library: bool, signal: Arc<AtomicBool>) -> Result<Value, String> {
    let version = app.package_info().version.to_string();
    let catalogue = catalogue(&app.state::<Store>(), &version).await.ok_or("game-executable-catalogue-unavailable")?;
    let pass = {
        let (app, catalogue, signal) = (app.clone(), catalogue.clone(), signal.clone());
        tauri::async_runtime::spawn_blocking(move || file_pass(&app, &catalogue, extra, include_defaults, add_to_library, &signal)).await.map_err(|e| e.to_string())??
    };

    let mut found = Found { path_by_id: pass.path_by_id, ambiguous: Vec::new() };
    if add_to_library && !signal.load(Ordering::Relaxed) {
        let mut resolver = Resolver { app: app.clone(), version: version.clone(), cache: HashMap::new() };
        name_outside_library(&mut resolver, &mut found, pass.undecided, &pass.library).await;
        let taken: HashSet<String> = pass.claimed.iter().cloned().chain(found.path_by_id.iter().map(|(_, p)| normalize(p))).chain(found.ambiguous.iter().map(|(p, _)| normalize(p))).collect();
        let unnamed = collect_unnamed(&catalogue, &pass.scanned, &pass.library, &taken);
        name_by_folder(&mut resolver, &mut found, unnamed).await;

        let roots: Vec<String> = pass.scanned.iter().map(|s| normalize(&s.dir.to_string_lossy())).collect();
        let occupied: HashSet<String> = pass.claimed.iter().cloned().chain(found.path_by_id.iter().map(|(_, p)| normalize(p))).filter_map(|p| installation_folder_of(&p, &roots)).collect();
        found.ambiguous.retain(|(path, _)| {
            let keep = installation_folder_of(&normalize(path), &roots).is_none_or(|f| !occupied.contains(&f));
            if !keep {
                eprintln!("[scan] not asking about {path}, its folder already holds a game");
            }
            keep
        });
        found.ambiguous.sort_by(|a, b| a.0.cmp(&b.0));
    }

    let picks: Vec<(String, String)> = pass.steam_to_add.into_iter().chain(found.path_by_id).collect();
    let added = if signal.load(Ordering::Relaxed) { Vec::new() } else { add_games(app, &version, &picks).await };
    eprintln!("[scan] linked {} of {} library games, added {} new ones, {} need a choice", pass.linked.len(), pass.total, added.len(), found.ambiguous.len());
    app.emit("on-library-batch-complete", ()).map_err(|e| e.to_string())?;
    // shortcut: the SCAN_GAMES_COMPLETE local notification is not created; the scan modal shows the
    // result, and local notifications move here with the notifications milestone.
    let ambiguous: Vec<Value> = found
        .ambiguous
        .iter()
        .map(|(path, choices)| {
            let choices: Vec<Value> = choices.iter().map(|c| json!({ "objectId": c.object_id, "title": c.title, "iconUrl": c.icon_url })).collect();
            json!({ "executablePath": path, "choices": choices })
        })
        .collect();
    Ok(json!({ "linkedGames": pass.linked, "addedGames": added, "ambiguousMatches": ambiguous, "total": pass.total }))
}

/// `addScannedGames(picks)`: the user's answers to the scan's ambiguous matches.
pub async fn add_scanned(app: &AppHandle, picks: &Value) -> Result<Value, String> {
    let version = app.package_info().version.to_string();
    let picks: Vec<(String, String)> = picks
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some((p["objectId"].as_str()?.to_string(), p["executablePath"].as_str()?.to_string())))
        .collect();
    let added = add_games(app, &version, &picks).await;
    if !added.is_empty() {
        app.emit("on-library-batch-complete", ()).map_err(|e| e.to_string())?;
    }
    Ok(json!(added))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(names: &[&str]) -> Vec<Known> {
        normalize_catalogue(&json!({ "1": names.iter().map(|n| json!({ "name": n, "os": "win32" })).collect::<Vec<_>>() }), false).map.remove("1").unwrap()
    }

    fn files(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn catalogue_keeps_runnable_executables_in_numeric_order() {
        let raw = json!({
            "100": [{ "name": ">Game/Bin/Game.exe", "os": "win32" }, { "name": "game.sh", "os": "linux" }, { "name": "mac", "os": "darwin" }],
            "20": [{ "name": "a.exe", "os": "win32" }],
            "3": [{ "name": "b", "os": "darwin" }],
        });
        let windows = normalize_catalogue(&raw, true);
        assert_eq!(windows.ids, ["20", "100"]);
        assert_eq!(windows.map["100"], [Known { exe: "game.exe".into(), name: r"game\bin\game.exe".into() }]);
        assert_eq!(normalize_catalogue(&raw, false).map["100"].len(), 2);
    }

    #[test]
    fn ranking_prefers_named_folder_then_original_copy() {
        let ex = known(&["game.exe", "bin/game.exe"]);
        assert_eq!(rank_executable(&files(&["game.exe", "bin/game.exe"]), &ex).as_deref(), Some("bin/game.exe"));
        let ex = known(&["game.exe"]);
        assert_eq!(rank_executable(&files(&["game.exe", "backup/game.exe", "vcredist/game.exe"]), &ex).as_deref(), Some("game.exe"));
        // Two equal copies in sibling folders tie, so nothing is picked.
        assert_eq!(rank_executable(&files(&["a/game.exe", "b/game.exe"]), &ex), None);
    }

    #[test]
    fn library_ranking_needs_one_installation() {
        let ex = known(&["game.exe"]);
        assert_eq!(rank_across_installations(&files(&["Game/game.exe", "Game/readme.txt"]), &ex).as_deref(), Some("Game/game.exe"));
        assert_eq!(rank_across_installations(&files(&["One/game.exe", "Two/game.exe"]), &ex), None);
    }

    #[test]
    fn steam_manifests_and_library_folders_parse() {
        let vdf = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\SteamLibrary\"\n\t\t\"apps\"\n\t\t{\n\t\t\t\"228980\"\t\t\"123\"\n\t\t}\n\t}\n\t\"1\"\t\"/home/me/steam\"\n}";
        let folders = parse_library_folders(vdf);
        let expected: Vec<PathBuf> = if cfg!(windows) { vec![PathBuf::from(r"D:\SteamLibrary")] } else { vec![PathBuf::from("/home/me/steam")] };
        assert_eq!(folders, expected);
        let acf = "\"AppState\"\n{\n\t\"appid\"\t\t\"42\"\n\t\"name\"\t\t\"A \\\"Quoted\\\" Game\"\n\t\"StateFlags\"\t\t\"4\"\n}";
        assert_eq!(manifest_value(acf, "AppID").as_deref(), Some("42"));
        assert_eq!(manifest_value(acf, "name").as_deref(), Some("A \"Quoted\" Game"));
        assert_eq!(manifest_value(acf, "installdir"), None);
    }

    #[test]
    fn folder_naming_and_installation_folders() {
        assert!(names_the_folder(r"D:\Games\Hollow Knight\hollow_knight.exe", "Hollow Knight"));
        assert!(!names_the_folder(r"D:\Games\Other\hollow_knight.exe", "Hollow Knight"));
        assert!(ends_with_known_folder(r"D:\Games\X\Bin64\Game.exe", r"bin64\game.exe"));
        let roots = vec!["d:/games".to_string()];
        assert_eq!(installation_folder_of("d:/games/hk/hk.exe", &roots).as_deref(), Some("d:/games/hk"));
        assert_eq!(installation_folder_of("e:/hk.exe", &roots), None);
    }

    #[test]
    fn scan_skips_redistributables() {
        let dir = std::env::temp_dir().join(format!("chimaera-scan-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(dir.join("Game/_CommonRedist")).unwrap();
        fs::write(dir.join("Game/game.exe"), b"").unwrap();
        fs::write(dir.join("Game/_CommonRedist/game.exe"), b"").unwrap();
        fs::write(dir.join("Game/UnityCrashHandler64.exe"), b"").unwrap();
        let names: HashSet<String> = ["game.exe".to_string()].into();
        let scanned = scan_directory(&dir, &names, &AtomicBool::new(false));
        assert_eq!(scanned.files.len(), 1);
        assert_eq!(resolve_within(&known(&["game.exe"]), &scanned), Some(dir.join("Game").join("game.exe").to_string_lossy().into_owned()));
        fs::remove_dir_all(dir).unwrap();
    }
}
