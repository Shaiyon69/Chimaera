//! Classics (LaunchBox) ROM import and the ROM folder channels. Port of
//! `events/emulators/import-launchbox-roms.ts`, the ROM folder handlers in
//! `events/emulators/`, `list-emulator-roms.ts`, `helpers/platform-to-system.ts`
//! and the RPCS3 games.yml half of `services/emulators/emulator-config.ts`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use regex::Regex;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::emulators;
use crate::games::game_key;
use crate::library::{iso_now, now_ms};
use crate::rom_scan;
use crate::store::Store;

pub struct Folder {
    pub path: String,
    pub scan_subfolders: bool,
}

pub fn folders_arg(value: &Value) -> Vec<Folder> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| Some(Folder { path: f["path"].as_str()?.to_string(), scan_subfolders: f["scanSubfolders"].as_bool().unwrap_or(false) }))
        .collect()
}

/// In-flight imports by request id, plus the snapshot `getActiveClassicsImport` answers.
#[derive(Default)]
pub struct ClassicsImports {
    inflight: Mutex<HashMap<String, Arc<AtomicBool>>>,
    active: Mutex<Value>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl ClassicsImports {
    pub fn active(&self) -> Value {
        lock(&self.active).clone()
    }

    pub fn is_importing(&self) -> bool {
        !lock(&self.active).is_null()
    }

    pub fn cancel(&self, request_id: &str) {
        if let Some(cancel) = lock(&self.inflight).get(request_id) {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

static PLATFORM_RULES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"playstation\s*portable|\bpsp\b", "psp"),
        (r"playstation\s*3|\bps3\b", "ps3"),
        (r"playstation\s*2|\bps2\b", "ps2"),
        (r"playstation|\bps1\b|\bpsx\b", "ps1"),
        (r"game\s*cube", "dolphin"),
    ]
    .into_iter()
    .map(|(re, system)| (Regex::new(re).unwrap(), system))
    .collect()
});

/// platformToSystem: which emulator a catalogue platform name runs on.
pub fn platform_to_system(platform: &str) -> Option<&'static str> {
    let p = platform.to_lowercase();
    if let Some((_, system)) = PLATFORM_RULES.iter().find(|(re, _)| re.is_match(&p)) {
        return Some(system);
    }
    let trimmed = p.trim();
    (trimmed == "wii" || trimmed.strip_prefix("nintendo").is_some_and(|rest| rest.starts_with(char::is_whitespace) && rest.trim() == "wii")).then_some("dolphin")
}

fn normalize_path(p: &str) -> String {
    let p = if cfg!(windows) { p.replace('/', "\\").to_lowercase() } else { p.to_string() };
    let sep = std::path::MAIN_SEPARATOR;
    let trimmed = p.trim_end_matches(sep);
    if trimmed.is_empty() { p } else { trimmed.to_string() }
}

/// rom-path-utils `isWithin`: the same path or anything under it.
pub fn is_within(child: &str, parent: &str) -> bool {
    let (c, p) = (normalize_path(child), normalize_path(parent));
    c == p || c.strip_prefix(&p).is_some_and(|rest| rest.starts_with(std::path::MAIN_SEPARATOR))
}

pub fn recompute_totals(config: &mut Value) {
    let folders = config["romFolders"].as_array().cloned().unwrap_or_default();
    config["totalFiles"] = json!(folders.iter().map(|f| f["fileCount"].as_u64().unwrap_or(0)).sum::<u64>());
    config["totalSizeBytes"] = json!(folders.iter().map(|f| f["sizeBytes"].as_u64().unwrap_or(0)).sum::<u64>());
    config["lastScanAt"] = json!(folders.iter().filter_map(|f| f["lastScanAt"].as_u64()).max());
}

fn folders_mut(config: &mut Value) -> &mut Vec<Value> {
    if !config["romFolders"].is_array() {
        config["romFolders"] = json!([]);
    }
    config["romFolders"].as_array_mut().unwrap()
}

/// ensureRomFolderRegistered: adds a new folder with empty counts; a known one keeps its counts.
pub fn ensure_registered(store: &Store, system: &str, path: &str, scan_subfolders: bool) -> Result<Value, String> {
    emulators::update_config(store, system, |config| {
        let folders = folders_mut(config);
        if folders.iter().any(|f| f["path"].as_str() == Some(path)) {
            return;
        }
        folders.push(json!({ "id": uuid::Uuid::new_v4().to_string(), "path": path, "scanSubfolders": scan_subfolders, "fileCount": 0, "sizeBytes": 0, "lastScanAt": null }));
        recompute_totals(config);
    })
}

fn persist_rom_folder(store: &Store, system: &str, folder: &Folder, file_count: u64, size_bytes: u64) -> Result<Value, String> {
    emulators::update_config(store, system, |config| {
        let folders = folders_mut(config);
        let existing = folders.iter().position(|f| f["path"].as_str() == Some(&folder.path));
        let id = existing.and_then(|i| folders[i]["id"].as_str().map(str::to_string)).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let updated = json!({ "id": id, "path": folder.path, "scanSubfolders": folder.scan_subfolders, "fileCount": file_count, "sizeBytes": size_bytes, "lastScanAt": now_ms() });
        match existing {
            Some(i) => folders[i] = updated,
            None => folders.push(updated),
        }
        recompute_totals(config);
    })
}

pub fn remove_folder(store: &Store, system: &str, folder_id: &str) -> Result<Value, String> {
    emulators::update_config(store, system, |config| {
        folders_mut(config).retain(|f| f["id"].as_str() != Some(folder_id));
        recompute_totals(config);
    })
}

/// previewRomFolder: what a folder holds, without importing it.
pub fn preview_folder(system: &str, path: &str, scan_subfolders: bool) -> Result<Value, String> {
    let scan = rom_scan::scan_rom_folder(Path::new(path), system, scan_subfolders, &AtomicBool::new(false), &mut |_, _, _, _| {})?;
    Ok(json!({ "fileCount": scan.file_count, "sizeBytes": scan.size_bytes }))
}

