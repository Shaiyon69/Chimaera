//! RetroArch: port of `services/retroarch/` (repository, detection, cores, ROM
//! hashing, archives, rom-match, installers) and the `events/retroarch/` handlers.
//! The config lives in the imported `retroarch` sublevel under `config`, in
//! Electron's shape, so `classics_launch` and both builds read the same record.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use regex::Regex;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_opener::OpenerExt;

use crate::classics_import::{band_percent, is_within, persist_entry, recompute_totals, without_ext, Folder};
use crate::classics_launch::retroarch_platform;
use crate::emulators::{self, RETROARCH};
use crate::library::now_ms;
use crate::store::Store;

pub const PLATFORMS: [&str; 6] = ["nes", "snes", "n64", "gb", "gbc", "gba"];
pub const CORES: [&str; 5] = ["fceumm", "snes9x", "mupen64plus_next", "gambatte", "mgba"];
const ARCHIVE_EXTENSIONS: [&str; 2] = [".zip", ".7z"];
// The supported cartridge platforms fit within this bound, including large hacks.
const MAX_ARCHIVED_ROM_BYTES: u64 = 128 * 1024 * 1024;
const MAX_COLLECTED_FILES: usize = 50_000;
const BUILDBOT: &str = "https://buildbot.libretro.com";

fn rom_extensions(platform: &str) -> &'static [&'static str] {
    match platform {
        "nes" => &[".nes", ".fds"],
        "snes" => &[".sfc", ".smc"],
        "n64" => &[".n64", ".z64", ".v64"],
        "gb" => &[".gb"],
        "gbc" => &[".gbc"],
        _ => &[".gba"],
    }
}

fn launchbox_name(platform: &str) -> &'static str {
    match platform {
        "nes" => "Nintendo Entertainment System",
        "snes" => "Super Nintendo Entertainment System",
        "n64" => "Nintendo 64",
        "gb" => "Nintendo Game Boy",
        "gbc" => "Nintendo Game Boy Color",
        _ => "Nintendo Game Boy Advance",
    }
}

fn extension_platform(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    PLATFORMS.into_iter().find(|p| rom_extensions(p).iter().any(|ext| lower.ends_with(ext)))
}

fn is_archive(path: &str) -> bool {
    let lower = path.to_lowercase();
    ARCHIVE_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
}

// ---- config ------------------------------------------------------------------

fn empty_core(name: &str) -> Value {
    json!({ "name": name, "installed": false, "version": null, "path": null, "installedAt": null })
}

fn empty_counts() -> Value {
    Value::Object(PLATFORMS.iter().map(|p| (p.to_string(), json!(0))).collect())
}

fn empty_config() -> Value {
    json!({
        "executablePath": null, "detectedVersion": null, "detectedAt": null, "coresDir": null,
        "cores": Value::Object(CORES.iter().map(|c| (c.to_string(), empty_core(c))).collect()),
        "romFolders": [], "perPlatformCounts": empty_counts(), "totalFiles": 0, "totalSizeBytes": 0, "lastScanAt": null,
    })
}

/// getRetroArchConfig (repository): the stored record over the empty one, cores and counts merged per key.
pub fn get_config(store: &Store) -> Result<Value, String> {
    let mut config = empty_config();
    let Value::Object(stored) = store.get("retroarch", "config")? else { return Ok(config) };
    for (key, value) in stored {
        match (key.as_str(), &value) {
            ("cores" | "perPlatformCounts", Value::Object(map)) => {
                for (k, v) in map {
                    config[key.as_str()][k.as_str()] = v.clone();
                }
            }
            _ => config[key.as_str()] = value,
        }
    }
    Ok(config)
}

pub fn update_config(store: &Store, patch: impl FnOnce(&mut Value)) -> Result<Value, String> {
    let mut config = get_config(store)?;
    patch(&mut config);
    store.put("retroarch", "config", &config)?;
    Ok(config)
}

fn core_missing(core: &Value) -> bool {
    core["installed"].as_bool() == Some(true) && !core["path"].as_str().is_some_and(|p| Path::new(p).exists())
}

/// getRetroArchConfig (handler): cores whose library file is gone read as not installed.
pub fn config_checked(store: &Store) -> Result<Value, String> {
    let config = get_config(store)?;
    if !CORES.iter().any(|c| core_missing(&config["cores"][c])) {
        return Ok(config);
    }
    update_config(store, |config| {
        for core in CORES {
            if core_missing(&config["cores"][core]) {
                config["cores"][core] = empty_core(core);
            }
        }
    })
}

fn managed_dir(app: &AppHandle) -> Result<PathBuf, String> {
    crate::save_backups::data_dir(app, "retroarch")
}

fn library_file_name(core: &str) -> String {
    let ext = if cfg!(windows) { "dll" } else if cfg!(target_os = "macos") { "dylib" } else { "so" };
    format!("{core}_libretro.{ext}")
}

/// buildCoresStateForDir: a library already recorded at the same path keeps its record.
fn cores_state_for_dir(dir: &Path, current: &Value) -> Value {
    let mut cores = current.clone();
    for core in CORES {
        let path = dir.join(library_file_name(core));
        let found = path.exists().then(|| path.to_string_lossy().into_owned());
        if found.is_some() && current[core]["path"].as_str() == found.as_deref() {
            continue;
        }
        cores[core] = json!({ "name": core, "installed": found.is_some(), "version": null, "path": found, "installedAt": found.as_ref().map(|_| now_ms()) });
    }
    cores
}

fn resolve_cores_dir(config: &Value, managed: &Path) -> PathBuf {
    config["coresDir"].as_str().map(PathBuf::from).unwrap_or_else(|| managed.join("cores"))
}

pub fn set_cores_dir(store: &Store, managed: &Path, dir: Option<&str>) -> Result<Value, String> {
    let dir = dir.filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).components().collect::<PathBuf>());
    update_config(store, |config| {
        config["coresDir"] = json!(dir.as_ref().map(|d| d.to_string_lossy()));
        config["cores"] = cores_state_for_dir(&resolve_cores_dir(config, managed), &config["cores"]);
    })
}

// ---- detection ---------------------------------------------------------------

fn home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default()
}

/// retroArchConfigRoots: next to the executable, then the per-user config folder.
fn config_roots(exe: &str) -> Vec<PathBuf> {
    if exe.contains("org.libretro.RetroArch") {
        return vec![home().join(".var/app/org.libretro.RetroArch/config/retroarch")];
    }
    let mut roots: Vec<PathBuf> = Path::new(exe).parent().map(Path::to_path_buf).into_iter().collect();
    if cfg!(windows) {
        roots.extend(std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("RetroArch")));
    } else {
        roots.push(home().join(".config/retroarch"));
    }
    roots
}

/// readLibretroDirectory: `libretro_directory` from retroarch.cfg, `:` meaning the cfg's folder.
fn libretro_directory(content: &str, cfg: &Path) -> Option<PathBuf> {
    let line = content.lines().find(|l| l.split_once('=').is_some_and(|(k, _)| k.trim() == "libretro_directory"))?;
    let raw = line.split_once('=')?.1.trim();
    let raw = raw.strip_prefix('"').and_then(|r| r.strip_suffix('"')).unwrap_or(raw);
    if raw.is_empty() {
        return None;
    }
    let base = cfg.parent().unwrap_or(Path::new(""));
    Some(if let Some(rest) = raw.strip_prefix(':') {
        base.join(rest.trim_start_matches(['/', '\\']))
    } else if let Some(rest) = raw.strip_prefix('~') {
        home().join(rest.trim_start_matches(['/', '\\']))
    } else if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        base.join(raw)
    })
}

/// detectRetroArchCoresDir.
fn detect_cores_dir(exe: &str) -> Option<PathBuf> {
    let roots = config_roots(exe);
    roots
        .iter()
        .flat_map(|root| [root.join("retroarch.cfg"), root.join("config").join("retroarch.cfg")])
        .find_map(|cfg| libretro_directory(&fs::read_to_string(&cfg).ok()?, &cfg).filter(|d| d.is_dir()))
        .or_else(|| roots.iter().map(|r| r.join("cores")).find(|d| d.is_dir()))
}

