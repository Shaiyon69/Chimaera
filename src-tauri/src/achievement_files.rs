//! Where Steam emulators write achievement unlocks, and how to read each format.
//! Ported from src/main/services/achievements/ (find-achievement-files.ts,
//! parse-achievement-formats.ts, game-directory.ts, scan-*-achievement-files.ts).
// shortcut: Windows paths only; the Linux port adds the Wine prefix in front of every root.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cracker {
    Codex,
    Rune,
    OnlineFix,
    Goldberg,
    UserStats,
    Rld,
    Empress,
    Skidrow,
    CreamApi,
    SmartSteamEmu,
    ThreeDm,
    Razor1911,
    Ali213,
    /// The real Steam client's per-user library cache.
    Steam,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AchievementFile {
    pub cracker: Cracker,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Unlock {
    pub name: String,
    pub unlock_time: i64,
}

/// The Windows folders emulators write under.
pub struct Roots {
    pub app_data: PathBuf,
    pub local_app_data: PathBuf,
    pub documents: PathBuf,
    pub public_documents: PathBuf,
    pub program_data: PathBuf,
}

impl Roots {
    pub fn from_env(documents: PathBuf) -> Self {
        let env = |key: &str, fallback: &str| std::env::var_os(key).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(fallback));
        Roots {
            app_data: env("APPDATA", ""),
            local_app_data: env("LOCALAPPDATA", ""),
            documents,
            public_documents: env("PUBLIC", r"C:\Users\Public").join("Documents"),
            program_data: env("PROGRAMDATA", r"C:\ProgramData"),
        }
    }
}

/// (cracker, folder, path below it with `<id>` for the app id), per getPathFromCracker.
fn cracker_locations(r: &Roots) -> Vec<(Cracker, PathBuf, &'static [&'static str])> {
    use Cracker::*;
    vec![
        (Codex, r.public_documents.join(r"Steam\CODEX"), &["<id>", "achievements.ini"]),
        (Codex, r.app_data.join(r"Steam\CODEX"), &["<id>", "achievements.ini"]),
        (Goldberg, r.app_data.join("Goldberg SteamEmu Saves"), &["<id>", "achievements.json"]),
        (Goldberg, r.app_data.join("GSE Saves"), &["<id>", "achievements.json"]),
        (Rune, r.public_documents.join(r"Steam\RUNE"), &["<id>", "achievements.ini"]),
        (OnlineFix, r.public_documents.join("OnlineFix"), &["<id>", "Stats", "Achievements.ini"]),
        (OnlineFix, r.public_documents.join("OnlineFix"), &["<id>", "Achievements.ini"]),
        (Rld, r.program_data.join("RLD!"), &["<id>", "achievements.ini"]),
        (Rld, r.program_data.join(r"Steam\Player"), &["<id>", "stats", "achievements.ini"]),
        (Rld, r.program_data.join(r"Steam\RLD!"), &["<id>", "stats", "achievements.ini"]),
        (Rld, r.program_data.join(r"Steam\dodi"), &["<id>", "stats", "achievements.ini"]),
        (CreamApi, r.app_data.join("CreamAPI"), &["<id>", "stats", "CreamAPI.Achievements.cfg"]),
        (Skidrow, r.documents.join("SKIDROW"), &["<id>", "SteamEmu", "UserStats", "achiev.ini"]),
        (Skidrow, r.documents.join("Player"), &["<id>", "SteamEmu", "UserStats", "achiev.ini"]),
        (Skidrow, r.local_app_data.join("SKIDROW"), &["<id>", "SteamEmu", "UserStats", "achiev.ini"]),
        (SmartSteamEmu, r.app_data.join("SmartSteamEmu"), &["<id>", "User", "Achievements.ini"]),
        (Empress, r.app_data.join(r"EMPRESS\remote"), &["<id>", "achievements.json"]),
        (Empress, r.public_documents.join("EMPRESS"), &["<id>", "remote", "<id>", "achievements.json"]),
        (Razor1911, r.app_data.join(".1911"), &["<id>", "achievement"]),
    ]
}