/// toggleRomFolderSubfolders: rescans the folder's counts only; no catalogue match.
pub fn toggle_subfolders(store: &Store, system: &str, folder_id: &str, scan_subfolders: bool) -> Result<Value, String> {
    let current = emulators::get_config(store, system)?;
    let Some(path) = current["romFolders"].as_array().into_iter().flatten().find(|f| f["id"].as_str() == Some(folder_id)).and_then(|f| f["path"].as_str()) else {
        return Ok(current);
    };
    let scan = rom_scan::scan_rom_folder(Path::new(path), system, scan_subfolders, &AtomicBool::new(false), &mut |_, _, _, _| {})?;
    emulators::update_config(store, system, |config| {
        for folder in folders_mut(config).iter_mut().filter(|f| f["id"].as_str() == Some(folder_id)) {
            folder["scanSubfolders"] = json!(scan_subfolders);
            folder["fileCount"] = json!(scan.file_count);
            folder["sizeBytes"] = json!(scan.size_bytes);
            folder["lastScanAt"] = json!(now_ms());
        }
        recompute_totals(config);
    })
}

fn launchbox_games_in_folders<'a>(entries: &'a [(String, Value)], system: &'a str, folders: &'a [String]) -> impl Iterator<Item = &'a (String, Value)> + 'a {
    entries.iter().filter(move |(_, game)| {
        game["isDeleted"].as_bool() != Some(true)
            && game["shop"].as_str() == Some("launchbox")
            && game["platform"].as_str().and_then(platform_to_system) == Some(system)
            && discs(game).iter().any(|d| d["path"].as_str().is_some_and(|p| folders.iter().any(|f| is_within(p, f))))
    })
}

fn discs(game: &Value) -> Vec<Value> {
    game["discs"].as_array().cloned().unwrap_or_default()
}

/// listEmulatorRoms: the imported games whose discs sit in this system's ROM folders.
pub fn list_roms(store: &Store, system: &str) -> Result<Value, String> {
    let config = emulators::get_config(store, system)?;
    let folders: Vec<String> = config["romFolders"].as_array().into_iter().flatten().filter_map(|f| f["path"].as_str().map(str::to_string)).collect();
    if folders.is_empty() {
        return Ok(json!([]));
    }
    let entries = store.entries("games")?;
    let mut roms = Vec::new();
    for (key, game) in launchbox_games_in_folders(&entries, system, &folders) {
        let assets = crate::library::composed_assets_for(store, key)?;
        let skus: Vec<Value> = discs(game).iter().filter_map(|d| d["sku"].as_str().filter(|s| !s.is_empty()).map(Value::from)).collect();
        let or_null = |v: &Value| if v.is_null() { None } else { Some(v.clone()) };
        roms.push(json!({
            "objectId": game["objectId"],
            "title": game["title"],
            "coverImageUrl": assets["coverImageUrl"],
            "libraryImageUrl": assets["libraryImageUrl"],
            "iconUrl": or_null(&assets["iconUrl"]).or_else(|| or_null(&game["iconUrl"])),
            "customCoverImageUrl": game["customCoverImageUrl"],
            "customIconUrl": game["customIconUrl"],
            "sizeBytes": game["romSizeBytes"],
            "skus": skus,
        }));
    }
    roms.sort_by_key(|r| r["title"].as_str().unwrap_or_default().to_lowercase());
    Ok(Value::Array(roms))
}

// ---- RPCS3 games.yml --------------------------------------------------------

/// rpcs3GuiConfigsCandidates, reduced to the config roots (the parents of GuiConfigs/).
pub fn rpcs3_config_roots(executable: Option<&str>) -> Vec<PathBuf> {
    let home = PathBuf::from(std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap_or_default());
    let mut roots = Vec::new();
    if cfg!(windows) {
        let app_data = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| home.join("AppData").join("Roaming"));
        roots.push(app_data.join("rpcs3"));
    } else {
        roots.push(home.join(".config").join("rpcs3"));
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()) {
            roots.push(xdg.join("rpcs3"));
        }
        roots.push(home.join(".var/app/net.rpcs3.RPCS3/config/rpcs3"));
    }
    if let Some(dir) = executable.and_then(|e| Path::new(e).parent()) {
        roots.push(dir.to_path_buf());
    }
    roots
}

fn games_yml_candidates(executable: Option<&str>) -> Vec<PathBuf> {
    rpcs3_config_roots(executable).into_iter().map(|r| r.join("games.yml")).collect()
}

static TITLE_ID_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^[A-Z]{4}\d{5}$").unwrap());

/// readGamesYml: `TITLEID -> path` from the first games.yml that exists.
pub fn read_games_yml(executable: Option<&str>) -> Vec<(String, String)> {
    let Some(path) = games_yml_candidates(executable).into_iter().find(|p| p.exists()) else { return vec![] };
    let Ok(parsed) = std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|s| serde_yaml::from_str::<serde_yaml::Mapping>(&s).map_err(|e| e.to_string())) else {
        return vec![];
    };
    parsed
        .into_iter()
        .filter_map(|(k, v)| Some((k.as_str()?.to_string(), v.as_str()?.to_string())))
        .filter(|(k, _)| TITLE_ID_RE.is_match(k))
        .map(|(k, v)| (k.to_uppercase(), v))
        .collect()
}

fn trim_separators(p: &str) -> String {
    let normalized = if cfg!(windows) { p.replace('/', "\\") } else { p.to_string() };
    normalized.trim_end_matches(['\\', '/']).to_string()
}