/// detectRetroArchWithFallback: the usual places, the downloads folders (their
/// `RetroArch/` included) and the managed `retroarch/emulator` folder.
fn detect(app: &AppHandle, resolve_version: bool) -> Result<Option<(PathBuf, Option<String>)>, String> {
    let default_downloads = app.path().download_dir().map_err(|e| e.to_string())?;
    let mut dirs = emulators::download_dirs(&app.state::<Store>(), &default_downloads);
    dirs.push(managed_dir(app)?.join("emulator"));
    Ok(emulators::detect(&RETROARCH, &dirs, resolve_version))
}

/// isValidEmulatorExecutable plus isLikelyRetroArchExecutable.
fn valid_retroarch(path: &Path) -> bool {
    emulators::is_valid_executable(path) && path.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().contains("retroarch"))
}

/// The detectRetroArch / setRetroArchExecutablePath patch: a fresh setup also picks up the cores folder.
fn apply_executable(config: &mut Value, managed: &Path, exe: Option<&Path>, version: Option<String>) {
    config["executablePath"] = json!(exe.map(|p| p.to_string_lossy()));
    config["detectedVersion"] = json!(version);
    config["detectedAt"] = if exe.is_some() { json!(now_ms()) } else { Value::Null };
    let has_installed = CORES.iter().any(|c| config["cores"][c]["installed"].as_bool() == Some(true));
    if let (Some(exe), true, false) = (exe, config["coresDir"].is_null(), has_installed) {
        if let Some(dir) = detect_cores_dir(&exe.to_string_lossy()) {
            config["coresDir"] = json!(dir.to_string_lossy());
            config["cores"] = cores_state_for_dir(&resolve_cores_dir(config, managed), &config["cores"]);
        }
    }
}

pub fn detect_and_save(app: &AppHandle) -> Result<Value, String> {
    let store = app.state::<Store>();
    let Some((exe, version)) = detect(app, true)? else { return get_config(&store) };
    let managed = managed_dir(app)?;
    update_config(&store, |c| apply_executable(c, &managed, Some(&exe), version))
}

/// setRetroArchExecutablePath: null clears it; a file that is not RetroArch answers null.
pub fn set_executable_path(app: &AppHandle, path: Option<&str>) -> Result<Value, String> {
    let path = path.filter(|p| !p.is_empty()).map(|p| PathBuf::from(p).components().collect::<PathBuf>());
    if path.as_deref().is_some_and(|p| !valid_retroarch(p)) {
        return Ok(Value::Null);
    }
    let version = path.as_deref().and_then(|p| emulators::version_of(p, &RETROARCH));
    let managed = managed_dir(app)?;
    update_config(&app.state::<Store>(), |c| apply_executable(c, &managed, path.as_deref(), version))
}

pub fn preview_executable(app: &AppHandle, path: Option<&str>) -> Result<Value, String> {
    let found = match path.filter(|p| !p.is_empty()) {
        Some(p) => Some(PathBuf::from(p).components().collect::<PathBuf>()).filter(|p| valid_retroarch(p)).map(|p| {
            let v = emulators::version_of(&p, &RETROARCH);
            (p, v)
        }),
        None => detect(app, true)?,
    };
    Ok(found.map(|(p, v)| json!({ "executablePath": p.to_string_lossy(), "detectedVersion": v })).unwrap_or(Value::Null))
}

pub fn remove(store: &Store) -> Result<Value, String> {
    update_config(store, |c| {
        for (key, value) in [
            ("executablePath", Value::Null), ("detectedVersion", Value::Null), ("detectedAt", Value::Null),
            ("romFolders", json!([])), ("perPlatformCounts", empty_counts()), ("lastScanAt", Value::Null),
            ("totalFiles", json!(0)), ("totalSizeBytes", json!(0)),
        ] {
            c[key] = value;
        }
    })
}

// ---- ROM folders -------------------------------------------------------------

fn folders_mut(config: &mut Value) -> &mut Vec<Value> {
    if !config["romFolders"].is_array() {
        config["romFolders"] = json!([]);
    }
    config["romFolders"].as_array_mut().unwrap()
}

fn folder_paths(config: &Value) -> Vec<String> {
    config["romFolders"].as_array().into_iter().flatten().filter_map(|f| f["path"].as_str().map(str::to_string)).collect()
}

fn new_folder(path: &str, scan_subfolders: bool) -> Value {
    json!({ "id": uuid::Uuid::new_v4().to_string(), "path": path, "scanSubfolders": scan_subfolders, "fileCount": 0, "sizeBytes": 0, "lastScanAt": null })
}

/// addRetroArchRomFolder (and ensureRomFolderRegistered, which also recomputes the totals).
pub fn add_folder(store: &Store, path: &str, scan_subfolders: bool) -> Result<Value, String> {
    update_config(store, |config| {
        let folders = folders_mut(config);
        if !folders.iter().any(|f| f["path"].as_str() == Some(path)) {
            folders.push(new_folder(path, scan_subfolders));
            recompute_totals(config);
        }
    })
}

pub fn toggle_subfolders(store: &Store, id: &str, scan_subfolders: bool) -> Result<Value, String> {
    update_config(store, |config| {
        for f in folders_mut(config).iter_mut().filter(|f| f["id"].as_str() == Some(id)) {
            f["scanSubfolders"] = json!(scan_subfolders);
        }
    })
}

pub fn change_folder(store: &Store, id: &str, new_path: &str) -> Result<Value, String> {
    let before = get_config(store)?;
    let previous = before["romFolders"].as_array().into_iter().flatten().find(|f| f["id"].as_str() == Some(id)).and_then(|f| f["path"].as_str()).map(str::to_string);
    let next = update_config(store, |config| {
        let folders = folders_mut(config);
        if folders.iter().any(|f| f["path"].as_str() == Some(new_path) && f["id"].as_str() != Some(id)) {
            return;
        }
        for f in folders.iter_mut().filter(|f| f["id"].as_str() == Some(id)) {
            for (key, value) in [("path", json!(new_path)), ("fileCount", json!(0)), ("sizeBytes", json!(0)), ("lastScanAt", Value::Null)] {
                f[key] = value;
            }
        }
        recompute_totals(config);
    })?;
    let changed = next["romFolders"].as_array().into_iter().flatten().any(|f| f["id"].as_str() == Some(id) && f["path"].as_str() == Some(new_path));
    match previous.filter(|p| changed && p != new_path) {
        Some(previous) => {
            reconcile_removed_folder(store, &previous, &folder_paths(&next))?;
            recompute_counts(store)
        }
        None => Ok(next),
    }
}

pub fn remove_folder(store: &Store, id: &str) -> Result<Value, String> {
    let before = get_config(store)?;
    let removed = before["romFolders"].as_array().into_iter().flatten().find(|f| f["id"].as_str() == Some(id)).and_then(|f| f["path"].as_str()).map(str::to_string);
    let next = update_config(store, |config| {
        folders_mut(config).retain(|f| f["id"].as_str() != Some(id));
        recompute_totals(config);
    })?;
    if let Some(removed) = removed {
        reconcile_removed_folder(store, &removed, &folder_paths(&next))?;
    }
    recompute_counts(store)
}

fn discs(game: &Value) -> Vec<Value> {
    game["discs"].as_array().cloned().unwrap_or_default()
}

fn disc_path(disc: &Value) -> &str {
    disc["path"].as_str().unwrap_or_default()
}

/// Imported RetroArch games with a disc inside one of `folders`, with their platform.
fn games_in_folders<'a>(entries: &'a [(String, Value)], folders: &'a [String]) -> impl Iterator<Item = (&'a String, &'a Value, &'static str)> + 'a {
    entries.iter().filter_map(move |(key, game)| {
        if game["isDeleted"].as_bool() == Some(true) || game["shop"].as_str() != Some("launchbox") {
            return None;
        }
        let platform = retroarch_platform(game["platform"].as_str()?)?;
        discs(game).iter().any(|d| folders.iter().any(|f| is_within(disc_path(d), f))).then_some((key, game, platform))
    })
}

