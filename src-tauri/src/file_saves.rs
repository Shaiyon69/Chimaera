//! `listLocalEmulationSaves` for file-based saves (port of the discovery half of
//! `emulation-file-saves.ts`, `ppsspp-paths.ts`, `dolphinUserDirectoryCandidates`
//! and `events/emulators/list-local-emulation-saves.ts`): PPSSPP SAVEDATA folders,
//! Dolphin GameCube `.gci` files and Wii NAND title data. Read-only.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::classics_import::{home_dir, windows_documents};
use crate::memcard_saves::{entry_assets, local_asset_index};
use crate::rom_scan::{lookup_key, param_sfo_value};
use crate::store::Store;

#[derive(Debug)]
struct Found {
    source: PathBuf,
    label: String,
    identity: String,
    sku: String,
    file_count: u64,
    size: u64,
    created: i64,
    modified: i64,
}

fn ms(t: std::io::Result<SystemTime>) -> Option<i64> {
    t.ok()?.duration_since(SystemTime::UNIX_EPOCH).ok().map(|d| d.as_millis() as i64)
}

fn created_ms(meta: &std::fs::Metadata) -> i64 {
    ms(meta.created()).or(ms(meta.modified())).unwrap_or(0)
}

/// readDirectoryStats: files, bytes, oldest creation and newest change under a folder.
fn dir_stats(dir: &Path) -> (u64, u64, i64, i64) {
    let (mut count, mut size, mut created, mut modified) = (0, 0, i64::MAX, 0);
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in std::fs::read_dir(&current).into_iter().flatten().flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                pending.push(entry.path());
            } else if let Some(meta) = entry.metadata().ok().filter(|_| kind.is_file()) {
                count += 1;
                size += meta.len();
                created = created.min(created_ms(&meta));
                modified = modified.max(ms(meta.modified()).unwrap_or(0));
            }
        }
    }
    let now = crate::library::now_ms() as i64;
    (count, size, if created == i64::MAX { now } else { created }, if modified == 0 { now } else { modified })
}

/// parseIniValue: the first `key = value` anywhere in the file, quotes stripped.
fn ini_value(content: &str, key: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        if !k.trim().eq_ignore_ascii_case(key) {
            return None;
        }
        let v = v.trim();
        let quoted = v.len() >= 2 && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')));
        Some(if quoted { v[1..v.len() - 1].trim() } else { v }.to_string())
    })
    .filter(|v| !v.is_empty())
}

fn expand_home(value: &str, base: &Path) -> PathBuf {
    let expanded = match value.strip_prefix('~') {
        Some(rest) => home_dir().join(rest.trim_start_matches(['/', '\\'])),
        None => PathBuf::from(value),
    };
    if expanded.is_absolute() { expanded } else { base.join(expanded) }
}

// ---- PPSSPP ------------------------------------------------------------------

fn ppsspp_configs(executable: &Path) -> Vec<PathBuf> {
    let dir = executable.parent().unwrap_or(executable);
    let ini = |root: PathBuf| root.join("PSP").join("SYSTEM").join("ppsspp.ini");
    let mut out = vec![ini(dir.join("memstick")), ini(dir.to_path_buf())];
    let home = home_dir();
    if cfg!(windows) {
        out.extend(windows_documents(&home).into_iter().map(|d| ini(d.join("PPSSPP"))));
    } else {
        let flatpak = ini(home.join(".var/app/org.ppsspp.PPSSPP/config/ppsspp"));
        let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
        let standard = ini(config.join("ppsspp"));
        if executable.to_string_lossy().contains("org.ppsspp.PPSSPP") { out.extend([flatpak, standard]) } else { out.extend([standard, flatpak]) }
    }
    out
}