fn base_name(p: &str) -> String {
    Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// buildPathToTitleIdIndex: by full path and by base name (flatpak portals keep only the name).
fn path_index(map: &[(String, String)]) -> HashMap<String, String> {
    let mut index = HashMap::new();
    for (id, raw) in map {
        let normalized = trim_separators(raw);
        index.insert(normalized.clone(), id.clone());
        let base = base_name(&normalized);
        if !base.is_empty() {
            index.entry(base).or_insert_with(|| id.clone());
        }
    }
    index
}

fn lookup_yml_sku(index: &HashMap<String, String>, primary: &str) -> Option<String> {
    let norm = trim_separators(primary);
    let parent = Path::new(&norm).parent().map(|p| base_name(&p.to_string_lossy())).unwrap_or_default();
    index.get(&norm).or_else(|| index.get(&base_name(&norm))).or_else(|| index.get(&parent)).cloned()
}

/// mergeWriteGamesYml: adds new title ids only, after a one-off `.hydra-bak` copy.
fn merge_write_games_yml(executable: Option<&str>, entries: &[(String, String)]) -> bool {
    let candidates = games_yml_candidates(executable);
    let Some(target) = candidates.iter().find(|p| p.exists()).or(candidates.first()).cloned() else { return false };
    if entries.is_empty() || !target.parent().is_some_and(Path::exists) {
        return false;
    }
    let mut existing = serde_yaml::Mapping::new();
    if let Ok(content) = std::fs::read_to_string(&target) {
        if let Ok(parsed) = serde_yaml::from_str::<serde_yaml::Mapping>(&content) {
            existing = parsed;
        }
        let _ = std::fs::write(target.with_extension("yml.hydra-bak"), &content);
    }
    let mut added = 0;
    for (id, path) in entries {
        let key = serde_yaml::Value::from(id.as_str());
        if !existing.contains_key(&key) {
            existing.insert(key, serde_yaml::Value::from(path.as_str()));
            added += 1;
        }
    }
    added > 0 && serde_yaml::to_string(&existing).ok().is_some_and(|s| std::fs::write(&target, s).is_ok())
}

/// getRpcs3DefaultSources: the `games/` folder and games.yml under the config root.
pub fn rpcs3_default_sources(store: &Store) -> Result<Value, String> {
    let config = emulators::get_config(store, "ps3")?;
    let exe = config["executablePath"].as_str();
    let roots = rpcs3_config_roots(exe);
    let first = |name: &str| roots.iter().map(|r| r.join(name)).find(|p| p.exists()).map(|p| p.to_string_lossy().into_owned());
    let entries: Vec<Value> = read_games_yml(exe).into_iter().map(|(id, path)| json!({ "titleId": id, "path": path })).collect();
    Ok(json!({ "gamesDir": first("games"), "gamesYmlPath": first("games.yml"), "gamesYmlEntries": entries }))
}

// ---- RecursivePaths in DuckStation / PCSX2 configs ----------------------------

pub(crate) fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap_or_default())
}

pub(crate) fn windows_documents(home: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![home.join("Documents")];
    for key in ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"] {
        if let Some(root) = std::env::var_os(key) {
            let docs = PathBuf::from(root).join("Documents");
            if !dirs.contains(&docs) {
                dirs.push(docs);
            }
        }
    }
    dirs
}

/// duckstationConfigCandidates / pcsx2ConfigCandidates.
pub(crate) fn ini_candidates(system: &str, executable: Option<&str>) -> Vec<PathBuf> {
    let home = home_dir();
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| home.join("AppData").join("Local"));
    let beside = executable.and_then(|e| Path::new(e).parent()).map(|d| d.join("inis").join("PCSX2.ini"));
    match system {
        "ps1" if cfg!(windows) => std::iter::once(local.join("DuckStation").join("settings.ini")).chain(windows_documents(&home).into_iter().map(|d| d.join("DuckStation").join("settings.ini"))).collect(),
        "ps1" => vec![home.join(".local/share/duckstation/settings.ini"), home.join(".var/app/org.duckstation.DuckStation/data/duckstation/settings.ini")],
        "ps2" if cfg!(windows) => windows_documents(&home).into_iter().map(|d| d.join("PCSX2").join("inis").join("PCSX2.ini")).chain([local.join("PCSX2").join("inis").join("PCSX2.ini")]).chain(beside).collect(),
        "ps2" => [home.join(".config/PCSX2/inis/PCSX2.ini"), home.join(".var/app/net.pcsx2.PCSX2/config/PCSX2/inis/PCSX2.ini")].into_iter().chain(beside).collect(),
        _ => vec![],
    }
}

static SECTION_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\[(.+?)\]\s*$").unwrap());
static RECURSIVE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\s*RecursivePaths\s*=\s*(.+?)\s*$").unwrap());

/// readRecursivePaths: the `[GameList] RecursivePaths` entries of the emulator's ini.
pub fn read_recursive_paths(store: &Store, system: &str) -> Result<Value, String> {
    let config = emulators::get_config(store, system)?;
    let Some(content) = ini_candidates(system, config["executablePath"].as_str()).into_iter().find(|p| p.exists()).and_then(|p| std::fs::read_to_string(p).ok()) else {
        return Ok(json!([]));
    };
    let (mut out, mut in_list) = (Vec::<String>::new(), false);
    for line in content.lines() {
        if let Some(section) = SECTION_RE.captures(line) {
            in_list = section[1].eq_ignore_ascii_case("gamelist");
        } else if let Some(m) = RECURSIVE_RE.captures(line).filter(|_| in_list) {
            if !out.iter().any(|p| p == &m[1]) {
                out.push(m[1].to_string());
            }
        }
    }
    Ok(json!(out))
}

/// The ini with `RecursivePaths = folder` added at the end of `[GameList]` (or a new section).
fn with_recursive_path(content: &str, folder: &str) -> String {
    let entry = format!("RecursivePaths = {folder}");
    let mut out: Vec<String> = Vec::new();
    let (mut in_list, mut done, mut last_in_list) = (false, false, None::<usize>);
    for line in content.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)) {
        if let Some(section) = SECTION_RE.captures(line) {
            if in_list && !done {
                out.insert(last_in_list.map_or(out.len(), |i| i + 1), entry.clone());
                done = true;
            }
            in_list = section[1].eq_ignore_ascii_case("gamelist");
            out.push(line.to_string());
            if in_list {
                last_in_list = Some(out.len() - 1);
            }
            continue;
        }
        if in_list {
            if RECURSIVE_RE.captures(line).is_some_and(|m| &m[1] == folder) {
                done = true;
            }
            last_in_list = Some(out.len());
        }
        out.push(line.to_string());
    }
    if in_list && !done {
        out.insert(last_in_list.map_or(out.len(), |i| i + 1), entry.clone());
        done = true;
    }
    if !done {
        if out.last().is_some_and(|l| !l.is_empty()) {
            out.push(String::new());
        }
        out.push("[GameList]".into());
        out.push(entry);
    }
    out.join("\n")
}