/// recomputeRetroArchPlatformCounts.
fn recompute_counts(store: &Store) -> Result<Value, String> {
    let folders = folder_paths(&get_config(store)?);
    let mut counts = empty_counts();
    if !folders.is_empty() {
        let entries = store.entries("games")?;
        for (_, _, platform) in games_in_folders(&entries, &folders) {
            counts[platform] = json!(counts[platform].as_u64().unwrap_or(0) + 1);
        }
    }
    update_config(store, |c| c["perPlatformCounts"] = counts)
}

/// listRetroArchRoms.
pub fn list_roms(store: &Store) -> Result<Value, String> {
    let folders = folder_paths(&get_config(store)?);
    if folders.is_empty() {
        return Ok(json!([]));
    }
    let entries = store.entries("games")?;
    let mut roms = Vec::new();
    for (key, game, platform) in games_in_folders(&entries, &folders) {
        let assets = crate::library::composed_assets_for(store, key)?;
        let or_null = |v: &Value| if v.is_null() { None } else { Some(v.clone()) };
        roms.push(json!({
            "objectId": game["objectId"], "title": game["title"],
            "coverImageUrl": assets["coverImageUrl"], "libraryImageUrl": assets["libraryImageUrl"],
            "iconUrl": or_null(&assets["iconUrl"]).or_else(|| or_null(&game["iconUrl"])),
            "customCoverImageUrl": game["customCoverImageUrl"], "customIconUrl": game["customIconUrl"],
            "sizeBytes": game["romSizeBytes"], "skus": [], "platform": platform,
        }));
    }
    roms.sort_by_key(|r| r["title"].as_str().unwrap_or_default().to_lowercase());
    Ok(Value::Array(roms))
}

// ---- disc reconciliation (reconcile-discs.ts) ---------------------------------

/// Some((discs, selected, deleted)) when a game's discs changed.
type Reconciled = (Vec<Value>, Value, bool);

fn reconciliation(discs: &[Value], surviving: Vec<Value>, selected: &Value) -> Option<Reconciled> {
    if surviving.len() == discs.len() {
        return None;
    }
    if surviving.is_empty() {
        return Some((discs.to_vec(), selected.clone(), true));
    }
    let survived = surviving.iter().any(|d| d["path"] == *selected);
    let selected = if selected.as_str().is_some_and(|s| !s.is_empty()) && !survived { Value::Null } else { selected.clone() };
    Some((surviving, selected, false))
}

fn reconcile_for_removed_folder(discs: &[Value], selected: &Value, removed: &str, remaining: &[String]) -> Option<Reconciled> {
    if !discs.iter().any(|d| is_within(disc_path(d), removed)) {
        return None;
    }
    let covered = discs.iter().filter(|d| remaining.iter().any(|f| is_within(disc_path(d), f))).cloned().collect();
    reconciliation(discs, covered, selected)
}

fn reconcile_after_scan(discs: &[Value], selected: &Value, scanned: &[String], exists: impl Fn(&str) -> bool) -> Option<Reconciled> {
    let was_scanned = |d: &Value| scanned.iter().any(|f| is_within(disc_path(d), f));
    if !discs.iter().any(was_scanned) {
        return None;
    }
    let surviving = discs.iter().filter(|d| !was_scanned(d) || exists(disc_path(d))).cloned().collect();
    reconciliation(discs, surviving, selected)
}

fn apply_reconciliation(store: &Store, key: &str, game: &Value, (discs, selected, deleted): Reconciled) -> Result<(), String> {
    let mut game = game.clone();
    if deleted {
        game["isDeleted"] = json!(true);
    } else {
        let sizes: Vec<u64> = discs.iter().filter_map(|d| fs::metadata(disc_path(d)).ok().map(|m| m.len())).collect();
        game["romSizeBytes"] = if sizes.is_empty() { Value::Null } else { json!(sizes.iter().sum::<u64>()) };
        game["discs"] = Value::Array(discs);
        game["selectedDiscPath"] = selected;
    }
    store.put("games", key, &game)
}

fn retroarch_games(store: &Store) -> Result<Vec<(String, Value)>, String> {
    Ok(store
        .entries("games")?
        .into_iter()
        .filter(|(_, g)| g["isDeleted"].as_bool() != Some(true) && g["shop"].as_str() == Some("launchbox") && g["platform"].as_str().and_then(retroarch_platform).is_some())
        .collect())
}

/// reconcileRemovedRetroArchFolder.
fn reconcile_removed_folder(store: &Store, removed: &str, remaining: &[String]) -> Result<(), String> {
    for (key, game) in retroarch_games(store)? {
        if let Some(r) = reconcile_for_removed_folder(&discs(&game), &game["selectedDiscPath"], removed, remaining) {
            apply_reconciliation(store, &key, &game, r)?;
        }
    }
    Ok(())
}

// ---- scanning, archives and hashing -------------------------------------------

#[derive(Clone)]
struct ScannedRom {
    folder: String,
    path: PathBuf,
    name: String,
    size: u64,
    platform: &'static str,
    archive_entry: Option<(String, u64)>,
}

/// collectFilesByExtension: breadth first, symlinked folders visited once, 50,000 files at most.
fn collect_files(root: &Path, scan_subfolders: bool, cancel: &AtomicBool) -> Vec<(PathBuf, String, u64)> {
    let mut out = Vec::new();
    let mut queue = std::collections::VecDeque::from([root.to_path_buf()]);
    let mut seen = HashSet::new();
    while let Some(dir) = queue.pop_front() {
        if cancel.load(Ordering::Relaxed) || out.len() >= MAX_COLLECTED_FILES {
            break;
        }
        let Ok(real) = fs::canonicalize(&dir) else { continue };
        if !seen.insert(real) {
            continue;
        }
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let Ok(meta) = fs::metadata(&path) else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            if meta.is_dir() {
                if scan_subfolders {
                    queue.push_back(path);
                }
            } else if extension_platform(&name).is_some() || is_archive(&name) {
                out.push((path, name, meta.len()));
            }
        }
    }
    out
}

fn seven_zip_command(binary: &Path) -> Command {
    let mut command = Command::new(binary);
    emulators::no_window(&mut command).stdin(Stdio::null()).stderr(Stdio::null());
    command
}

/// listArchiveEntries: `7z l -slt` records as (name, size, encrypted).
fn parse_listing(stdout: &str) -> Vec<(String, u64, bool)> {
    stdout
        .replace("\r\n", "\n")
        .split("\n\n")
        .filter_map(|block| {
            let fields: HashMap<&str, &str> = block.lines().filter_map(|l| l.split_once(" = ")).collect();
            let name = fields.get("Path").filter(|n| !n.is_empty())?;
            if fields.get("Folder") == Some(&"+") || fields.get("Attributes").is_some_and(|a| a.contains('D')) {
                return None;
            }
            let size = fields.get("Size")?.parse::<u64>().ok()?;
            Some((name.to_string(), size, fields.get("Encrypted") == Some(&"+")))
        })
        .collect()
}

// shortcut: no 30 s timeout on 7-Zip like Electron; add a kill timer if a broken archive ever hangs a scan.
fn list_archive(binary: &Path, archive: &Path) -> Option<Vec<(String, u64, bool)>> {
    let output = seven_zip_command(binary).args(["l", "-slt", "-ba", "-sccUTF-8", "-p-", "--"]).arg(archive).output().ok()?;
    output.status.success().then(|| parse_listing(&String::from_utf8_lossy(&output.stdout)))
}

/// selectArchivedRom: a bare archive must hold exactly one ROM, else RetroArch may load another one.
fn select_archived_rom(entries: &[(String, u64, bool)]) -> Option<(String, u64, &'static str)> {
    let roms: Vec<_> = entries.iter().filter_map(|(name, size, enc)| extension_platform(name).map(|p| (name, *size, *enc, p))).collect();
    let [(name, size, encrypted, platform)] = roms.as_slice() else { return None };
    (!encrypted && *size > 0 && *size <= MAX_ARCHIVED_ROM_BYTES).then(|| (name.to_string(), *size, *platform))
}

/// readArchiveEntry: to stdout, so entry names can never write files.
fn read_archive_entry(binary: &Path, archive: &Path, entry: &str, max: u64) -> Option<Vec<u8>> {
    let mut child = seven_zip_command(binary).args(["x", "-so", "-spd", "-p-"]).arg(format!("-i!{entry}")).arg("--").arg(archive).stdout(Stdio::piped()).spawn().ok()?;
    let mut data = Vec::new();
    let read = child.stdout.take()?.take(max + 1).read_to_end(&mut data);
    let _ = child.kill();
    let status = child.wait().ok()?;
    (read.is_ok() && data.len() as u64 <= max && (status.success() || data.len() as u64 == max)).then_some(data)
}