/// ppssppSavedataDirectoryCandidates: a configured MemStickDirectory first.
fn ppsspp_savedata_dirs(executable: &Path) -> Vec<PathBuf> {
    let (mut preferred, mut all) = (Vec::new(), Vec::new());
    for config in ppsspp_configs(executable) {
        let psp = config.parent().and_then(Path::parent).unwrap_or(&config).to_path_buf();
        let conventional = psp.join("SAVEDATA");
        all.push(conventional.clone());
        if let Ok(content) = std::fs::read_to_string(&config) {
            preferred.push(conventional);
            if let Some(memstick) = ini_value(&content, "MemStickDirectory") {
                let base = config.parent().unwrap_or(&config);
                preferred.insert(0, expand_home(&memstick, base).join("PSP").join("SAVEDATA"));
            }
        }
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in preferred.into_iter().chain(all) {
        if !out.contains(&dir) {
            out.push(dir);
        }
    }
    out
}

/// readPpssppSavedataDiscId: DISC_ID, SAVEDATA_DIRECTORY or the folder name, as `ABCD12345`.
fn ppsspp_disc_id(dir: &Path) -> Option<String> {
    let sfo = std::fs::read(dir.join("PARAM.SFO")).ok()?;
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned());
    [param_sfo_value(&sfo, "DISC_ID"), param_sfo_value(&sfo, "SAVEDATA_DIRECTORY"), name].into_iter().flatten().find_map(|c| {
        let id: String = lookup_key(&c).chars().take(9).collect();
        let b = id.as_bytes();
        (b.len() == 9 && b[..4].iter().all(u8::is_ascii_uppercase) && b[4..].iter().all(u8::is_ascii_digit)).then_some(id)
    })
}

fn ppsspp_saves(executable: &Path) -> Vec<Found> {
    let mut out = Vec::new();
    for root in ppsspp_savedata_dirs(executable) {
        let label = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        for entry in std::fs::read_dir(&root).into_iter().flatten().flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())) {
            let source = entry.path();
            let Some(sku) = ppsspp_disc_id(&source) else { continue };
            let (file_count, size, created, modified) = dir_stats(&source);
            let identity = entry.file_name().to_string_lossy().into_owned();
            out.push(Found { source, label: label.clone(), identity, sku, file_count, size, created, modified });
        }
    }
    out
}

// ---- Dolphin -----------------------------------------------------------------

fn dolphin_user_dirs(executable: &Path) -> Vec<PathBuf> {
    let dir = executable.parent().unwrap_or(executable);
    let home = home_dir();
    let mut out = Vec::new();
    if dir.join("portable.txt").exists() {
        out.push(dir.join(if cfg!(target_os = "linux") { "user" } else { "User" }));
    }
    if cfg!(windows) {
        let app_data = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| home.join("AppData").join("Roaming"));
        out.push(app_data.join("Dolphin Emulator"));
    } else {
        out.extend(std::env::var_os("DOLPHIN_EMU_USERPATH").map(PathBuf::from));
        if executable.to_string_lossy().contains("org.DolphinEmu.dolphin-emu") {
            out.push(home.join(".var/app/org.DolphinEmu.dolphin-emu/data/dolphin-emu"));
        }
        out.push(home.join(".dolphin-emu"));
        out.push(std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share")).join("dolphin-emu"));
    }
    let mut seen = Vec::new();
    out.retain(|d| !seen.contains(d) && { seen.push(d.clone()); true });
    out
}

/// parseGciGameId + parseGciInternalFileName from a 0x40-byte GCI header.
fn gci_ids(header: &[u8]) -> Option<(String, String)> {
    let id = String::from_utf8_lossy(&header[..6]).to_uppercase();
    if id.len() != 6 || !id.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) {
        return None;
    }
    let raw = &header[0x08..0x28];
    let raw = &raw[..raw.iter().position(|&b| b == 0).unwrap_or(raw.len())];
    let name: String = raw.iter().map(|&b| b as char).collect::<String>().trim().chars().map(|c| if (c as u32) < 0x20 || c == '/' || c == '\\' { '_' } else { c }).take(32).collect();
    (!name.is_empty()).then_some((id, name))
}