/// addRecursivePath: false when the emulator has no ini yet.
pub fn add_recursive_path(store: &Store, system: &str, folder: &str) -> Result<Value, String> {
    let config = emulators::get_config(store, system)?;
    let Some(path) = ini_candidates(system, config["executablePath"].as_str()).into_iter().find(|p| p.exists()) else { return Ok(json!(false)) };
    let Ok(content) = std::fs::read_to_string(&path) else { return Ok(json!(false)) };
    Ok(json!(std::fs::write(&path, with_recursive_path(&content, folder)).is_ok()))
}

// ---- Import -----------------------------------------------------------------

struct Scanned {
    folder: String,
    game: rom_scan::ScannedGame,
    sku: Option<String>,
}

static DISC_NUMBER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(?:disc|cd|disk)\s*(\d+)\b").unwrap());
static DISC_MARKERS: LazyLock<[Regex; 3]> = LazyLock::new(|| {
    [
        Regex::new(r"(?i)\s*[(\[][^()\]]*?(?:disc|cd|disk)\s*\d+[^()\]]*?[)\]]").unwrap(),
        Regex::new(r"(?i)\s*-\s*(?:disc|cd|disk)\s*\d+\b").unwrap(),
        Regex::new(r"(?i)\s+(?:disc|cd|disk)\s*\d+\b").unwrap(),
    ]
});
static SPACES_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

fn disc_number(name: &str) -> Option<u64> {
    DISC_NUMBER_RE.captures(name).and_then(|c| c[1].parse().ok())
}

pub(crate) fn without_ext(name: &str) -> &str {
    match name.rfind('.') {
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    }
}

/// stripDiscMarker: "Game (Disc 2).cue" and "Game - CD1.cue" both become "game".
fn strip_disc_marker(name: &str) -> String {
    let mut base = without_ext(name).to_string();
    for re in DISC_MARKERS.iter() {
        base = re.replace_all(&base, "").into_owned();
    }
    SPACES_RE.replace_all(&base, " ").trim().to_lowercase()
}

/// buildDiscList: numbered discs first, in order, then the rest by name.
fn disc_list(mut files: Vec<(String, String, Option<String>)>) -> Vec<Value> {
    files.sort_by(|(_, a, _), (_, b, _)| match (disc_number(a), disc_number(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.to_lowercase().cmp(&b.to_lowercase()),
    });
    let many = files.len() > 1;
    files
        .iter()
        .enumerate()
        .map(|(i, (path, name, sku))| {
            let detected = disc_number(name);
            let label = if detected.is_some() || many { format!("Disc {}", detected.unwrap_or(i as u64 + 1)) } else { without_ext(name).to_string() };
            json!({ "path": path, "label": label, "fileName": name, "sku": sku })
        })
        .collect()
}

fn default_platform(system: &str) -> Option<&'static str> {
    match system {
        "ps1" => Some("PlayStation"),
        "ps2" => Some("PlayStation 2"),
        "ps3" => Some("PlayStation 3"),
        "psp" => Some("PlayStation Portable"),
        _ => None,
    }
}

fn catalogue_platforms(system: &str) -> &'static [&'static str] {
    match system {
        "ps1" => &["Sony Playstation"],
        "ps2" => &["Sony Playstation 2"],
        "ps3" => &["Sony Playstation 3"],
        "psp" => &["Sony PSP"],
        _ => &["Nintendo GameCube", "Nintendo Wii"],
    }
}

fn entry_platform(entry: &Value) -> Option<&str> {
    entry["platform"].as_str().or_else(|| entry["data"]["platform"].as_str())
}

fn platform_matches(entry: &Value, system: &str) -> bool {
    let squash = |s: &str| SPACES_RE.replace_all(s.trim(), " ").to_lowercase();
    match entry_platform(entry).filter(|p| !p.is_empty()) {
        None => true,
        Some(raw) => catalogue_platforms(system).iter().any(|p| squash(p) == squash(raw)),
    }
}

pub(crate) fn band_percent(start: f64, span: f64, processed: usize, total: usize) -> f64 {
    let frac = if total > 0 { (processed as f64 / total as f64).min(1.0) } else { 0.0 };
    ((start + frac * span) * 10.0).round().min(1000.0) / 10.0
}

const SCAN_BAND: f64 = 15.0;
const EXTRACT_BAND: f64 = 70.0;

type Progress = Arc<dyn Fn(Value) + Send + Sync>;

#[allow(clippy::too_many_arguments)]
fn progress_payload(phase: &str, processed: usize, total: usize, percent: f64, current: Option<&str>, status: Option<&str>, discovered: usize, matched: usize, size_bytes: u64) -> Value {
    json!({
        "type": "progress", "phase": phase, "processed": processed, "total": total, "percent": percent,
        "currentFile": current, "status": status, "discovered": discovered, "matched": matched, "sizeBytes": size_bytes,
    })
}