/// Dishonored ships under several app ids.
pub fn alternative_ids(object_id: &str) -> Vec<&str> {
    if object_id == "205100" {
        vec!["205100", "217980", "31292"]
    } else {
        vec![object_id]
    }
}

/// Files at the fixed per-emulator locations (findAchievementFiles).
fn fixed_location_files(roots: &Roots, object_id: &str) -> Vec<AchievementFile> {
    let mut files = Vec::new();
    for (cracker, folder, location) in cracker_locations(roots) {
        for id in alternative_ids(object_id) {
            let path = location.iter().fold(folder.clone(), |p, part| p.join(part.replace("<id>", id)));
            if path.is_file() {
                files.push(AchievementFile { cracker, path });
            }
        }
    }
    files
}

const ACHIEVEMENTS_JSON: &str = "achievements.json";

fn find_named_file(dir: &Path, lower_name: &str) -> Option<PathBuf> {
    fs::read_dir(dir).ok()?.flatten().find(|e| e.file_type().is_ok_and(|t| !t.is_dir()) && e.file_name().to_string_lossy().to_lowercase() == lower_name).map(|e| e.path())
}

/// Goldberg saves can sit one folder deeper (scanObjectIdFolder): `<id>/achievements.json` or `<id>/<any>/achievements.json`.
fn nested_goldberg_files(roots: &Roots, object_id: &str) -> Vec<AchievementFile> {
    let mut files = Vec::new();
    for save_folder in [roots.app_data.join("Goldberg SteamEmu Saves"), roots.app_data.join("GSE Saves")] {
        for id in alternative_ids(object_id) {
            let dir = save_folder.join(id);
            files.extend(find_named_file(&dir, ACHIEVEMENTS_JSON));
            for sub in fs::read_dir(&dir).into_iter().flatten().flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())) {
                files.extend(find_named_file(&sub.path(), ACHIEVEMENTS_JSON));
            }
        }
    }
    files.into_iter().map(|path| AchievementFile { cracker: Cracker::Goldberg, path }).collect()
}

const UNSAFE_ROOTS: &[&str] = &[
    "program files", "program files (x86)", "users", "windows", "system32", "appdata", "programdata", "steamapps", "common", "desktop",
    "documents", "downloads",
];
const NESTED_EXECUTABLE_DIRS: &[&str] = &["bin", "bin32", "bin64", "binaries", "win32", "win64", "x64", "x86", "game", "runtime"];
const GAME_ROOT_MARKER_DIRS: &[&str] = &["engine", "binaries", "content", "plugins"];
const GAME_ROOT_MARKER_FILES: &[&str] = &["steam_api.dll", "steam_api64.dll", "steam_appid.txt"];
const STEAM_SETTINGS: &str = "steam_settings";

fn lower_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// isUnsafePath (find-game-root.ts): a drive root, a folder right under one, or a shared system folder.
fn is_unsafe(dir: &Path) -> bool {
    UNSAFE_ROOTS.contains(&lower_name(dir).as_str()) || dir.parent().is_none_or(|p| p.parent().is_none())
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir).into_iter().flatten().flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).collect()
}

fn has_root_marker(dir: &Path) -> bool {
    fs::read_dir(dir).into_iter().flatten().flatten().any(|e| {
        let name = e.file_name().to_string_lossy().to_lowercase();
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            GAME_ROOT_MARKER_DIRS.contains(&name.as_str()) || name.ends_with("_data") || name == STEAM_SETTINGS
        } else {
            GAME_ROOT_MARKER_FILES.contains(&name.as_str())
        }
    })
}