#[derive(Clone, Copy, PartialEq)]
enum N64Order {
    Z64,
    V64,
    N64,
}

/// resolveNormalization: the bytes to skip (iNES/FDS or SMC copier headers) and the N64 byte order.
fn normalization(head: &[u8], size: u64, platform: &str) -> (u64, N64Order) {
    if platform == "nes" && size > 16 && head.len() >= 4 && (head[..4] == *b"NES\x1a" || head[..4] == *b"FDS\x1a") {
        return (16, N64Order::Z64);
    }
    if platform == "snes" && size > 512 && size % 1024 == 512 {
        return (512, N64Order::Z64);
    }
    if platform == "n64" && size >= 4 && size % 4 == 0 && head.len() >= 4 {
        match u32::from_be_bytes([head[0], head[1], head[2], head[3]]) {
            0x3780_4012 => return (0, N64Order::V64),
            0x4012_3780 => return (0, N64Order::N64),
            _ => {}
        }
    }
    (0, N64Order::Z64)
}

fn swap(chunk: &mut [u8], order: N64Order) {
    match order {
        N64Order::V64 => chunk.chunks_exact_mut(2).for_each(|w| w.swap(0, 1)),
        N64Order::N64 => chunk.chunks_exact_mut(4).for_each(|w| w.reverse()),
        N64Order::Z64 => {}
    }
}

fn crc_hex(hasher: crc32fast::Hasher) -> String {
    format!("{:08X}", hasher.finalize())
}

/// hashRomBuffer: Hydra's `headerless-v1` CRC32.
fn hash_rom_buffer(mut data: Vec<u8>, platform: &str) -> String {
    let (skip, order) = normalization(&data, data.len() as u64, platform);
    let content = &mut data[skip as usize..];
    if platform == "n64" {
        swap(content, order);
    }
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(content);
    crc_hex(hasher)
}

/// hashRomFile, streaming 1 MiB at a time.
fn hash_rom_file(path: &Path, platform: &str, cancel: &AtomicBool) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let mut head = Vec::with_capacity(16);
    (&mut file).take(16).read_to_end(&mut head).ok()?;
    let (skip, order) = normalization(&head, size, platform);
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(skip)).ok()?;
    let mut hasher = crc32fast::Hasher::new();
    let mut chunk = vec![0u8; 1024 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        // Fill whole chunks so N64 word swaps never straddle a read boundary.
        let mut filled = 0;
        while filled < chunk.len() {
            match file.read(&mut chunk[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(_) => return None,
            }
        }
        if filled == 0 {
            break;
        }
        if platform == "n64" {
            swap(&mut chunk[..filled], order);
        }
        hasher.update(&chunk[..filled]);
    }
    Some(crc_hex(hasher))
}

fn hash_rom(binary: Option<&Path>, rom: &ScannedRom, cancel: &AtomicBool) -> Option<String> {
    match (&rom.archive_entry, binary) {
        (Some((entry, size)), Some(binary)) => read_archive_entry(binary, &rom.path, entry, MAX_ARCHIVED_ROM_BYTES).filter(|d| d.len() as u64 == *size).map(|d| hash_rom_buffer(d, rom.platform)),
        (Some(_), None) => None,
        (None, _) => hash_rom_file(&rom.path, rom.platform, cancel),
    }
}

/// scanRetroArchFolder: a folder (or a single ROM file) as ROMs with their platform.
// shortcut: archives are listed one at a time (Electron runs 4 at once); parallelise if big zip sets scan slowly.
fn scan_folder(binary: Option<&Path>, folder: &Folder, cancel: &AtomicBool) -> Vec<ScannedRom> {
    let root = Path::new(&folder.path);
    let files = match fs::metadata(root) {
        Ok(m) if m.is_file() => vec![(root.to_path_buf(), root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), m.len())],
        _ => collect_files(root, folder.scan_subfolders, cancel),
    };
    let mut roms = Vec::new();
    for (path, name, size) in files {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let archived = if is_archive(&name) { binary.and_then(|b| select_archived_rom(&list_archive(b, &path)?)) } else { None };
        let Some(platform) = archived.as_ref().map(|a| a.2).or_else(|| extension_platform(&name)) else { continue };
        roms.push(ScannedRom { folder: folder.path.clone(), path, name, size, platform, archive_entry: archived.map(|(n, s, _)| (n, s)) });
    }
    roms
}

fn seven_zip_binary(app: &AppHandle) -> Option<PathBuf> {
    crate::extraction::seven_zip(app).ok()
}

/// previewRetroArchRomFolder.
pub fn preview_folder(app: &AppHandle, path: &str, scan_subfolders: bool) -> Value {
    let binary = seven_zip_binary(app);
    let folder = Folder { path: path.to_string(), scan_subfolders };
    json!({ "fileCount": scan_folder(binary.as_deref(), &folder, &AtomicBool::new(false)).len() })
}

// ---- import (import-retroarch-roms.ts) ------------------------------------------

/// The one RetroArch import that may run, plus the snapshot getActiveRetroArchImport answers.
#[derive(Default)]
pub struct RetroArchImports(Mutex<Option<ActiveImport>>);

struct ActiveImport {
    request_id: String,
    cancel: Arc<AtomicBool>,
    snapshot: Value,
    done: tokio::sync::watch::Receiver<bool>,
}

impl RetroArchImports {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<ActiveImport>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn active(&self) -> Value {
        self.lock().as_ref().map(|a| a.snapshot.clone()).unwrap_or(Value::Null)
    }

    pub fn cancel(&self, request_id: &str) {
        if let Some(active) = self.lock().as_ref().filter(|a| a.request_id == request_id) {
            active.cancel.store(true, Ordering::Relaxed);
        }
    }
}

const SCAN_BAND: f64 = 10.0;
const HASH_BAND: f64 = 60.0;
const PROGRESS_CHANNEL: &str = "on-retroarch-import-progress";

type Progress = Arc<dyn Fn(Value) + Send + Sync>;

#[allow(clippy::too_many_arguments)]
fn progress(phase: &str, processed: usize, total: usize, percent: f64, current: Option<&str>, status: Option<&str>, discovered: usize, matched: usize, size: u64) -> Value {
    json!({
        "type": "progress", "phase": phase, "processed": processed, "total": total, "percent": percent,
        "currentFile": current, "status": status, "discovered": discovered, "matched": matched, "sizeBytes": size,
    })
}

/// scanRetroArchFolders + hashRoms: blocking reads.
fn scan_and_hash(binary: Option<&Path>, folders: &[Folder], cancel: &AtomicBool, emit: &dyn Fn(Value)) -> Vec<(ScannedRom, Option<String>)> {
    let mut collected: Vec<ScannedRom> = Vec::new();
    let mut seen = HashSet::new();
    for (i, folder) in folders.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return vec![];
        }
        for rom in scan_folder(binary, folder, cancel) {
            if seen.insert(rom.path.clone()) {
                collected.push(rom);
            }
        }
        emit(progress("scanning", i + 1, folders.len(), band_percent(0.0, SCAN_BAND, i + 1, folders.len()), None, None, collected.len(), 0, 0));
    }
    let total = collected.len();
    let mut hashed = Vec::with_capacity(total);
    for (i, rom) in collected.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return vec![];
        }
        let crc = hash_rom(binary, &rom, cancel);
        if crc.is_none() {
            eprintln!("[retroarch-import] could not hash {}", rom.path.display());
        }
        emit(progress("scanning", i + 1, total, band_percent(SCAN_BAND, HASH_BAND, i + 1, total), Some(&rom.name), None, total, 0, 0));
        hashed.push((rom, crc));
    }
    hashed
}