fn gci_region(parts: &[String]) -> &'static str {
    let has = |x: &str| parts.iter().any(|p| p.eq_ignore_ascii_case(x));
    if has("USA") { "USA" } else if has("JAP") || has("JPN") { "JPN" } else if has("EUR") { "EUR" } else if has("KOR") { "KOR" } else if has("DEV") { "DEV" } else { "unknown" }
}

fn gci_slot(parts: &[String]) -> &'static str {
    let re = regex::Regex::new(r"(?i)(?:card|slot)[ _-]*b").unwrap();
    if parts.iter().any(|p| re.is_match(p)) { "B" } else { "A" }
}

fn gamecube_saves(executable: &Path) -> Vec<Found> {
    use std::io::Read;
    let mut out = Vec::new();
    for user in dolphin_user_dirs(executable) {
        let root = user.join("GC");
        let mut pending = vec![root.clone()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let (path, Ok(kind)) = (entry.path(), entry.file_type()) else { continue };
                if kind.is_dir() {
                    pending.push(path);
                    continue;
                }
                if !kind.is_file() || !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("gci")) {
                    continue;
                }
                let mut header = [0u8; 0x40];
                if std::fs::File::open(&path).and_then(|mut f| f.read_exact(&mut header)).is_err() {
                    continue;
                }
                let (Some((id, name)), Ok(meta)) = (gci_ids(&header), entry.metadata()) else { continue };
                let parts: Vec<String> = path.strip_prefix(&root).unwrap_or(&path).iter().map(|p| p.to_string_lossy().into_owned()).collect();
                let (slot, region) = (gci_slot(&parts), gci_region(&parts));
                out.push(Found {
                    label: format!("{region} · Card {slot}"),
                    identity: format!("{slot}:{region}:{id}:{name}"),
                    sku: id,
                    file_count: 1,
                    size: meta.len(),
                    created: created_ms(&meta),
                    modified: ms(meta.modified()).unwrap_or(0),
                    source: path,
                });
            }
        }
    }
    out
}

/// dolphinWiiRootCandidates: Dolphin.ini's NANDRootPath first, then `<user>/Wii`.
fn wii_roots(user: &Path) -> Vec<PathBuf> {
    let mut roots = vec![user.join("Wii")];
    if let Some(configured) = std::fs::read_to_string(user.join("Config").join("Dolphin.ini")).ok().and_then(|c| ini_value(&c, "NANDRootPath")) {
        let root = expand_home(&configured, user);
        if !roots.contains(&root) {
            roots.insert(0, root);
        }
    }
    roots
}

/// parseWiiTitleDirectory: `52534245` -> game code `RSBE`.
fn wii_game_code(dir: &str) -> Option<String> {
    if dir.len() != 8 || !dir.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let code: String = (0..4).map(|i| u8::from_str_radix(&dir[i * 2..i * 2 + 2], 16).unwrap() as char).collect::<String>().to_uppercase();
    code.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()).then_some(code)
}

/// readDolphinWiiGameId: the TMD's group id completes the 6-character id when its title id matches.
fn wii_game_id(title_path: &Path, title_id: &str, code: &str) -> String {
    let Ok(tmd) = std::fs::read(title_path.join("content").join("title.tmd")) else { return code.into() };
    if tmd.len() < 0x198 + 2 {
        return code.into();
    }
    let tmd_title: String = tmd[0x18c..0x194].iter().map(|b| format!("{b:02x}")).collect();
    let id = format!("{code}{}", String::from_utf8_lossy(&tmd[0x198..0x19a]).to_uppercase());
    let valid = id.len() == 6 && id.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    if tmd_title == title_id && valid { id } else { code.into() }
}