/// resolveGameSearchRoot: climb at most 3 levels while the parent still looks like the game.
fn search_root(exe: &Path) -> PathBuf {
    let mut root = exe.parent().unwrap_or(exe).to_path_buf();
    for _ in 0..3 {
        let Some(parent) = root.parent().map(Path::to_path_buf) else { break };
        if is_unsafe(&parent) || !parent.is_dir() {
            break;
        }
        if !NESTED_EXECUTABLE_DIRS.contains(&lower_name(&root).as_str()) && !has_root_marker(&parent) {
            break;
        }
        root = parent;
    }
    root
}

/// collectEmulatorDirectories: folders an emulator dll may sit in, around the game's exe.
fn emulator_dirs(exe: &Path) -> Vec<PathBuf> {
    let exe_dir = exe.parent().unwrap_or(exe).to_path_buf();
    if is_unsafe(&exe_dir) {
        return Vec::new();
    }
    let root = search_root(exe);
    let mut dirs = vec![exe_dir, root.clone()];
    for sub in subdirs(&root) {
        let name = lower_name(&sub);
        if NESTED_EXECUTABLE_DIRS.contains(&name.as_str()) {
            dirs.push(sub.clone());
        }
        for arch in ["Win64", "Win32", "WinGDK"] {
            dirs.push(sub.join("Binaries").join(arch));
        }
        if name.ends_with("_data") {
            for arch in ["x86_64", "x86"] {
                dirs.push(sub.join("Plugins").join(arch));
            }
        }
    }
    for version in subdirs(&root.join(r"Engine\Binaries\ThirdParty\Steamworks")) {
        for arch in ["Win64", "Win32", "WinGDK"] {
            dirs.push(version.join(arch));
        }
    }
    let mut seen = HashSet::new();
    dirs.retain(|d| seen.insert(d.to_string_lossy().to_lowercase()));
    dirs
}