/// fetchShopDetailsForHashes, per platform, 100 ROMs a request. `failed` when any batch failed.
async fn match_roms(store: &Store, version: &str, hashed: &[(ScannedRom, Option<String>)], language: &str, cancel: &AtomicBool) -> (HashMap<String, Value>, bool) {
    let (mut lookup, mut failed) = (HashMap::new(), false);
    for platform in PLATFORMS {
        let roms: Vec<Value> = hashed
            .iter()
            .filter(|(r, crc)| r.platform == platform && crc.is_some())
            .map(|(r, crc)| {
                let (file_name, size) = r.archive_entry.as_ref().map(|(n, s)| (n.as_str(), *s)).unwrap_or((r.name.as_str(), r.size));
                json!({ "crc32": crc, "fileName": file_name, "sizeBytes": size, "serial": null })
            })
            .collect();
        for chunk in roms.chunks(100) {
            if cancel.load(Ordering::Relaxed) {
                return (lookup, failed);
            }
            let request = json!({
                "method": "post", "url": "/games/shop-details",
                "data": { "shop": "launchbox", "platform": platform, "hashFormat": "headerless-v1", "roms": chunk, "language": language },
                "options": { "needsAuth": false },
            });
            match crate::hydra_api::call(store, version, &request).await {
                Ok(Value::Array(entries)) => {
                    for entry in entries.into_iter().filter(|e| e["objectId"].is_string() && !e["data"].is_null()) {
                        let hashes = ["matchedCrc32s", "matchedSkus", "skus"].into_iter().map(|k| &entry[k]).find(|v| !v.is_null()).cloned().unwrap_or_default();
                        for hash in hashes.as_array().into_iter().flatten().filter_map(Value::as_str) {
                            lookup.insert(hash.trim().to_uppercase(), entry.clone());
                        }
                    }
                }
                Ok(_) => failed = true,
                Err(error) => {
                    failed = true;
                    eprintln!("[retroarch-import] rom-match batch failed: {error}");
                }
            }
        }
    }
    (lookup, failed)
}

/// buildRomDiscList: by file name, labelled without the extension, no SKU.
fn rom_disc_list(mut files: Vec<(String, String)>) -> Vec<Value> {
    files.sort_by_key(|(_, name)| name.to_lowercase());
    files.into_iter().map(|(path, name)| json!({ "path": path, "label": without_ext(&name), "fileName": name, "sku": null })).collect()
}

/// persistFolderRollups: scanned folders get their counts and scan time.
fn persist_rollups(store: &Store, rollups: &[(&Folder, u64, u64)]) -> Result<Value, String> {
    update_config(store, |config| {
        let folders = folders_mut(config);
        for (folder, files, size) in rollups {
            let existing = folders.iter().position(|f| f["path"].as_str() == Some(&folder.path));
            let id = existing.and_then(|i| folders[i]["id"].as_str().map(str::to_string)).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let updated = json!({ "id": id, "path": folder.path, "scanSubfolders": folder.scan_subfolders, "fileCount": files, "sizeBytes": size, "lastScanAt": now_ms() });
            match existing {
                Some(i) => folders[i] = updated,
                None => folders.push(updated),
            }
        }
        recompute_totals(config);
    })
}

/// runRetroArchImport: scan, hash, match against Hydra's LaunchBox catalogue by CRC32, then save.
async fn run(app: &AppHandle, folders: Vec<Folder>, language: &str, cancel: Arc<AtomicBool>, emit: Progress) -> Result<Value, String> {
    let store = app.state::<Store>();
    let result = |files: u64, size: u64, matched: usize, unmatched: &[Value], cancelled: bool| {
        json!({ "fileCount": files, "sizeBytes": size, "matched": matched, "unmatched": unmatched.len(), "unmatchedFiles": unmatched, "cancelled": cancelled })
    };
    let is_cancelled = || cancel.load(Ordering::Relaxed);

    // Registered up front: a cancelled scan still lists the folder the user picked.
    for folder in &folders {
        add_folder(&store, &folder.path, folder.scan_subfolders)?;
    }

    let binary = seven_zip_binary(app);
    let (hashed, folders) = {
        let (cancel, emit) = (cancel.clone(), emit.clone());
        tauri::async_runtime::spawn_blocking(move || (scan_and_hash(binary.as_deref(), &folders, &cancel, &*emit), folders)).await.map_err(|e| e.to_string())?
    };
    if is_cancelled() {
        return Ok(result(0, 0, 0, &[], true));
    }

    let version = app.package_info().version.to_string();
    let (lookup, match_failed) = match_roms(&store, &version, &hashed, language, &cancel).await;
    if is_cancelled() {
        return Ok(result(0, 0, 0, &[], true));
    }

    // aggregateMatches: per title, its first folder, platform, size and ROM files.
    let mut titles: Vec<String> = Vec::new();
    let mut title_entry: HashMap<String, &Value> = HashMap::new();
    let mut title_info: HashMap<String, (String, &'static str, u64)> = HashMap::new();
    let mut title_files: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let (mut unmatched, mut matched_size) = (Vec::new(), 0u64);
    let total = hashed.len();
    for (i, (rom, crc)) in hashed.iter().enumerate() {
        if is_cancelled() {
            break;
        }
        let entry = crc.as_ref().and_then(|c| lookup.get(c));
        match entry {
            Some(entry) => {
                let id = entry["objectId"].as_str().unwrap_or_default().to_string();
                if title_entry.insert(id.clone(), entry).is_none() {
                    titles.push(id.clone());
                }
                let path = rom.path.to_string_lossy().into_owned();
                let files = title_files.entry(id.clone()).or_default();
                if !files.iter().any(|(p, _)| *p == path) {
                    files.push((path, rom.name.clone()));
                    title_info.entry(id).or_insert_with(|| (rom.folder.clone(), rom.platform, 0)).2 += rom.size;
                }
                matched_size += rom.size;
            }
            None => unmatched.push(json!({ "name": rom.name, "reason": "unmatched" })),
        }
        let start = SCAN_BAND + HASH_BAND;
        let status = if entry.is_some() { "matched" } else { "unmatched" };
        emit(progress("matching", i + 1, total, band_percent(start, 100.0 - start, i + 1, total), Some(&rom.name), Some(status), total, title_entry.len(), matched_size));
    }
    let matched = titles.len();
    eprintln!("[retroarch-import] discovered {total}, hashed {}, matched titles {matched}, unmatched {}, match failed {match_failed}", hashed.iter().filter(|(_, c)| c.is_some()).count(), unmatched.len());

    // computeFolderRollups: one file per matched title, in the folder of its first ROM.
    let mut rollups: Vec<(&Folder, u64, u64)> = folders.iter().map(|f| (f, 0, 0)).collect();
    for (folder, _, size) in title_info.values() {
        if let Some(bucket) = rollups.iter_mut().find(|(f, _, _)| f.path == *folder) {
            bucket.1 += 1;
            bucket.2 += size;
        }
    }
    let total_files: u64 = rollups.iter().map(|r| r.1).sum();
    let total_size: u64 = rollups.iter().map(|r| r.2).sum();
    if is_cancelled() {
        return Ok(result(total_files, total_size, matched, &unmatched, true));
    }

    for id in &titles {
        let info = &title_info[id];
        let discs = rom_disc_list(title_files.remove(id).unwrap_or_default());
        if let Err(error) = persist_entry(&store, title_entry[id], language, discs, Some(launchbox_name(info.1)), Some(info.2)) {
            eprintln!("[retroarch-import] could not save {id}: {error}");
        }
    }
    // A failed match keeps the totals of folders that already had some.
    let config = get_config(&store)?;
    let known_count = |path: &str| config["romFolders"].as_array().into_iter().flatten().find(|f| f["path"].as_str() == Some(path)).and_then(|f| f["fileCount"].as_u64()).unwrap_or(0);
    rollups.retain(|(f, _, _)| !match_failed || known_count(&f.path) == 0);
    persist_rollups(&store, &rollups)?;

    let scanned: Vec<String> = folders.iter().map(|f| f.path.clone()).collect();
    for (key, game) in retroarch_games(&store)? {
        if let Some(r) = reconcile_after_scan(&discs(&game), &game["selectedDiscPath"], &scanned, |p| Path::new(p).exists()) {
            apply_reconciliation(&store, &key, &game, r)?;
        }
    }
    recompute_counts(&store)?;
    // shortcut: no /profile/games/batch push; the Tauri build has no Hydra login. Add it with library sync.
    Ok(result(total_files, total_size, matched, &unmatched, false))
}

/// startTrackedRetroArchImport: one import at a time; a second call joins the running one.
pub fn start(app: &AppHandle, folders: Vec<Folder>, language: &str) -> (String, tokio::sync::watch::Receiver<bool>) {
    let imports = app.state::<RetroArchImports>();
    let mut active = imports.lock();
    if let Some(a) = active.as_ref() {
        return (a.request_id.clone(), a.done.clone());
    }
    let request_id = uuid::Uuid::new_v4().to_string();
    let cancel = Arc::new(AtomicBool::new(false));
    let (done_tx, done) = tokio::sync::watch::channel(false);
    let mut snapshot = progress("scanning", 0, 0, 0.0, None, None, 0, 0, 0);
    snapshot["requestId"] = json!(request_id);
    snapshot.as_object_mut().map(|s| s.remove("type"));
    *active = Some(ActiveImport { request_id: request_id.clone(), cancel: cancel.clone(), snapshot, done: done.clone() });
    drop(active);
    let _ = app.emit("on-retroarch-import-status", true);

    let (app, language, id) = (app.clone(), language.to_string(), request_id.clone());
    tauri::async_runtime::spawn(async move {
        let emit: Progress = {
            let (app, id) = (app.clone(), id.clone());
            Arc::new(move |payload: Value| {
                if let Some(active) = app.state::<RetroArchImports>().lock().as_mut() {
                    if let (Value::Object(snapshot), Value::Object(patch)) = (&mut active.snapshot, &payload) {
                        snapshot.extend(patch.iter().filter(|(k, _)| *k != "type").map(|(k, v)| (k.clone(), v.clone())));
                    }
                }
                let mut event = payload;
                event["requestId"] = json!(id);
                let _ = app.emit(PROGRESS_CHANNEL, event);
            })
        };
        let mut event = match run(&app, folders, &language, cancel, emit).await {
            Ok(mut result) => {
                let cancelled = result.as_object_mut().and_then(|r| r.remove("cancelled")) == Some(json!(true));
                result["type"] = json!(if cancelled { "cancelled" } else { "done" });
                result
            }
            Err(message) => json!({ "type": "error", "message": message }),
        };
        event["requestId"] = json!(id);
        let _ = app.emit(PROGRESS_CHANNEL, event);
        *app.state::<RetroArchImports>().lock() = None;
        let _ = app.emit("on-retroarch-import-status", false);
        let _ = done_tx.send(true);
    });
    (request_id, done)
}

/// rescanRetroArch: re-imports every folder and answers the config once done.
pub async fn rescan(app: &AppHandle, language: &str) -> Result<Value, String> {
    let store = app.state::<Store>();
    let current = get_config(&store)?;
    let folders: Vec<Folder> = current["romFolders"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| Some(Folder { path: f["path"].as_str()?.to_string(), scan_subfolders: f["scanSubfolders"].as_bool().unwrap_or(false) }))
        .collect();
    if folders.is_empty() {
        return update_config(&store, recompute_totals);
    }
    let (_, mut done) = start(app, folders, language);
    let _ = done.wait_for(|d| *d).await;
    get_config(&store)
}

// ---- installs (retroarch-core-installer.ts, retroarch-install-sources.ts) -----

fn buildbot_platform() -> Option<&'static str> {
    if std::env::consts::ARCH == "aarch64" {
        return None;
    }
    if cfg!(windows) {
        Some("windows/x86_64")
    } else if cfg!(target_os = "linux") {
        Some("linux/x86_64")
    } else {
        None
    }
}