/// scanFolders + resolveSkus: blocking disc reads.
fn scan_and_extract(system: &str, folders: &[Folder], ps3_index: Option<&HashMap<String, String>>, cancel: &AtomicBool, progress: &Option<Progress>) -> Result<(Vec<Scanned>, Vec<(String, String)>), String> {
    let emit = |p: Value| if let Some(cb) = progress { cb(p) };
    let mut totals = Vec::new();
    if folders.len() > 1 {
        for folder in folders {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            totals.push(rom_scan::count_rom_groups(Path::new(&folder.path), system, folder.scan_subfolders, cancel)?);
        }
    }
    let grand_total: usize = totals.iter().sum();
    let (mut base, mut kept) = (0, 0);
    let mut collected = Vec::new();
    for (i, folder) in folders.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let scan = rom_scan::scan_rom_folder(Path::new(&folder.path), system, folder.scan_subfolders, cancel, &mut |processed, total, current, k| {
            let total = if folders.len() > 1 { grand_total } else { total };
            emit(progress_payload("scanning", base + processed, total, band_percent(0.0, SCAN_BAND, base + processed, total), current, None, kept + k, 0, 0));
        })?;
        base += totals.get(i).copied().unwrap_or(scan.games.len());
        kept += scan.games.len();
        collected.extend(scan.games.into_iter().map(|game| Scanned { folder: folder.path.clone(), game, sku: None }));
    }
    if cancel.load(Ordering::Relaxed) {
        return Ok((collected, vec![]));
    }

    let mut extracted_for_yml = Vec::new();
    let total = collected.len();
    for (i, item) in collected.iter_mut().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if !item.game.wrong_platform {
            let primary = item.game.primary_path.to_string_lossy().into_owned();
            item.sku = ps3_index.and_then(|index| lookup_yml_sku(index, &primary));
            if item.sku.is_none() {
                item.sku = rom_scan::extract_disc_sku(&item.game.primary_path, system);
                if let (true, Some(sku)) = (system == "ps3", &item.sku) {
                    // games.yml points at the folder above PS3_GAME.
                    let value = if base_name(&primary).eq_ignore_ascii_case("PS3_GAME") { item.game.primary_path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or(primary.clone()) } else { primary.clone() };
                    extracted_for_yml.push((rom_scan::lookup_key(sku), value));
                }
            }
        }
        emit(progress_payload("matching", i + 1, total, band_percent(SCAN_BAND, EXTRACT_BAND, i + 1, total), Some(&item.game.name), None, total, 0, 0));
    }
    Ok((collected, extracted_for_yml))
}

/// fetchShopDetailsForSkus: entries keyed by the dashless SKU, 100 SKUs a request.
pub(crate) async fn fetch_shop_details(store: &Store, version: &str, skus: &[String], language: &str, cancel: &AtomicBool) -> HashMap<String, Value> {
    let mut lookup = HashMap::new();
    for chunk in skus.chunks(100) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let request = json!({
            "method": "post", "url": "/games/shop-details",
            "data": { "shop": "launchbox", "skus": chunk, "language": language },
            "options": { "needsAuth": false },
        });
        match crate::hydra_api::call(store, version, &request).await {
            Ok(Value::Array(entries)) => {
                for entry in entries.into_iter().filter(|e| e["objectId"].is_string() && e["data"].is_object()) {
                    let skus = if entry["skus"].is_array() { &entry["skus"] } else { &entry["matchedSkus"] };
                    for sku in skus.as_array().into_iter().flatten().filter_map(Value::as_str) {
                        lookup.insert(rom_scan::lookup_key(sku), entry.clone());
                    }
                }
            }
            Ok(_) => {}
            Err(error) => eprintln!("[classics-import] shop-details batch failed: {error}"),
        }
    }
    lookup
}

/// clearFinishedDownload: a finished download record would shadow the imported discs.
fn clear_finished_download(store: &Store, key: &str) -> Result<(), String> {
    let download = store.get("downloads", key)?;
    let status = download["status"].as_str().unwrap_or_default();
    let seeding = status == "seeding" || download["shouldSeed"].as_bool() == Some(true);
    let active = status == "active" || status == "extracting" || download["extracting"].as_bool() == Some(true);
    if status == "removed" || (!active && !seeding && status == "complete") {
        store.del("downloads", key)?;
    }
    Ok(())
}

/// persistEntryLocally: shop details, assets and the game record with its discs.
pub(crate) fn persist_entry(store: &Store, entry: &Value, language: &str, discs: Vec<Value>, default_platform: Option<&str>, rom_size: Option<u64>) -> Result<(), String> {
    let object_id = entry["objectId"].as_str().unwrap_or_default();
    let key = game_key("launchbox", object_id);
    let details = crate::shop_details::map_launchbox(object_id, &Value::Null, entry, &Value::Null, language);
    if let Err(error) = store.put("gameShopCache", &format!("{key}:{language}"), &details) {
        eprintln!("[classics-import] could not cache shop details: {error}");
    }
    let data = &entry["data"];
    let source = &data["assets"];
    let title = source["title"].as_str().or_else(|| data["title"].as_str()).unwrap_or_default();
    let assets = json!({
        "objectId": object_id, "shop": "launchbox", "title": title,
        "iconUrl": source["iconUrl"], "libraryHeroImageUrl": source["libraryHeroImageUrl"],
        "libraryImageUrl": source["libraryImageUrl"], "logoImageUrl": source["logoImageUrl"],
        "logoPosition": null, "coverImageUrl": null, "downloadSources": [], "updatedAt": now_ms(),
    });
    if let Err(error) = store.put("gameShopAssets", &key, &assets) {
        eprintln!("[classics-import] could not cache assets: {error}");
    }

    let platform = entry_platform(entry).or(default_platform);
    let first_disc = discs.first().map(|d| d["path"].clone()).unwrap_or(Value::Null);
    let mut game = store.get("games", &key)?;
    if game.is_object() {
        clear_finished_download(store, &key)?;
        game["isDeleted"] = json!(false);
        if game["addedToLibraryAt"].is_null() {
            game["addedToLibraryAt"] = json!(iso_now());
        }
        if let Some(p) = platform.filter(|_| game["platform"].as_str().is_none_or(str::is_empty)) {
            game["platform"] = json!(p);
        }
        let selected = game["selectedDiscPath"].as_str().map(str::to_string);
        if !selected.is_some_and(|s| discs.iter().any(|d| d["path"].as_str() == Some(&s))) {
            game["selectedDiscPath"] = first_disc;
        }
        game["discs"] = Value::Array(discs);
        if let Some(size) = rom_size {
            game["romSizeBytes"] = json!(size);
        } else if game["romSizeBytes"].is_null() {
            game["romSizeBytes"] = Value::Null;
        }
    } else {
        game = json!({
            "title": data["title"].as_str().unwrap_or(title), "iconUrl": assets["iconUrl"],
            "libraryHeroImageUrl": assets["libraryHeroImageUrl"], "logoImageUrl": assets["logoImageUrl"],
            "objectId": object_id, "shop": "launchbox", "remoteId": null, "isDeleted": false,
            "playTimeInMilliseconds": 0, "lastTimePlayed": null, "addedToLibraryAt": iso_now(),
            "platform": platform, "discs": discs, "selectedDiscPath": first_disc, "romSizeBytes": rom_size,
        });
    }
    store.put("games", &key, &game)
}