/// canonicalize without Windows' `\\?\` prefix, so paths read like the rest.
pub(crate) fn real_path(path: &Path) -> Option<PathBuf> {
    let real = fs::canonicalize(path).ok()?;
    Some(real.to_string_lossy().strip_prefix(r"\\?\").map(PathBuf::from).unwrap_or(real))
}

const IGNORED_DIRS: &[&str] = &[
    ".git", "audio", "config", "configs", "content", "data", "intermediate", "localization", "logs", "movies", "music", "pak", "paks", "saved",
    "saves", "shaders", "sound", "sounds", "textures", "videos",
];

/// findSteamSettingsDirectories: `steam_settings` next to an emulator dll, else the
/// shallowest ones under the game root (8 levels, 2000 folders at most). Symlinks
/// that leave the game root are ignored.
fn steam_settings_dirs(exe: &Path) -> Vec<PathBuf> {
    if is_unsafe(exe.parent().unwrap_or(exe)) {
        return Vec::new();
    }
    let Some(containment) = real_path(&search_root(exe)) else { return Vec::new() };
    let contained = |candidate: &Path| real_path(candidate).filter(|r| r.starts_with(&containment) && r.is_dir());
    let mut found: Vec<PathBuf> = emulator_dirs(exe)
        .iter()
        .flat_map(|d| [d.join(STEAM_SETTINGS), d.join("coldclient").join(STEAM_SETTINGS)])
        .filter_map(|c| contained(&c))
        .collect();
    if found.is_empty() {
        let (mut level, mut visited) = (vec![containment.clone()], 0);
        for _ in 0..8 {
            let mut next = Vec::new();
            for dir in &level {
                if visited >= 2000 {
                    break;
                }
                visited += 1;
                for sub in subdirs(dir) {
                    let name = lower_name(&sub);
                    if name == STEAM_SETTINGS {
                        found.extend(contained(&sub));
                    } else if !IGNORED_DIRS.contains(&name.as_str()) {
                        next.push(sub);
                    }
                }
            }
            if !found.is_empty() || next.is_empty() {
                break;
            }
            level = next;
        }
    }
    found.sort();
    found.dedup();
    found
}

/// Files inside the game folder: user_stats.ini, 3DM and ALI213 profiles, and Goldberg's steam_settings.
fn game_directory_files(exe: &Path) -> Vec<AchievementFile> {
    let mut files = Vec::new();
    for base in emulator_dirs(exe) {
        let user_stats = base.join(r"SteamData\user_stats.ini");
        if user_stats.is_file() {
            files.push(AchievementFile { cracker: Cracker::UserStats, path: user_stats });
        }
        for profile in subdirs(&base.join("3DMGAME")) {
            let path = profile.join(r"stats\achievements.ini");
            if path.is_file() {
                files.push(AchievementFile { cracker: Cracker::ThreeDm, path });
            }
        }
        for profile in subdirs(&base.join("Profile")) {
            let path = profile.join(r"Stats\Achievements.Bin");
            if path.is_file() {
                files.push(AchievementFile { cracker: Cracker::Ali213, path });
            }
        }
    }
    for settings in steam_settings_dirs(exe) {
        for app in subdirs(&settings).into_iter().filter(|d| lower_name(d).chars().all(|c| c.is_ascii_digit())) {
            let path = app.join(ACHIEVEMENTS_JSON);
            if path.is_file() {
                files.push(AchievementFile { cracker: Cracker::Goldberg, path });
            }
        }
    }
    files
}

/// The real Steam client's caches of the user's unlocks (findAchievementFileInSteamPath).
/// `librarycache/<appid>.json` lists at most 12 unlocks, so the full per-account stats in
/// `appcache/stats/UserGameStats_<account>_<appid>.bin` are read too.
fn steam_cache_files(steam_path: &Path, object_id: &str) -> Vec<AchievementFile> {
    let stats = steam_path.join(r"appcache\stats");
    subdirs(&steam_path.join("userdata"))
        .into_iter()
        .flat_map(|user| {
            let account = user.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            [user.join(r"config\librarycache").join(format!("{object_id}.json")), stats.join(format!("UserGameStats_{account}_{object_id}.bin"))]
        })
        .filter(|p| p.is_file())
        .map(|path| AchievementFile { cracker: Cracker::Steam, path })
        .collect()
}

/// UserGameStats_<account>_<appid>.bin: binary VDF `cache/<statId>/data` is a bitmask whose
/// bit n is the achievement at `stats/<statId>/bits/<n>/name` in UserGameStatsSchema_<appid>.bin,
/// with its unlock time at `cache/<statId>/AchievementTimes/<n>`.
fn process_steam_stats(path: &Path) -> Vec<Unlock> {
    use crate::steam_shortcuts::{parse_map, Entries, Node};
    fn read(path: &Path) -> Option<Entries> {
        parse_map(&fs::read(path).ok()?, &mut 0).ok()
    }
    fn get<'a>(map: &'a Entries, key: &str) -> Option<&'a Entries> {
        map.iter().find(|(k, _)| k == key).and_then(|(_, v)| if let Node::Map(m) = v { Some(m) } else { None })
    }
    fn int(map: &Entries, key: &str) -> Option<i64> {
        map.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
            Node::Fixed(0x02, b) => Some(i32::from_le_bytes(b[..4].try_into().ok()?) as i64),
            _ => None,
        })
    }
    let Some(app_id) = path.file_stem().and_then(|s| s.to_str()).and_then(|s| s.rsplit('_').next()) else { return Vec::new() };
    let (Some(user), Some(schema)) = (read(path), path.parent().and_then(|dir| read(&dir.join(format!("UserGameStatsSchema_{app_id}.bin"))))) else {
        return Vec::new();
    };
    let (Some(cache), Some((_, Node::Map(root)))) = (get(&user, "cache"), schema.first()) else { return Vec::new() };
    let Some(stats) = get(root, "stats") else { return Vec::new() };
    let mut unlocks = Vec::new();
    for (stat_id, stat) in stats {
        let (Node::Map(stat), Some(own)) = (stat, get(cache, stat_id)) else { continue };
        let mask = int(own, "data").unwrap_or(0) as u32;
        let times = get(own, "AchievementTimes");
        for (key, bit) in get(stat, "bits").into_iter().flatten() {
            let Node::Map(bit) = bit else { continue };
            let n = int(bit, "bit").or_else(|| key.parse().ok()).unwrap_or(-1);
            let name = bit.iter().find(|(k, _)| k == "name").and_then(|(_, v)| if let Node::Str(s) = v { Some(s.as_str()) } else { None });
            if let (Some(name), 0..=31) = (name, n) {
                if mask >> n & 1 == 1 {
                    unlocks.push(unlock(name, times.and_then(|t| int(t, &n.to_string())).unwrap_or(0) * 1000));
                }
            }
        }
    }
    unlocks
}