fn core_url(core: &str) -> Option<String> {
    Some(format!("{BUILDBOT}/nightly/{}/latest/{}.zip", buildbot_platform()?, library_file_name(core)))
}

fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder().user_agent("Chimaera").build().map_err(|e| e.to_string())
}

/// downloadToFile: streams to `dest` and answers the Last-Modified header.
async fn download(url: &str, dest: &Path, on_progress: impl Fn(u64, Option<u64>)) -> Result<Option<String>, String> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut response = http()?.get(url).send().await.and_then(|r| r.error_for_status()).map_err(|e| e.to_string())?;
    let total = response.content_length();
    let last_modified = response.headers().get("last-modified").and_then(|v| v.to_str().ok()).map(str::to_string);
    let mut file = fs::File::create(dest).map_err(|e| e.to_string())?;
    let (mut received, mut last_emit) = (0u64, 0u64);
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        std::io::Write::write_all(&mut file, &chunk).map_err(|e| e.to_string())?;
        received += chunk.len() as u64;
        if received - last_emit >= 512 * 1024 || total == Some(received) {
            last_emit = received;
            on_progress(received, total);
        }
    }
    Ok(last_modified)
}

async fn extract(app: &AppHandle, archive: &Path, dest: &Path) -> Result<(), String> {
    let binary = crate::extraction::seven_zip(app)?;
    let (archive, dest) = (archive.to_path_buf(), dest.to_path_buf());
    tauri::async_runtime::spawn_blocking(move || crate::extraction::run_7z(&binary, &archive, &dest, &mut |_| {})).await.map_err(|e| e.to_string())?
}

fn temp_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().temp_dir().map_err(|e| e.to_string())?;
    Ok(dir)
}

/// swapCoreLibrary: the previous library comes back when the copy or the config write fails.
fn swap_core_library(staged: &Path, target: &Path, commit: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let backup = PathBuf::from(format!("{}.backup", target.display()));
    let _ = fs::remove_file(&backup);
    let had_previous = target.exists();
    if had_previous {
        fs::rename(target, &backup).map_err(|e| e.to_string())?;
    }
    let outcome = fs::copy(staged, target).map_err(|e| e.to_string()).and_then(|_| commit());
    if let Err(error) = outcome {
        let _ = fs::remove_file(target);
        if had_previous {
            let _ = fs::rename(&backup, target);
        }
        return Err(error);
    }
    let _ = fs::remove_file(&backup);
    Ok(())
}

/// downloadAndInstallCore.
pub async fn install_core(app: &AppHandle, core: &str) -> Value {
    let send = |payload: Value| {
        let _ = app.emit_to("main", "on-retroarch-core-install-progress", payload);
    };
    let fail = |reason: &str| {
        send(json!({ "core": core, "phase": "error", "reason": reason }));
        json!({ "ok": false, "core": core, "reason": reason })
    };
    if !CORES.contains(&core) {
        eprintln!("[retroarch] refused to install unknown core {core}");
        return json!({ "ok": false, "core": core, "reason": "invalid_core" });
    }
    let Some(url) = core_url(core) else { return fail("unsupported_platform") };
    let (Ok(temp), Ok(managed)) = (temp_dir(app), managed_dir(app)) else { return fail("install_failed") };
    let library = library_file_name(core);
    let archive = temp.join(format!("{library}.zip"));
    let staging = temp.join(format!("retroarch-core-{core}-staging"));
    let store = app.state::<Store>();
    let target = match get_config(&store) {
        Ok(config) => resolve_cores_dir(&config, &managed).join(&library),
        Err(_) => return fail("install_failed"),
    };

    let outcome: Result<Result<(), &str>, String> = async {
        send(json!({ "core": core, "phase": "downloading", "loaded": 0 }));
        let last_modified = download(&url, &archive, |loaded, total| send(json!({ "core": core, "phase": "downloading", "loaded": loaded, "total": total }))).await?;
        send(json!({ "core": core, "phase": "extracting" }));
        let _ = fs::remove_dir_all(&staging);
        fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
        extract(app, &archive, &staging).await?;
        let _ = fs::remove_file(&archive);
        let staged = staging.join(&library);
        if !staged.exists() {
            return Ok(Err("extract_failed"));
        }
        swap_core_library(&staged, &target, || {
            update_config(&store, |c| {
                c["cores"][core] = json!({ "name": core, "installed": true, "version": last_modified, "path": target.to_string_lossy(), "installedAt": now_ms() });
            })
            .map(|_| ())
        })?;
        Ok(Ok(()))
    }
    .await;
    let _ = fs::remove_file(&archive);
    let _ = fs::remove_dir_all(&staging);
    match outcome {
        Ok(Ok(())) => {
            let path = target.to_string_lossy();
            send(json!({ "core": core, "phase": "done", "path": path }));
            json!({ "ok": true, "core": core, "path": path })
        }
        Ok(Err(reason)) => fail(reason),
        Err(error) => {
            eprintln!("[retroarch] core {core} install failed: {error}");
            fail("install_failed")
        }
    }
}