fn wii_saves(executable: &Path) -> Vec<Found> {
    let mut out = Vec::new();
    for user in dolphin_user_dirs(executable) {
        for root in wii_roots(&user) {
            let titles = root.join("title").join("00010000");
            for entry in std::fs::read_dir(&titles).into_iter().flatten().flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())) {
                let dir = entry.file_name().to_string_lossy().into_owned();
                let Some(code) = wii_game_code(&dir) else { continue };
                let source = entry.path().join("data");
                if !source.join("banner.bin").is_file() {
                    continue;
                }
                let title_id = format!("00010000{}", dir.to_lowercase());
                let (file_count, size, created, modified) = dir_stats(&source);
                let sku = wii_game_id(&entry.path(), &title_id, &code);
                out.push(Found { source, label: "Wii NAND".into(), identity: title_id, sku, file_count, size, created, modified });
            }
        }
    }
    out
}

fn discover(platform: &str, executable: &Path) -> Vec<Found> {
    let found = match platform {
        "psp" => ppsspp_saves(executable),
        "wii" => wii_saves(executable),
        _ => gamecube_saves(executable),
    };
    // Dedupe by source path, the last one winning like Electron's Map.
    let mut by_path: Vec<Found> = Vec::new();
    for f in found {
        by_path.retain(|o| o.source != f.source);
        by_path.push(f);
    }
    by_path
}

/// findUniqueLocalAssetBySkuPrefix: a 6-character id whose 4-character game code matches, if only one game has it.
fn unique_prefix_match(index: &HashMap<String, Value>, prefix: &str) -> Option<(String, Value)> {
    let candidates: Vec<(&String, &Value)> = index.iter().filter(|(sku, _)| sku.len() == 6 && sku.starts_with(prefix)).collect();
    let first = candidates.first()?;
    candidates.iter().all(|(_, a)| a["objectId"] == first.1["objectId"]).then(|| (first.0.clone(), first.1.clone()))
}