/// reconcileDeletedGames: imported games whose discs are all gone get hidden.
fn reconcile_deleted(store: &Store, system: &str, folders: &[Folder], cancel: &AtomicBool) -> Result<(), String> {
    let paths: Vec<String> = folders.iter().map(|f| f.path.clone()).collect();
    let entries = store.entries("games")?;
    for (key, game) in launchbox_games_in_folders(&entries, system, &paths) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if discs(game).iter().any(|d| d["path"].as_str().is_some_and(|p| Path::new(p).exists())) {
            continue;
        }
        let mut game = game.clone();
        game["isDeleted"] = json!(true);
        store.put("games", key, &game)?;
    }
    Ok(())
}

/// runLaunchboxImport: scan, read SKUs, match against Hydra's LaunchBox
/// catalogue, then write the games and the folder counts.
pub async fn run(app: &AppHandle, system: &str, folders: Vec<Folder>, language: &str, cancel: Arc<AtomicBool>, progress: Option<Progress>) -> Result<Value, String> {
    emulators::known(system)?;
    let store = app.state::<Store>();
    let result = |file_count: u64, size_bytes: u64, matched: usize, unmatched: &[Value], cancelled: bool| {
        json!({ "fileCount": file_count, "sizeBytes": size_bytes, "matched": matched, "unmatched": unmatched.len(), "unmatchedFiles": unmatched, "cancelled": cancelled })
    };
    let is_cancelled = || cancel.load(Ordering::Relaxed);

    // Registered up front: a cancelled scan still lists the folder the user picked.
    for folder in &folders {
        if is_cancelled() {
            return Ok(result(0, 0, 0, &[], true));
        }
        ensure_registered(&store, system, &folder.path, folder.scan_subfolders)?;
    }

    let ps3_exe = if system == "ps3" { emulators::get_config(&store, "ps3")?["executablePath"].as_str().map(str::to_string) } else { None };
    let (collected, extracted_for_yml, folders) = {
        let (system, cancel, progress, ps3_exe) = (system.to_string(), cancel.clone(), progress.clone(), ps3_exe.clone());
        tauri::async_runtime::spawn_blocking(move || {
            let index = (system == "ps3").then(|| path_index(&read_games_yml(ps3_exe.as_deref())));
            scan_and_extract(&system, &folders, index.as_ref(), &cancel, &progress).map(|(c, y)| (c, y, folders))
        })
        .await
        .map_err(|e| e.to_string())??
    };
    if is_cancelled() {
        return Ok(result(0, 0, 0, &[], true));
    }

    let mut unique: Vec<String> = Vec::new();
    for sku in collected.iter().filter_map(|c| c.sku.clone()).filter(|s| !s.is_empty()) {
        if !unique.contains(&sku) {
            unique.push(sku);
        }
    }
    let version = app.package_info().version.to_string();
    let lookup = fetch_shop_details(&store, &version, &unique, language, &cancel).await;
    if is_cancelled() {
        return Ok(result(0, 0, 0, &[], true));
    }

    // buildEnriched: the first matched disc of a group names the whole group.
    let total = collected.len();
    let enriched: Vec<(String, Option<&Value>, &'static str)> = collected
        .iter()
        .map(|c| {
            let group = if system == "ps3" { c.game.primary_path.to_string_lossy().into_owned() } else { format!("{}::{}", c.folder, strip_disc_marker(&c.game.name)) };
            if c.game.wrong_platform {
                return (group, None, "wrong_platform");
            }
            match c.sku.as_deref().and_then(|s| lookup.get(&rom_scan::lookup_key(s))) {
                Some(entry) if platform_matches(entry, system) => (group, Some(entry), "matched"),
                Some(_) => (group, None, "wrong_platform"),
                None => (group, None, "unmatched"),
            }
        })
        .collect();
    let mut canonical: HashMap<&str, &Value> = HashMap::new();
    for (group, entry, _) in &enriched {
        if let Some(entry) = entry {
            canonical.entry(group.as_str()).or_insert(entry);
        }
    }

    // aggregateMatches: per title, its folder, size and discs.
    let mut titles: Vec<String> = Vec::new();
    let mut title_entry: HashMap<String, &Value> = HashMap::new();
    let mut title_folder: HashMap<String, (String, u64)> = HashMap::new();
    let mut title_discs: HashMap<String, Vec<(String, String, Option<String>)>> = HashMap::new();
    let (mut unmatched, mut seen_unmatched, mut matched_size) = (Vec::new(), HashSet::new(), 0u64);
    for (i, (c, (group, _, reason))) in collected.iter().zip(&enriched).enumerate() {
        if is_cancelled() {
            break;
        }
        let status = match canonical.get(group.as_str()) {
            Some(entry) => {
                let id = entry["objectId"].as_str().unwrap_or_default().to_string();
                if !title_entry.contains_key(&id) {
                    titles.push(id.clone());
                    title_entry.insert(id.clone(), entry);
                }
                title_folder.entry(id.clone()).or_insert_with(|| (c.folder.clone(), 0)).1 += c.game.size_bytes;
                matched_size += c.game.size_bytes;
                let path = c.game.primary_path.to_string_lossy().into_owned();
                let discs = title_discs.entry(id).or_default();
                if !discs.iter().any(|(p, _, _)| *p == path) {
                    discs.push((path, c.game.name.clone(), c.sku.clone()));
                }
                "matched"
            }
            None => {
                if seen_unmatched.insert(group.clone()) {
                    unmatched.push(json!({ "name": c.game.name, "reason": if *reason == "matched" { "unmatched" } else { reason } }));
                }
                reason
            }
        };
        if let Some(cb) = &progress {
            let start = SCAN_BAND + EXTRACT_BAND;
            cb(progress_payload("matching", i + 1, total, band_percent(start, 100.0 - start, i + 1, total), Some(&c.game.name), Some(status), total, title_folder.len(), matched_size));
        }
    }
    let matched = titles.len();

    // rollupFolders: one file per matched title, in the folder of its first disc.
    let mut rollup: Vec<(&Folder, u64, u64)> = folders.iter().map(|f| (f, 0, 0)).collect();
    for (folder, size) in title_folder.values() {
        if let Some(bucket) = rollup.iter_mut().find(|(f, _, _)| f.path == *folder) {
            bucket.1 += 1;
            bucket.2 += size;
        }
    }
    let total_files: u64 = rollup.iter().map(|r| r.1).sum();
    let total_size: u64 = rollup.iter().map(|r| r.2).sum();
    if is_cancelled() {
        return Ok(result(total_files, total_size, matched, &unmatched, true));
    }

    for id in &titles {
        if is_cancelled() {
            return Ok(result(total_files, total_size, matched, &unmatched, true));
        }
        let discs = disc_list(title_discs.remove(id).unwrap_or_default());
        if let Err(error) = persist_entry(&store, title_entry[id], language, discs, default_platform(system), title_folder.get(id).map(|f| f.1)) {
            eprintln!("[classics-import] could not save {id}: {error}");
        }
    }
    for (folder, files, size) in &rollup {
        if is_cancelled() {
            return Ok(result(total_files, total_size, matched, &unmatched, true));
        }
        if let Err(error) = persist_rom_folder(&store, system, folder, *files, *size) {
            eprintln!("[classics-import] could not save folder {}: {error}", folder.path);
        }
    }
    reconcile_deleted(&store, system, &folders, &cancel)?;
    if is_cancelled() {
        return Ok(result(total_files, total_size, matched, &unmatched, true));
    }
    // shortcut: no /profile/games/batch push; the Tauri build has no Hydra login. Add it with library sync.
    if system == "ps3" && !extracted_for_yml.is_empty() {
        let (exe, entries) = (ps3_exe, extracted_for_yml);
        let _ = tauri::async_runtime::spawn_blocking(move || merge_write_games_yml(exe.as_deref(), &entries)).await;
    }
    Ok(result(total_files, total_size, matched, &unmatched, false))
}

const PROGRESS_CHANNEL: &str = "on-classics-import-progress";

/// importLaunchboxRoms: starts an import in the background and answers its request id.
pub fn start(app: &AppHandle, system: &str, folders: Vec<Folder>, language: &str) -> Value {
    let request_id = uuid::Uuid::new_v4().to_string();
    let cancel = Arc::new(AtomicBool::new(false));
    let imports = app.state::<ClassicsImports>();
    lock(&imports.inflight).insert(request_id.clone(), cancel.clone());
    *lock(&imports.active) = json!({
        "requestId": request_id, "system": system, "phase": "scanning", "processed": 0, "total": 0, "percent": 0,
        "currentFile": null, "status": null, "discovered": 0, "matched": 0, "sizeBytes": 0,
    });
    let _ = app.emit("on-classics-import-status", true);

    let (app, system, language, id) = (app.clone(), system.to_string(), language.to_string(), request_id.clone());
    tauri::async_runtime::spawn(async move {
        let progress: Progress = {
            let (app, system, id) = (app.clone(), system.clone(), id.clone());
            Arc::new(move |payload: Value| {
                let imports = app.state::<ClassicsImports>();
                if let (Value::Object(active), Value::Object(patch)) = (&mut *lock(&imports.active), &payload) {
                    active.extend(patch.iter().filter(|(k, _)| *k != "type").map(|(k, v)| (k.clone(), v.clone())));
                }
                let mut event = payload;
                event["requestId"] = json!(id);
                event["system"] = json!(system);
                let _ = app.emit(PROGRESS_CHANNEL, event);
            })
        };
        let outcome = run(&app, &system, folders, &language, cancel, Some(progress)).await;
        let event = match outcome {
            Ok(mut result) => {
                let cancelled = result["cancelled"].as_bool() == Some(true);
                result.as_object_mut().map(|r| r.remove("cancelled"));
                result["type"] = json!(if cancelled { "cancelled" } else { "done" });
                result
            }
            Err(message) => json!({ "type": "error", "message": message }),
        };
        let mut event = event;
        event["requestId"] = json!(id);
        event["system"] = json!(system);
        let _ = app.emit(PROGRESS_CHANNEL, event);
        let imports = app.state::<ClassicsImports>();
        lock(&imports.inflight).remove(&id);
        *lock(&imports.active) = Value::Null;
        let _ = app.emit("on-classics-import-status", false);
    });
    json!({ "requestId": request_id })
}

/// addRomFolder / rescanEmulator: a full import without progress events.
pub async fn import_and_config(app: &AppHandle, system: &str, folders: Vec<Folder>, language: &str) -> Result<Value, String> {
    if !folders.is_empty() {
        run(app, system, folders, language, Arc::new(AtomicBool::new(false)), None).await?;
    }
    emulators::get_config(&app.state::<Store>(), system)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_platforms_to_systems() {
        assert_eq!(platform_to_system("Sony PlayStation Portable"), Some("psp"));
        assert_eq!(platform_to_system("PlayStation 3"), Some("ps3"));
        assert_eq!(platform_to_system("Sony Playstation 2"), Some("ps2"));
        assert_eq!(platform_to_system("PSX"), Some("ps1"));
        assert_eq!(platform_to_system("Nintendo GameCube"), Some("dolphin"));
        assert_eq!(platform_to_system("Nintendo Wii"), Some("dolphin"));
        assert_eq!(platform_to_system("Nintendo Wii U"), None);
        assert_eq!(platform_to_system("Windows"), None);
    }

    #[test]
    fn strips_disc_markers_and_orders_discs() {
        assert_eq!(strip_disc_marker("Final Fantasy VII (USA) (Disc 2).cue"), "final fantasy vii (usa)");
        assert_eq!(strip_disc_marker("Metal Gear Solid - CD1.cue"), "metal gear solid");
        assert_eq!(strip_disc_marker("Xenogears Disc 1.chd"), "xenogears");
        assert_eq!(strip_disc_marker("Spyro [Disc 1 of 3].bin"), "spyro");
        let discs = disc_list(vec![("b".into(), "Game (Disc 2).cue".into(), None), ("a".into(), "Game (Disc 1).cue".into(), Some("SLUS-00001".into()))]);
        assert_eq!(discs[0]["path"], "a");
        assert_eq!(discs[0]["label"], "Disc 1");
        assert_eq!(discs[1]["label"], "Disc 2");
        assert_eq!(disc_list(vec![("x".into(), "Crash.chd".into(), None)])[0]["label"], "Crash");
    }

    #[test]
    fn checks_paths_and_platforms() {
        let sep = std::path::MAIN_SEPARATOR;
        let root = format!("{sep}roms{sep}ps1");
        assert!(is_within(&format!("{root}{sep}game.cue"), &root));
        assert!(is_within(&root, &format!("{root}{sep}")));
        assert!(!is_within(&format!("{root}x{sep}game.cue"), &root));
        assert!(platform_matches(&json!({ "platform": "Sony  PlayStation " }), "ps1"));
        assert!(!platform_matches(&json!({ "data": { "platform": "Sony Playstation 2" } }), "ps1"));
        assert!(platform_matches(&json!({ "data": {} }), "ps2"));
        assert_eq!(band_percent(15.0, 70.0, 1, 2), 50.0);
        assert_eq!(band_percent(0.0, 15.0, 0, 0), 0.0);
    }

    #[test]
    fn adds_recursive_paths_to_the_game_list() {
        let ini = "[Main]\nA = 1\n[GameList]\nRecursivePaths = C:/old\n[UI]\nB = 2";
        assert_eq!(with_recursive_path(ini, "D:/roms"), "[Main]\nA = 1\n[GameList]\nRecursivePaths = C:/old\nRecursivePaths = D:/roms\n[UI]\nB = 2");
        assert_eq!(with_recursive_path(ini, "C:/old"), ini);
        assert_eq!(with_recursive_path("[Main]\nA = 1", "D:/roms"), "[Main]\nA = 1\n\n[GameList]\nRecursivePaths = D:/roms");
        assert_eq!(with_recursive_path("[GameList]", "D:/roms"), "[GameList]\nRecursivePaths = D:/roms");
    }

    #[test]
    fn folder_totals_follow_their_folders() {
        let mut config = json!({ "romFolders": [
            { "fileCount": 2, "sizeBytes": 10, "lastScanAt": 5 },
            { "fileCount": 1, "sizeBytes": 4, "lastScanAt": null },
        ] });
        recompute_totals(&mut config);
        assert_eq!((config["totalFiles"].as_u64(), config["totalSizeBytes"].as_u64(), config["lastScanAt"].as_u64()), (Some(3), Some(14), Some(5)));
    }

    #[test]
    fn indexes_games_yml_by_path_and_name() {
        let sep = std::path::MAIN_SEPARATOR;
        let game = format!("{sep}games{sep}Demon's Souls");
        let index = path_index(&[("BLUS30443".into(), format!("{game}{sep}"))]);
        assert_eq!(lookup_yml_sku(&index, &game).as_deref(), Some("BLUS30443"));
        assert_eq!(lookup_yml_sku(&index, &format!("{game}{sep}PS3_GAME")).as_deref(), Some("BLUS30443"));
        assert_eq!(lookup_yml_sku(&index, &format!("{sep}elsewhere{sep}Demon's Souls")).as_deref(), Some("BLUS30443"));
        assert_eq!(lookup_yml_sku(&index, &format!("{sep}other")), None);
    }

    #[test]
    fn persists_imported_games_and_revives_them() {
        let store = Store::memory().unwrap();
        let entry = json!({ "objectId": "lb1", "platform": "Sony Playstation", "data": { "title": "Crash", "assets": { "iconUrl": "i.png" } } });
        let discs = disc_list(vec![("/roms/crash.cue".into(), "crash.cue".into(), Some("SCUS-94900".into()))]);
        persist_entry(&store, &entry, "en", discs.clone(), Some("PlayStation"), Some(700)).unwrap();
        let game = store.get("games", "launchbox:lb1").unwrap();
        assert_eq!((game["title"].as_str(), game["platform"].as_str(), game["selectedDiscPath"].as_str()), (Some("Crash"), Some("Sony Playstation"), Some("/roms/crash.cue")));
        assert_eq!(store.get("gameShopAssets", "launchbox:lb1").unwrap()["iconUrl"], "i.png");
        let mut hidden = game.clone();
        hidden["isDeleted"] = json!(true);
        store.put("games", "launchbox:lb1", &hidden).unwrap();
        store.put("downloads", "launchbox:lb1", &json!({ "status": "complete" })).unwrap();
        persist_entry(&store, &entry, "en", discs, None, None).unwrap();
        let game = store.get("games", "launchbox:lb1").unwrap();
        assert_eq!((game["isDeleted"].as_bool(), game["romSizeBytes"].as_u64()), (Some(false), Some(700)));
        assert!(store.get("downloads", "launchbox:lb1").unwrap().is_null());
    }
}