pub async fn install_all_cores(app: &AppHandle) -> Value {
    let mut results = Vec::new();
    for core in CORES {
        results.push(install_core(app, core).await);
    }
    Value::Array(results)
}

static STABLE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"href="(?:/stable/)?(\d+\.\d+(?:\.\d+)?)/""#).unwrap());

fn newest_version(listing: &str) -> Option<String> {
    let parts = |v: &str| v.split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    STABLE_RE.captures_iter(listing).map(|c| c[1].to_string()).max_by_key(|v| parts(v))
}

/// fetchLatestStableVersion: the newest folder in the buildbot's `stable/` listing.
async fn latest_stable() -> Option<String> {
    let listing = http().ok()?.get(format!("{BUILDBOT}/stable/")).timeout(Duration::from_secs(15)).send().await.ok()?.error_for_status().ok()?.text().await.ok()?;
    newest_version(&listing)
}

fn link_option(id: &str, url: &str, kind: &str) -> Value {
    json!({ "id": id, "kind": "link", "downloadUrl": null, "fileName": null, "version": null, "linkUrl": url, "linkKind": kind })
}

/// resolveRetroArchInstallOptions.
pub async fn install_options() -> Value {
    let mut options = Vec::new();
    if let Some(platform) = buildbot_platform() {
        let version = latest_stable().await;
        let url = match &version {
            Some(v) => format!("{BUILDBOT}/stable/{v}/{platform}/RetroArch.7z"),
            None => format!("{BUILDBOT}/nightly/{platform}/RetroArch.7z"),
        };
        let (id, kind) = if cfg!(windows) { ("retroarch-windows-archive", "windows-archive") } else { ("retroarch-linux-appimage", "linux-appimage") };
        options.push(json!({ "id": id, "kind": kind, "downloadUrl": url, "fileName": "RetroArch.7z", "version": version, "linkUrl": null, "linkKind": null }));
    }
    if cfg!(target_os = "linux") {
        options.push(link_option("retroarch-flatpak", "https://flathub.org/apps/org.libretro.RetroArch", "flatpak"));
        options.push(link_option("retroarch-aur", "https://aur.archlinux.org/packages/retroarch-git", "aur"));
    }
    Value::Array(options)
}

fn find_file(root: &Path, matches: &dyn Fn(&str) -> bool) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                stack.push(entry.path());
            } else if matches(&entry.file_name().to_string_lossy().to_lowercase()) {
                return Some(entry.path());
            }
        }
    }
    None
}