/// listLocalEmulationSaves for psp, gamecube and wii; other platforms answer [].
pub async fn list(app: &AppHandle, platform: &str) -> Value {
    if !matches!(platform, "psp" | "gamecube" | "wii") {
        return json!([]);
    }
    let store = app.state::<Store>();
    let system = if platform == "psp" { "psp" } else { "dolphin" };
    let Some(executable) = crate::emulators::get_config(&store, system).ok().and_then(|c| c["executablePath"].as_str().map(PathBuf::from)) else {
        return json!([]);
    };
    let platform_owned = platform.to_string();
    let Ok(found) = tauri::async_runtime::spawn_blocking(move || discover(&platform_owned, &executable)).await else { return json!([]) };

    let local = local_asset_index(&store);
    let find_local = |sku: &str| -> (String, Option<Value>) {
        let key = lookup_key(sku);
        match local.get(&key) {
            Some(a) => (key, Some(a.clone())),
            None if platform == "wii" => unique_prefix_match(&local, &key).map(|(k, a)| (k, Some(a))).unwrap_or((key, None)),
            None => (key, None),
        }
    };
    let mut unresolved: Vec<String> = Vec::new();
    for f in &found {
        if find_local(&f.sku).1.is_none() && !unresolved.contains(&f.sku) {
            unresolved.push(f.sku.clone());
        }
    }
    let version = app.package_info().version.to_string();
    let remote = crate::classics_import::fetch_shop_details(&store, &version, &unresolved, "en", &AtomicBool::new(false)).await;
    let hostname = sysinfo::System::host_name();
    let now = crate::library::now_ms();

    Value::Array(
        found
            .iter()
            .map(|f| {
                let (local_sku, local_assets) = find_local(&f.sku);
                let matched_local = local_assets.is_some();
                let assets = local_assets.or_else(|| remote.get(&lookup_key(&f.sku)).map(entry_assets)).unwrap_or(Value::Null);
                json!({
                    "cardFilePath": f.source, "cardLabel": f.label, "hostname": hostname, "folderName": f.identity,
                    "sku": if matched_local { local_sku } else { f.sku.clone() },
                    "objectId": assets["objectId"], "shop": if assets.is_null() { Value::Null } else { json!("launchbox") },
                    "title": assets["title"], "iconUrl": assets["iconUrl"], "libraryImageUrl": assets["libraryImageUrl"],
                    "libraryHeroImageUrl": assets["libraryHeroImageUrl"], "logoImageUrl": assets["logoImageUrl"],
                    "fileCount": f.file_count, "sizeBytes": f.size, "createdAt": f.created, "modifiedAt": f.modified, "detectedAt": now,
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chimaera-file-saves-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_ini_values_and_gci_headers() {
        assert_eq!(ini_value("[General]\nMemStickDirectory = \"D:/PSP\"\n", "memstickdirectory").as_deref(), Some("D:/PSP"));
        assert_eq!(ini_value("MemStickDirectory =\n", "MemStickDirectory"), None);
        let mut header = [0u8; 0x40];
        header[..6].copy_from_slice(b"gm4e01");
        header[0x08..0x08 + 12].copy_from_slice(b"Metroid/Prim");
        assert_eq!(gci_ids(&header), Some(("GM4E01".into(), "Metroid_Prim".into())));
        header[0] = b'-';
        assert_eq!(gci_ids(&header), None);
        let parts = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!((gci_slot(&parts(&["USA", "Card B", "x.gci"])), gci_region(&parts(&["USA", "Card B"]))), ("B", "USA"));
        assert_eq!((gci_slot(&parts(&["x.gci"])), gci_region(&parts(&["x.gci"]))), ("A", "unknown"));
    }

    #[test]
    fn finds_portable_ppsspp_and_dolphin_saves() {
        let dir = temp();
        let exe = dir.join("PPSSPPWindows64.exe");
        let save = dir.join("memstick").join("PSP").join("SAVEDATA").join("ULUS10041DATA00");
        std::fs::create_dir_all(&save).unwrap();
        std::fs::write(save.join("PARAM.SFO"), b"not an sfo").unwrap(); // falls back to the folder name
        std::fs::write(save.join("DATA.BIN"), [0u8; 10]).unwrap();
        std::fs::create_dir_all(dir.join("memstick").join("PSP").join("SAVEDATA").join("NOPARAM")).unwrap();
        let found = discover("psp", &exe);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].sku.as_str(), found[0].identity.as_str(), found[0].file_count, found[0].size), ("ULUS10041", "ULUS10041DATA00", 2, 20));

        std::fs::write(dir.join("portable.txt"), b"").unwrap();
        let user = dir.join(if cfg!(target_os = "linux") { "user" } else { "User" });
        let title = user.join("Wii").join("title").join("00010000").join("52534245");
        std::fs::create_dir_all(title.join("data")).unwrap();
        std::fs::write(title.join("data").join("banner.bin"), b"b").unwrap();
        let mut tmd = vec![0u8; 0x19a];
        tmd[0x18c..0x194].copy_from_slice(&[0, 1, 0, 0, 0x52, 0x53, 0x42, 0x45]);
        tmd[0x198..0x19a].copy_from_slice(b"01");
        std::fs::create_dir_all(title.join("content")).unwrap();
        std::fs::write(title.join("content").join("title.tmd"), &tmd).unwrap();
        let wii = discover("wii", &dir.join("Dolphin.exe"));
        let ours: Vec<&Found> = wii.iter().filter(|f| f.source.starts_with(&dir)).collect();
        assert_eq!(ours.len(), 1);
        assert_eq!((ours[0].sku.as_str(), ours[0].identity.as_str()), ("RSBE01", "0001000052534245"));
    }

    #[test]
    fn wii_prefix_match_needs_one_game() {
        let mut index = HashMap::new();
        index.insert("RSBE01".to_string(), json!({ "objectId": "a" }));
        assert_eq!(unique_prefix_match(&index, "RSBE").map(|m| m.0).as_deref(), Some("RSBE01"));
        index.insert("RSBE02".to_string(), json!({ "objectId": "b" }));
        assert!(unique_prefix_match(&index, "RSBE").is_none());
    }
}