/// Every achievement file for a Steam-catalogue game (collectGameAchievementFiles).
pub fn game_files(roots: &Roots, object_id: &str, executables: &[String], steam_path: Option<&Path>) -> Vec<AchievementFile> {
    let mut files = fixed_location_files(roots, object_id);
    files.extend(nested_goldberg_files(roots, object_id));
    for exe in executables.iter().filter(|e| !e.is_empty()) {
        files.extend(game_directory_files(Path::new(exe)));
    }
    if let Some(steam) = steam_path {
        files.extend(steam_cache_files(steam, object_id));
    }
    let mut seen = HashSet::new();
    files.retain(|f| seen.insert((f.cracker, f.path.to_string_lossy().to_lowercase())));
    files
}

// ---- parsers -------------------------------------------------------------

type Ini = BTreeMap<String, BTreeMap<String, String>>;

fn read_text(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Some(text.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(text))
}

/// iniParse: `[section]` headers, `key=value` lines, `#`/`;` comments.
pub fn parse_ini(text: &str) -> Ini {
    let mut ini = Ini::new();
    let mut section = String::new();
    for line in text.split(['\r', '\n']).map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with(';')) {
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].to_string();
            ini.insert(section.clone(), BTreeMap::new());
            continue;
        }
        let (key, value) = line.split_once('=').unwrap_or((line, ""));
        ini.entry(section.clone()).or_default().insert(key.trim().to_string(), value.trim().to_string());
    }
    ini
}

/// JS `Number(x) * 1000`, with unparsable values as 0.
fn seconds_to_ms(value: &str) -> i64 {
    (value.trim().parse::<f64>().unwrap_or(0.0) * 1000.0) as i64
}

/// parseUnlockTime: some emulators write 7-digit timestamps (units of 1e6 ms).
fn unlock_time(value: &str) -> i64 {
    if value.len() == 7 {
        (value.parse::<f64>().unwrap_or(0.0) * 1_000_000.0) as i64
    } else {
        seconds_to_ms(value)
    }
}