/// `fs.cp(..., { force: false })`: copies what the destination lacks.
fn copy_missing(from: &Path, to: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(from)?.flatten() {
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type()?.is_dir() {
            fs::create_dir_all(&dst)?;
            copy_missing(&src, &dst)?;
        } else if !dst.exists() {
            fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// swapDirectory: the new install replaces `target`, keeping files only the old one had
/// (the user's config and saves); the old folder comes back on failure.
fn swap_directory(staging: &Path, target: &Path, commit: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let backup = PathBuf::from(format!("{}.backup", target.display()));
    let _ = fs::remove_dir_all(&backup);
    let had_previous = target.exists();
    if had_previous {
        fs::rename(target, &backup).map_err(|e| e.to_string())?;
    }
    let outcome = fs::rename(staging, target)
        .and_then(|_| if had_previous { copy_missing(&backup, target) } else { Ok(()) })
        .map_err(|e| e.to_string())
        .and_then(|_| commit());
    if let Err(error) = outcome {
        let _ = fs::remove_dir_all(target);
        if had_previous {
            let _ = fs::rename(&backup, target);
        }
        return Err(error);
    }
    let _ = fs::remove_dir_all(&backup);
    Ok(())
}

/// downloadAndInstallRetroArch: into `<downloads>/RetroArch`, then shown in the file manager.
pub async fn install(app: &AppHandle, option_id: &str) -> Value {
    let send = |payload: Value| {
        let _ = app.emit_to("main", "on-retroarch-install-progress", payload);
    };
    let fail = |reason: &str| {
        send(json!({ "optionId": option_id, "phase": "error", "reason": reason }));
        json!({ "ok": false, "reason": reason })
    };
    let options = install_options().await;
    let Some(option) = options.as_array().into_iter().flatten().find(|o| o["id"].as_str() == Some(option_id) && o["kind"] != "link") else {
        eprintln!("[retroarch] install option {option_id} is not installable");
        return json!({ "ok": false, "reason": "option_not_installable" });
    };
    let Some(url) = option["downloadUrl"].as_str() else { return json!({ "ok": false, "reason": "option_not_installable" }) };
    let store = app.state::<Store>();
    let (Ok(temp), Ok(default_downloads)) = (temp_dir(app), app.path().download_dir()) else { return fail("install_failed") };
    let file_name = option["fileName"].as_str().unwrap_or("RetroArch.7z");
    let archive = temp.join(Path::new(file_name).file_name().unwrap_or_default());
    let target = crate::installer::downloads_path(&store, &default_downloads).join("RetroArch");
    let staging = PathBuf::from(format!("{}-staging", target.display()));

    let outcome: Result<Result<PathBuf, &str>, String> = async {
        send(json!({ "optionId": option_id, "phase": "downloading", "loaded": 0 }));
        download(url, &archive, |loaded, total| send(json!({ "optionId": option_id, "phase": "downloading", "loaded": loaded, "total": total }))).await?;
        send(json!({ "optionId": option_id, "phase": "extracting" }));
        let _ = fs::remove_dir_all(&staging);
        fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
        extract(app, &archive, &staging).await?;
        let _ = fs::remove_file(&archive);
        let wanted: &dyn Fn(&str) -> bool = if cfg!(windows) { &|n| n == "retroarch.exe" } else { &|n| n.ends_with(".appimage") };
        let Some(staged) = find_file(&staging, wanted) else { return Ok(Err("executable_not_found")) };
        let exe = target.join(staged.strip_prefix(&staging).map_err(|e| e.to_string())?);
        swap_directory(&staging, &target, || {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(&exe).map_err(|e| e.to_string())?.permissions().mode();
                fs::set_permissions(&exe, fs::Permissions::from_mode(mode | 0o100)).map_err(|e| e.to_string())?;
            }
            let version = emulators::version_of(&exe, &RETROARCH).or_else(|| option["version"].as_str().map(str::to_string));
            update_config(&store, |c| {
                c["executablePath"] = json!(exe.to_string_lossy());
                c["detectedVersion"] = json!(version);
                c["detectedAt"] = json!(now_ms());
            })
            .map(|_| ())
        })?;
        Ok(Ok(exe))
    }
    .await;
    let _ = fs::remove_file(&archive);
    let _ = fs::remove_dir_all(&staging);
    match outcome {
        Ok(Ok(exe)) => {
            let _ = app.opener().reveal_item_in_dir(&exe);
            send(json!({ "optionId": option_id, "phase": "done", "path": exe.to_string_lossy() }));
            json!({ "ok": true, "path": exe.to_string_lossy() })
        }
        Ok(Err(reason)) => fail(reason),
        Err(error) => {
            eprintln!("[retroarch] install failed: {error}");
            fail("install_failed")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_extensions_to_platforms() {
        assert_eq!(extension_platform("Zelda.SFC"), Some("snes"));
        assert_eq!(extension_platform("mario.z64"), Some("n64"));
        assert_eq!(extension_platform("pokemon.gbc"), Some("gbc"));
        assert_eq!(extension_platform("pokemon.gb"), Some("gb"));
        assert_eq!(extension_platform("readme.txt"), None);
        assert!(is_archive("set.7Z"));
    }

    #[test]
    fn hashes_headerless_roms() {
        // CRC32 of "123456789" is CBF43926.
        let body = b"123456789".to_vec();
        assert_eq!(hash_rom_buffer(body.clone(), "gba"), "CBF43926");
        let mut ines = b"NES\x1a".to_vec();
        ines.extend([0u8; 12]);
        ines.extend(&body);
        assert_eq!(hash_rom_buffer(ines, "nes"), "CBF43926");
        // SMC copier header: size % 1024 == 512.
        let smc: Vec<u8> = [0u8; 512].into_iter().chain((0..1024).map(|i| i as u8)).collect();
        let plain: Vec<u8> = (0..1024).map(|i| i as u8).collect();
        assert_eq!(hash_rom_buffer(smc, "snes"), hash_rom_buffer(plain, "snes"));
        // N64 byte orders all hash as big-endian z64.
        let z64 = vec![0x80, 0x37, 0x12, 0x40, 1, 2, 3, 4];
        let v64 = vec![0x37, 0x80, 0x40, 0x12, 2, 1, 4, 3];
        let n64 = vec![0x40, 0x12, 0x37, 0x80, 4, 3, 2, 1];
        let expected = hash_rom_buffer(z64, "n64");
        assert_eq!(hash_rom_buffer(v64, "n64"), expected);
        assert_eq!(hash_rom_buffer(n64, "n64"), expected);
    }

    #[test]
    fn streamed_hash_matches_buffer_hash() {
        let dir = std::env::temp_dir().join(format!("chimaera-ra-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let rom: Vec<u8> = [0x37, 0x80, 0x40, 0x12].into_iter().chain((0..3 * 1024 * 1024 + 4).map(|i| (i % 251) as u8)).collect();
        let path = dir.join("big.v64");
        fs::write(&path, &rom).unwrap();
        assert_eq!(hash_rom_file(&path, "n64", &AtomicBool::new(false)), Some(hash_rom_buffer(rom, "n64")));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn picks_a_single_rom_from_an_archive() {
        let listing = "Path = game.gba\r\nSize = 100\r\nFolder = -\r\nAttributes = A\r\n\r\nPath = docs\r\nSize = 0\r\nFolder = +\r\n\r\nPath = readme.txt\r\nSize = 5\r\n";
        let entries = parse_listing(listing);
        assert_eq!(entries.len(), 2);
        assert_eq!(select_archived_rom(&entries), Some(("game.gba".into(), 100, "gba")));
        let two = vec![("a.gba".into(), 1, false), ("b.gba".into(), 1, false)];
        assert_eq!(select_archived_rom(&two), None);
        assert_eq!(select_archived_rom(&[("a.gba".into(), 1, true)]), None);
        assert_eq!(select_archived_rom(&[("a.gba".into(), MAX_ARCHIVED_ROM_BYTES + 1, false)]), None);
    }

    /// Needs binaries/7z.exe: a zipped ROM is listed, picked and hashed like the bare file.
    #[test]
    #[cfg(windows)]
    fn hashes_a_rom_inside_a_zip() {
        let binary = Path::new(env!("CARGO_MANIFEST_DIR")).join("../binaries/7z.exe");
        if !binary.exists() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("chimaera-ra7z-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let rom = dir.join("Game (USA).gba");
        fs::write(&rom, b"123456789").unwrap();
        let archive = dir.join("game.zip");
        assert!(Command::new(&binary).arg("a").arg(&archive).arg(&rom).stdout(Stdio::null()).status().unwrap().success());
        let picked = select_archived_rom(&list_archive(&binary, &archive).unwrap()).unwrap();
        assert_eq!(picked, ("Game (USA).gba".to_string(), 9, "gba"));
        let folder = Folder { path: dir.to_string_lossy().into_owned(), scan_subfolders: false };
        let scanned = scan_folder(Some(&binary), &folder, &AtomicBool::new(false));
        let zipped = scanned.iter().find(|r| r.archive_entry.is_some()).unwrap();
        assert_eq!(hash_rom(Some(&binary), zipped, &AtomicBool::new(false)).as_deref(), Some("CBF43926"));
        assert_eq!(scanned.len(), 2);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reads_the_libretro_directory() {
        let cfg = Path::new("/ra/retroarch.cfg");
        assert_eq!(libretro_directory("video = 1\nlibretro_directory = \":\\cores\"\n", cfg), Some(PathBuf::from("/ra").join("cores")));
        assert_eq!(libretro_directory("libretro_directory = \"/abs/cores\"", cfg), Some(PathBuf::from("/abs/cores")));
        assert_eq!(libretro_directory("libretro_directory = \"\"", cfg), None);
        assert_eq!(libretro_directory("other = 1", cfg), None);
    }

    #[test]
    fn reconciles_discs() {
        let sep = std::path::MAIN_SEPARATOR;
        let (a, b) = (format!("{sep}roms{sep}a.gba"), format!("{sep}other{sep}b.gba"));
        let discs = vec![json!({ "path": a }), json!({ "path": b })];
        let roms = format!("{sep}roms");
        let other = format!("{sep}other");
        // Folder removed, the other disc still covered: selection on the removed disc clears.
        let (kept, selected, deleted) = reconcile_for_removed_folder(&discs, &json!(a), &roms, &[other.clone()]).unwrap();
        assert_eq!((kept.len(), selected, deleted), (1, Value::Null, false));
        assert!(reconcile_for_removed_folder(&discs, &Value::Null, &format!("{sep}none"), &[]).is_none());
        // Nothing left: the game is hidden.
        assert!(reconcile_for_removed_folder(&discs, &Value::Null, &roms, &[]).unwrap().2);
        // After a scan, missing files in scanned folders drop out.
        let (kept, selected, _) = reconcile_after_scan(&discs, &json!(b), &[roms.clone()], |_| false).unwrap();
        assert_eq!((kept.len(), selected), (1, json!(b)));
        assert!(reconcile_after_scan(&discs, &Value::Null, &[roms], |_| true).is_none());
    }

    #[test]
    fn config_merges_and_cores_follow_their_folder() {
        let store = Store::memory().unwrap();
        store.put("retroarch", "config", &json!({ "executablePath": "x", "cores": { "mgba": { "name": "mgba", "installed": true, "path": "/gone.dll" } } })).unwrap();
        let config = get_config(&store).unwrap();
        assert_eq!(config["cores"]["fceumm"]["installed"], false);
        assert_eq!(config["perPlatformCounts"]["gba"], 0);
        assert_eq!(config_checked(&store).unwrap()["cores"]["mgba"]["installed"], false);

        let dir = std::env::temp_dir().join(format!("chimaera-cores-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(library_file_name("snes9x")), b"lib").unwrap();
        let cores = cores_state_for_dir(&dir, &config["cores"]);
        assert_eq!((cores["snes9x"]["installed"].as_bool(), cores["mgba"]["installed"].as_bool()), (Some(true), Some(false)));
        let again = cores_state_for_dir(&dir, &cores);
        assert_eq!(again["snes9x"]["installedAt"], cores["snes9x"]["installedAt"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn folder_channels_keep_ids_and_totals() {
        let store = Store::memory().unwrap();
        let config = add_folder(&store, "/roms", true).unwrap();
        assert_eq!(add_folder(&store, "/roms", false).unwrap()["romFolders"].as_array().unwrap().len(), 1);
        let id = config["romFolders"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(toggle_subfolders(&store, &id, false).unwrap()["romFolders"][0]["scanSubfolders"], false);
        add_folder(&store, "/more", false).unwrap();
        // A path another folder already has is refused.
        assert_eq!(change_folder(&store, &id, "/more").unwrap()["romFolders"][0]["path"], "/roms");
        assert_eq!(remove_folder(&store, &id).unwrap()["romFolders"].as_array().unwrap().len(), 1);
        assert_eq!(remove(&store).unwrap()["romFolders"], json!([]));
    }

    #[test]
    fn picks_the_newest_stable_version_and_disc_labels() {
        let listing = r#"<a href="/stable/1.9.0/">1.9.0</a><a href="1.21.0/">1.21.0</a><a href="1.10.3/">"#;
        assert_eq!(newest_version(listing).as_deref(), Some("1.21.0"));
        let discs = rom_disc_list(vec![("/b".into(), "b.gba".into()), ("/a".into(), "A.gba".into())]);
        assert_eq!((discs[0]["path"].as_str(), discs[0]["label"].as_str()), (Some("/a"), Some("A")));
    }
}