/// First four bytes of a hex string as a little-endian u32 (the DataView reads in processRld/process3DM).
fn hex_le_u32(hex: &str) -> Option<u32> {
    let bytes: Vec<u8> = (0..hex.len().min(8)).step_by(2).filter_map(|i| hex.get(i..i + 2)).map(|b| u8::from_str_radix(b, 16).ok()).collect::<Option<_>>()?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn unlock(name: &str, unlock_time: i64) -> Unlock {
    Unlock { name: name.to_string(), unlock_time }
}

fn process_default(ini: &Ini, achieved: &str, time: &str) -> Vec<Unlock> {
    ini.iter()
        .filter(|(_, s)| s.get(achieved).map(String::as_str) == Some("1"))
        .map(|(name, s)| unlock(name, seconds_to_ms(s.get(time).map_or("", String::as_str))))
        .collect()
}

fn process_online_fix(ini: &Ini) -> Vec<Unlock> {
    ini.iter()
        .filter_map(|(name, s)| {
            let get = |k: &str| s.get(k).map(String::as_str);
            if get("achieved") == Some("true") {
                Some(unlock(name, seconds_to_ms(get("timestamp").unwrap_or_default())))
            } else if get("Achieved") == Some("true") {
                Some(unlock(name, unlock_time(get("TimeUnlocked").unwrap_or_default())))
            } else {
                None
            }
        })
        .collect()
}

fn process_cream_api(ini: &Ini) -> Vec<Unlock> {
    ini.iter()
        .filter(|(_, s)| s.get("achieved").map(String::as_str) == Some("true"))
        .map(|(name, s)| unlock(name, unlock_time(s.get("unlocktime").map_or("", String::as_str))))
        .collect()
}

fn process_skidrow(ini: &Ini) -> Vec<Unlock> {
    ini.get("Achievements")
        .into_iter()
        .flatten()
        .filter_map(|(name, value)| {
            let parts: Vec<&str> = value.split('@').collect();
            (parts[0] == "1").then(|| unlock(name, seconds_to_ms(parts.last().unwrap_or(&""))))
        })
        .collect()
}

fn process_3dm(ini: &Ini) -> Vec<Unlock> {
    let (Some(states), Some(times)) = (ini.get("State"), ini.get("Time")) else { return Vec::new() };
    states
        .iter()
        .filter(|(_, state)| *state == "0101")
        .filter_map(|(name, _)| Some(unlock(name, i64::from(hex_le_u32(times.get(name)?)?) * 1000)))
        .collect()
}

fn process_rld(ini: &Ini) -> Vec<Unlock> {
    ini.iter()
        .filter(|(name, _)| *name != "Steam")
        .filter_map(|(name, s)| {
            (hex_le_u32(s.get("State")?)? == 1).then_some(())?;
            Some(unlock(name, i64::from(hex_le_u32(s.get("Time")?)?) * 1000))
        })
        .collect()
}

fn process_user_stats(ini: &Ini) -> Vec<Unlock> {
    ini.get("ACHIEVEMENTS")
        .into_iter()
        .flatten()
        .filter_map(|(name, value)| {
            let lower = value.to_lowercase();
            let compact: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
            if !compact.contains("unlocked=true") {
                return None;
            }
            // `time = <digits>` at the start or after `{`, `,` or whitespace.
            let time = lower.match_indices("time").find_map(|(i, _)| {
                let before = lower[..i].chars().last();
                if before.is_some_and(|c| !matches!(c, '{' | ',') && !c.is_whitespace()) {
                    return None;
                }
                let rest = lower[i + 4..].trim_start().strip_prefix('=')?.trim_start();
                let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                (!digits.is_empty()).then_some(digits)
            })?;
            Some(unlock(&name.replace('"', ""), seconds_to_ms(&time)))
        })
        .collect()
}

fn process_goldberg(json: &Value) -> Vec<Unlock> {
    let earned = |a: &Value| a["earned"].as_bool().unwrap_or(false) || a["earned"].as_i64().is_some_and(|n| n != 0);
    let time = |a: &Value| (a["earned_time"].as_f64().unwrap_or(0.0) * 1000.0) as i64;
    match json {
        Value::Array(list) => list.iter().filter(|a| earned(a)).filter_map(|a| Some(unlock(a["name"].as_str()?, time(a)))).collect(),
        Value::Object(map) => map.iter().filter(|(_, a)| earned(a)).map(|(name, a)| unlock(name, time(a))).collect(),
        _ => Vec::new(),
    }
}

fn process_razor1911(text: &str) -> Vec<Unlock> {
    text.split(['\r', '\n'])
        .filter_map(|line| {
            let mut parts = line.split(' ');
            let (name, unlocked, time) = (parts.next()?, parts.next()?, parts.next().unwrap_or_default());
            (unlocked == "1").then(|| unlock(name, seconds_to_ms(time)))
        })
        .collect()
}

/// librarycache/<appid>.json: `[["achievements", { data: { vecHighlight: [...] } }], ...]`.
fn process_steam_cache(json: &Value) -> Vec<Unlock> {
    let Some(entry) = json.as_array().and_then(|list| list.iter().find(|e| e[0] == "achievements")) else { return Vec::new() };
    entry[1]["data"]["vecHighlight"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["bAchieved"].as_bool().unwrap_or(false))
        .filter_map(|a| Some(unlock(a["strID"].as_str()?, a["rtUnlocked"].as_i64().unwrap_or(0) * 1000)))
        .collect()
}

/// parseAchievementFile: the unlocks in one file, or none when it is missing or unreadable.
pub fn parse_file(file: &AchievementFile) -> Vec<Unlock> {
    use Cracker::*;
    let Some(text) = read_text(&file.path) else { return Vec::new() };
    let json = || serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
    let unlocks = match file.cracker {
        Codex | Rune => process_default(&parse_ini(&text), "Achieved", "UnlockTime"),
        Ali213 => process_default(&parse_ini(&text), "HaveAchieved", "HaveAchievedTime"),
        OnlineFix => process_online_fix(&parse_ini(&text)),
        CreamApi => process_cream_api(&parse_ini(&text)),
        Skidrow => process_skidrow(&parse_ini(&text)),
        ThreeDm => process_3dm(&parse_ini(&text)),
        Rld => process_rld(&parse_ini(&text)),
        UserStats => process_user_stats(&parse_ini(&text)),
        Goldberg | Empress => process_goldberg(&json()),
        Razor1911 => process_razor1911(&text),
        Steam if file.path.extension().is_some_and(|e| e == "bin") => process_steam_stats(&file.path),
        Steam => process_steam_cache(&json()),
        // Electron finds these files but has no parser for them either.
        SmartSteamEmu => Vec::new(),
    };
    unlocks.into_iter().filter(|u| !u.name.trim().is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn names(unlocks: &[Unlock]) -> Vec<(&str, i64)> {
        unlocks.iter().map(|u| (u.name.as_str(), u.unlock_time)).collect()
    }

    #[test]
    fn parses_codex_ini_with_bom_and_comments() {
        let ini = parse_ini("\u{feff}; header\n[ACH_1]\nAchieved=1\nUnlockTime=1700000000\n[ACH_2]\nAchieved=0\n[SteamAchievements]\nCount=2");
        let mut got = process_default(&ini, "Achieved", "UnlockTime");
        got.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(names(&got), [("ACH_1", 1_700_000_000_000)]);
    }

    #[test]
    fn parses_goldberg_object_and_array_forms() {
        let object = json!({ "A": { "earned": true, "earned_time": 10 }, "B": { "earned": false, "earned_time": 0 } });
        assert_eq!(names(&process_goldberg(&object)), [("A", 10_000)]);
        let array = json!([{ "name": "C", "earned": true, "earned_time": 5 }, { "name": "D", "earned": false }]);
        assert_eq!(names(&process_goldberg(&array)), [("C", 5_000)]);
    }

    #[test]
    fn parses_online_fix_both_casings_and_short_timestamps() {
        let ini = parse_ini("[a]\nachieved=true\ntimestamp=100\n[b]\nAchieved=true\nTimeUnlocked=1700000\n[c]\nAchieved=false");
        assert_eq!(names(&process_online_fix(&ini)), [("a", 100_000), ("b", 1_700_000_000_000)]);
    }

    #[test]
    fn parses_hex_little_endian_formats() {
        // 0x64 = 100 seconds.
        let rld = parse_ini("[Steam]\nState=01000000\n[WIN]\nState=01000000\nTime=64000000\n[LOSS]\nState=00000000\nTime=64000000");
        assert_eq!(names(&process_rld(&rld)), [("WIN", 100_000)]);
        let three = parse_ini("[State]\nX=0101\nY=0000\n[Time]\nX=64000000\nY=64000000");
        assert_eq!(names(&process_3dm(&three)), [("X", 100_000)]);
    }

    #[test]
    fn parses_steam_client_stats_bitmask() {
        // Binary VDF: 0x00 map, 0x01 string, 0x02 int32, 0x08 end.
        fn map(name: &str, body: &[u8]) -> Vec<u8> {
            [&[0u8][..], name.as_bytes(), &[0], body, &[8]].concat()
        }
        fn s(name: &str, v: &str) -> Vec<u8> {
            [&[1u8][..], name.as_bytes(), &[0], v.as_bytes(), &[0]].concat()
        }
        fn i(name: &str, v: i32) -> Vec<u8> {
            [&[2u8][..], name.as_bytes(), &[0], &v.to_le_bytes()].concat()
        }
        let dir = std::env::temp_dir().join(format!("chimaera-stats-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let bits = [map("0", &s("name", "A")), map("1", &[s("name", "B"), i("bit", 1)].concat()), map("2", &s("name", "C"))].concat();
        let schema = map("620", &map("stats", &map("1", &map("bits", &bits))));
        fs::write(dir.join("UserGameStatsSchema_620.bin"), [schema, vec![8]].concat()).unwrap();
        let own = map("1", &[i("data", 0b101), map("AchievementTimes", &[i("0", 7), i("2", 9)].concat())].concat());
        fs::write(dir.join("UserGameStats_42_620.bin"), [map("cache", &own), vec![8]].concat()).unwrap();
        let file = AchievementFile { cracker: Cracker::Steam, path: dir.join("UserGameStats_42_620.bin") };
        assert_eq!(names(&parse_file(&file)), [("A", 7_000), ("C", 9_000)]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parses_skidrow_razor_user_stats_and_steam_cache() {
        assert_eq!(names(&process_skidrow(&parse_ini("[Achievements]\nA=1@0@50\nB=0@0@0"))), [("A", 50_000)]);
        assert_eq!(names(&process_razor1911("A 1 7\nB 0 9\n")), [("A", 7_000)]);
        let stats = parse_ini("[ACHIEVEMENTS]\n\"WIN\" = {unlocked = true, time = 42}\n\"LOSE\" = {unlocked = false, time = 1}");
        assert_eq!(names(&process_user_stats(&stats)), [("WIN", 42_000)]);
        let cache = json!([["friends", {}], ["achievements", { "data": { "vecHighlight": [
            { "strID": "S1", "bAchieved": true, "rtUnlocked": 3 }, { "strID": "S2", "bAchieved": false }
        ] } }]]);
        assert_eq!(names(&process_steam_cache(&cache)), [("S1", 3_000)]);
    }

    #[test]
    fn finds_fixed_nested_and_game_folder_files() {
        let base = std::env::temp_dir().join(format!("chimaera-ach-{}", uuid::Uuid::new_v4()));
        let roots = Roots {
            app_data: base.join("roaming"),
            local_app_data: base.join("local"),
            documents: base.join("docs"),
            public_documents: base.join("public"),
            program_data: base.join("programdata"),
        };
        let codex = roots.public_documents.join(r"Steam\CODEX\620");
        fs::create_dir_all(&codex).unwrap();
        fs::write(codex.join("achievements.ini"), "").unwrap();
        let nested = roots.app_data.join(r"GSE Saves\620\profile");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("Achievements.json"), "{}").unwrap();
        let game = base.join(r"games\Portal 2");
        fs::create_dir_all(game.join(r"steam_settings\620")).unwrap();
        fs::write(game.join(r"steam_settings\620\achievements.json"), "{}").unwrap();
        fs::write(game.join("steam_api64.dll"), "").unwrap();
        fs::write(game.join("portal2.exe"), "").unwrap();

        let exe = game.join("portal2.exe");
        let files = game_files(&roots, "620", &[exe.to_string_lossy().into_owned()], None);
        let kinds: Vec<Cracker> = files.iter().map(|f| f.cracker).collect();
        assert_eq!(kinds, [Cracker::Codex, Cracker::Goldberg, Cracker::Goldberg], "{files:?}");
        assert!(files[2].path.ends_with(r"steam_settings\620\achievements.json"));
        fs::remove_dir_all(&base).unwrap();
    }
}
