//! PS1/PS2 memory card save channels (port of `events/emulators/scan-ps*-memcards.ts`,
//! `ps*-memcard-saves.ts`, `export-ps*-save.ts`, `inspect-memcard.ts`,
//! `memcard-local-assets.ts` and `services/emulators/ps*-memcard-dirs.ts`).
//! Records live in Electron's `ps1MemoryCardSaves`/`ps2MemoryCardSaves` sublevels,
//! keyed `<card path>::<folder>`. Card files are only read, never changed.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use regex::Regex;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::DialogExt;

use crate::classics_import::ini_candidates;
use crate::memory_card::{self, Save};
use crate::rom_scan::lookup_key;
use crate::store::Store;

#[derive(Clone, Copy, PartialEq)]
pub enum Platform {
    Ps1,
    Ps2,
}

impl Platform {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ps1" => Some(Self::Ps1),
            "ps2" => Some(Self::Ps2),
            _ => None,
        }
    }
    fn name(self) -> &'static str {
        if self == Self::Ps1 { "ps1" } else { "ps2" }
    }
    fn sublevel(self) -> &'static str {
        if self == Self::Ps1 { "ps1MemoryCardSaves" } else { "ps2MemoryCardSaves" }
    }
    fn extensions(self) -> &'static [&'static str] {
        if self == Self::Ps1 { &["mcd", "mcr", "mc", "gme", "vgs", "vmp"] } else { &["ps2", "mcd", "mc2"] }
    }
    /// Manual folders also accept `.ps1` (PS1) cards, like Electron's MEMCARD_FILE_RE.
    fn manual_extensions(self) -> &'static [&'static str] {
        if self == Self::Ps1 { &["mcd", "mcr", "mc", "gme", "vgs", "vmp", "ps1"] } else { &["ps2", "mcd", "mc2"] }
    }
    fn list(self, path: &Path) -> Option<Vec<Save>> {
        if self == Self::Ps1 { memory_card::list_ps1(path) } else { memory_card::list_ps2(path) }
    }
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension().is_some_and(|e| extensions.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

fn files_in(dir: &Path, extensions: &[&str]) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| has_extension(p, extensions))
        .collect()
}

static SECTION_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\[(.+?)\]\s*$").unwrap());

/// The memory card folder override: DuckStation `[MemoryCards] Directory`, PCSX2 `[Folders] MemoryCards`.
fn ini_override(ini: &Path, root: &Path, section: &str, key: &str) -> Option<PathBuf> {
    let content = std::fs::read_to_string(ini).ok()?;
    let key_re = Regex::new(&format!(r"(?i)^\s*{key}\s*=\s*(.+?)\s*$")).ok()?;
    let mut inside = false;
    for line in content.lines() {
        if let Some(c) = SECTION_RE.captures(line) {
            inside = c[1].eq_ignore_ascii_case(section);
        } else if let Some(m) = key_re.captures(line).filter(|_| inside) {
            let value = m[1].trim();
            return (!value.is_empty()).then(|| if Path::new(value).is_absolute() { PathBuf::from(value) } else { root.join(value) });
        }
    }
    None
}

/// resolvePs1MemcardDirs / resolvePs2MemcardDirs: the ini override first, then the
/// `memcards/` folders beside the config roots (and beside DuckStation's exe).
fn memcard_dirs(platform: Platform, executable: Option<&str>) -> Vec<PathBuf> {
    let system = platform.name();
    let inis = ini_candidates(system, executable);
    let root_of = |ini: &Path| -> PathBuf {
        let parent = ini.parent().unwrap_or(ini);
        // PCSX2.ini sits in `inis/`; the data root is one level up.
        if platform == Platform::Ps2 { parent.parent().unwrap_or(parent).to_path_buf() } else { parent.to_path_buf() }
    };
    let mut dirs = Vec::new();
    if let Some(ini) = inis.iter().find(|p| p.exists()) {
        let (section, key) = if platform == Platform::Ps1 { ("MemoryCards", "Directory") } else { ("Folders", "MemoryCards") };
        dirs.extend(ini_override(ini, &root_of(ini), section, key));
    }
    dirs.extend(inis.iter().map(|ini| root_of(ini).join("memcards")));
    if let (Platform::Ps1, Some(dir)) = (platform, executable.and_then(|e| Path::new(e).parent())) {
        dirs.push(dir.join("memcards"));
    }
    let mut seen = HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()) && d.exists());
    dirs
}

fn card_files(platform: Platform, executable: Option<&str>, auto_detect: bool, manual: &[String]) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    if auto_detect {
        for dir in memcard_dirs(platform, executable) {
            files.extend(files_in(&dir, platform.extensions()));
        }
    }
    for target in manual.iter().map(PathBuf::from) {
        if target.is_dir() {
            files.extend(files_in(&target, platform.manual_extensions()));
        } else if target.exists() {
            files.push(target);
        }
    }
    let mut seen = HashSet::new();
    files.retain(|f| seen.insert(f.clone()));
    files
}

/// mapEntryToAssets.
pub fn entry_assets(entry: &Value) -> Value {
    let assets = &entry["data"]["assets"];
    json!({
        "objectId": entry["objectId"], "shop": "launchbox",
        "title": assets["title"].as_str().or(entry["data"]["title"].as_str()).unwrap_or_default(),
        "iconUrl": assets["iconUrl"], "libraryImageUrl": assets["libraryImageUrl"],
        "libraryHeroImageUrl": assets["libraryHeroImageUrl"], "logoImageUrl": assets["logoImageUrl"],
    })
}

/// buildLocalLaunchboxAssetIndex: SKU -> assets of LaunchBox games already in the library,
/// for serials the shop-details endpoint misses.
pub fn local_asset_index(store: &Store) -> HashMap<String, Value> {
    let mut sku_to_object = HashMap::new();
    for (key, details) in store.entries("gameShopCache").unwrap_or_default() {
        let Some(object_id) = details["objectId"].as_str().filter(|_| key.starts_with("launchbox:")) else { continue };
        for sku in details["skus"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            sku_to_object.insert(lookup_key(sku), object_id.to_string());
        }
    }
    if sku_to_object.is_empty() {
        return HashMap::new();
    }
    let assets: HashMap<String, Value> = store
        .entries("gameShopAssets")
        .unwrap_or_default()
        .into_iter()
        .filter(|(key, a)| key.starts_with("launchbox:") && a["objectId"].is_string())
        .map(|(_, a)| (a["objectId"].as_str().unwrap_or_default().to_string(), a))
        .collect();
    sku_to_object
        .into_iter()
        .filter_map(|(sku, object_id)| {
            let a = assets.get(&object_id)?;
            Some((sku, json!({
                "objectId": object_id, "shop": "launchbox", "title": a["title"], "iconUrl": a["iconUrl"],
                "libraryImageUrl": a["libraryImageUrl"], "libraryHeroImageUrl": a["libraryHeroImageUrl"], "logoImageUrl": a["logoImageUrl"],
            })))
        })
        .collect()
}

pub fn record_key(card: &str, folder: &str) -> String {
    format!("{card}::{folder}")
}

fn record(card: &Path, save: &Save, assets: Option<&Value>) -> Value {
    let a = assets.cloned().unwrap_or(Value::Null);
    let title = a["title"].as_str().filter(|t| !t.is_empty());
    json!({
        "cardFilePath": card, "cardLabel": card.file_name().map(|n| n.to_string_lossy()), "folderName": save.folder, "sku": save.sku,
        "objectId": a["objectId"], "shop": if assets.is_some() { json!("launchbox") } else { Value::Null }, "title": title,
        "iconUrl": a["iconUrl"], "libraryImageUrl": a["libraryImageUrl"], "libraryHeroImageUrl": a["libraryHeroImageUrl"],
        "logoImageUrl": a["logoImageUrl"], "fileCount": save.file_count, "sizeBytes": save.size,
        "createdAt": save.created_ms, "modifiedAt": save.modified_ms, "detectedAt": crate::library::now_ms(),
    })
}

/// pruneStaleRecords: drop records of rescanned cards that no longer hold the save,
/// and of cards whose file is gone.
fn prune(store: &Store, platform: Platform, detected: &HashSet<String>, cards: &HashSet<String>) -> Result<(), String> {
    for (key, rec) in store.entries(platform.sublevel())? {
        let card = rec["cardFilePath"].as_str().unwrap_or_default();
        let stale = if cards.contains(card) { !detected.contains(&key) } else { !Path::new(card).exists() };
        if stale {
            store.del(platform.sublevel(), &key)?;
        }
    }
    Ok(())
}

static SCANS: LazyLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = LazyLock::new(Default::default);

async fn run_scan(app: &AppHandle, platform: Platform, input: &Value, cancel: &AtomicBool, emit: &(dyn Fn(Value) + Sync)) -> Result<(), String> {
    let store = app.state::<Store>();
    let config = crate::emulators::get_config(&store, platform.name())?;
    let manual: Vec<String> = input["manualPaths"].as_array().into_iter().flatten().filter_map(|p| p.as_str().map(String::from)).collect();
    let files = card_files(platform, config["executablePath"].as_str(), input["autoDetect"] == true, &manual);
    let cancelled = |saves: usize| json!({ "type": "cancelled", "cardCount": files.len(), "saveCount": saves });

    let mut detected: Vec<(PathBuf, Save)> = Vec::new();
    for (i, file) in files.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            emit(cancelled(detected.len()));
            return Ok(());
        }
        emit(json!({ "type": "scan_progress", "processed": i, "total": files.len(), "currentCard": file.file_name().map(|n| n.to_string_lossy()) }));
        let path = file.clone();
        let saves = tauri::async_runtime::spawn_blocking(move || platform.list(&path)).await.map_err(|e| e.to_string())?;
        detected.extend(saves.into_iter().flatten().map(|s| (file.clone(), s)));
    }
    emit(json!({ "type": "scan_progress", "processed": files.len(), "total": files.len(), "currentCard": null }));

    let mut skus: Vec<String> = Vec::new();
    for sku in detected.iter().filter_map(|(_, s)| s.sku.clone()) {
        if !skus.contains(&sku) {
            skus.push(sku);
        }
    }
    let version = app.package_info().version.to_string();
    // Electron sends no language here; the API then answers in English.
    let lookup = crate::classics_import::fetch_shop_details(&store, &version, &skus, "en", &AtomicBool::new(false)).await;
    let local = local_asset_index(&store);

    let (mut matched, mut unmatched) = (0, 0);
    let mut keys = HashSet::new();
    for (i, (card, save)) in detected.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            emit(cancelled(detected.len()));
            return Ok(());
        }
        let assets = save.sku.as_deref().map(lookup_key).and_then(|k| lookup.get(&k).map(entry_assets).or_else(|| local.get(&k).cloned()));
        if assets.is_some() { matched += 1 } else { unmatched += 1 }
        let key = record_key(&card.to_string_lossy(), &save.folder);
        store.put(platform.sublevel(), &key, &record(card, save, assets.as_ref()))?;
        keys.insert(key);
        emit(json!({
            "type": "match_progress", "processed": i + 1, "total": detected.len(), "currentSave": save.folder,
            "status": if assets.is_some() { "matched" } else { "unmatched" }, "matched": matched, "unmatched": unmatched,
        }));
    }

    let cards: HashSet<String> = files.iter().map(|f| f.to_string_lossy().into_owned()).collect();
    prune(&store, platform, &keys, &cards)?;
    emit(json!({ "type": "done", "cardCount": files.len(), "saveCount": detected.len(), "matched": matched, "unmatched": unmatched }));
    Ok(())
}

/// scanPs1Memcards / scanPs2Memcards: answers `{ requestId }` at once; progress goes to
/// `on-ps<n>-memcard-scan-progress-<requestId>`.
pub fn start_scan(app: &AppHandle, platform: Platform, input: Value) -> Value {
    let request_id = uuid::Uuid::new_v4().to_string();
    let cancel = Arc::new(AtomicBool::new(false));
    SCANS.lock().unwrap().insert(request_id.clone(), cancel.clone());
    let (app, id) = (app.clone(), request_id.clone());
    tauri::async_runtime::spawn(async move {
        let channel = format!("on-{}-memcard-scan-progress-{id}", platform.name());
        let emit = |payload: Value| {
            let _ = app.emit(&channel, [payload]);
        };
        if let Err(message) = run_scan(&app, platform, &input, &cancel, &emit).await {
            eprintln!("[memcards] {} scan failed: {message}", platform.name());
            emit(json!({ "type": "error", "message": message }));
        }
        SCANS.lock().unwrap().remove(&id);
    });
    json!({ "requestId": request_id })
}

pub fn cancel_scan(request_id: &str) {
    if let Some(flag) = SCANS.lock().unwrap().get(request_id) {
        flag.store(true, Ordering::Relaxed);
    }
}

/// listPs*MemcardSaves: by card label, then title (folder name when unmatched).
pub fn list(store: &Store, platform: Platform) -> Result<Value, String> {
    let mut records: Vec<Value> = store.entries(platform.sublevel())?.into_iter().map(|(_, v)| v).collect();
    let text = |v: &Value, k: &str| v[k].as_str().unwrap_or_default().to_lowercase();
    let title = |v: &Value| v["title"].as_str().or(v["folderName"].as_str()).unwrap_or_default().to_lowercase();
    records.sort_by(|a, b| text(a, "cardLabel").cmp(&text(b, "cardLabel")).then_with(|| title(a).cmp(&title(b))));
    Ok(Value::Array(records))
}

/// Forget removes Chimaera's record only; the card file is untouched.
pub fn forget_save(store: &Store, platform: Platform, card: &str, folder: &str) -> Result<(), String> {
    store.del(platform.sublevel(), &record_key(card, folder))
}

pub fn forget_card(store: &Store, platform: Platform, card: &str) -> Result<(), String> {
    for (key, rec) in store.entries(platform.sublevel())? {
        if rec["cardFilePath"] == card {
            store.del(platform.sublevel(), &key)?;
        }
    }
    Ok(())
}

pub fn inspect(platform: &str, card: &str) -> &'static str {
    match Platform::parse(platform) {
        Some(Platform::Ps1) => memory_card::inspect_ps1(Path::new(card)).as_str(),
        Some(Platform::Ps2) => memory_card::inspect_ps2(Path::new(card)).as_str(),
        None => "formatted",
    }
}

/// exportPs1Save (.mcs) / exportPs2Save (.psu) through a save dialog.
pub fn export(app: &AppHandle, platform: Platform, card: &str, folder: &str, suggested: &str) -> Value {
    let (ext, label) = if platform == Platform::Ps1 { ("mcs", "PS1 Save") } else { ("psu", "PS2 Save") };
    let picked = app.dialog().file().set_file_name(format!("{suggested}.{ext}")).add_filter(label, &[ext]).blocking_save_file();
    let Some(dest) = picked.and_then(|p| p.into_path().ok()) else { return json!({ "ok": false, "error": "cancelled" }) };
    let card_path = Path::new(card);
    let bytes = if platform == Platform::Ps1 { memory_card::ps1_mcs(card_path, folder) } else { memory_card::ps2_psu(card_path, folder) };
    let Some(bytes) = bytes else { return json!({ "ok": false, "error": format!("Save \"{folder}\" not found on {card}") }) };
    match std::fs::write(&dest, &bytes) {
        Ok(()) => json!({ "ok": true, "location": dest, "sizeBytes": bytes.len() }),
        Err(e) => json!({ "ok": false, "error": e.to_string() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_card::tests::{ps1_card, temp_file};

    #[test]
    fn reads_ini_overrides() {
        let dir = temp_file("x", b"").parent().unwrap().to_path_buf();
        let ini = dir.join("settings.ini");
        std::fs::write(&ini, "[Main]\nDirectory = nope\n[MemoryCards]\nCard1Type = PerGame\nDirectory = cards\n").unwrap();
        assert_eq!(ini_override(&ini, &dir, "MemoryCards", "Directory"), Some(dir.join("cards")));
        std::fs::write(&ini, "[Folders]\nMemoryCards =  \n").unwrap();
        assert_eq!(ini_override(&ini, &dir, "Folders", "MemoryCards"), None);
    }

    #[test]
    fn collects_manual_cards_by_extension() {
        let card = temp_file("SLUS-00594.mcd", &ps1_card(0));
        let dir = card.parent().unwrap();
        std::fs::write(dir.join("old.ps1"), b"").unwrap();
        std::fs::write(dir.join("notes.txt"), b"").unwrap();
        let files = card_files(Platform::Ps1, None, false, &[dir.to_string_lossy().into_owned(), card.to_string_lossy().into_owned()]);
        assert_eq!(files.len(), 2, "folder gives .mcd and .ps1, the file itself is not repeated: {files:?}");
    }

    #[test]
    fn records_and_prunes_like_electron() {
        let store = Store::open(&temp_file("t", b"").with_file_name("t.db")).unwrap();
        let card = temp_file("shared_card_1.mcd", &ps1_card(0));
        let saves = Platform::Ps1.list(&card).unwrap();
        let assets = json!({ "objectId": "lb-1", "title": "Drakan" });
        let rec = record(&card, &saves[0], Some(&assets));
        assert_eq!((rec["shop"].as_str(), rec["title"].as_str(), rec["cardLabel"].as_str()), (Some("launchbox"), Some("Drakan"), Some("shared_card_1.mcd")));
        let unmatched = record(&card, &saves[0], None);
        assert!(unmatched["shop"].is_null() && unmatched["title"].is_null());

        let card_str = card.to_string_lossy().into_owned();
        let key = record_key(&card_str, &saves[0].folder);
        store.put("ps1MemoryCardSaves", &key, &rec).unwrap();
        store.put("ps1MemoryCardSaves", &record_key(&card_str, "GONE"), &rec).unwrap();
        store.put("ps1MemoryCardSaves", "Z:/missing.mcd::X", &json!({ "cardFilePath": "Z:/missing.mcd" })).unwrap();
        prune(&store, Platform::Ps1, &HashSet::from([key.clone()]), &HashSet::from([card_str.clone()])).unwrap();
        let left: Vec<String> = store.entries("ps1MemoryCardSaves").unwrap().into_iter().map(|(k, _)| k).collect();
        assert_eq!(left, vec![key]);

        forget_card(&store, Platform::Ps1, &card_str).unwrap();
        assert!(store.entries("ps1MemoryCardSaves").unwrap().is_empty());
    }

    #[test]
    fn indexes_local_launchbox_assets() {
        let store = Store::open(&temp_file("t", b"").with_file_name("t.db")).unwrap();
        store.put("gameShopCache", "launchbox:lb-1:en", &json!({ "objectId": "lb-1", "skus": ["SCUS-94163"] })).unwrap();
        store.put("gameShopAssets", "launchbox:lb-1", &json!({ "objectId": "lb-1", "title": "Drakan" })).unwrap();
        store.put("gameShopCache", "steam:1:en", &json!({ "objectId": "1", "skus": ["SLUS-00001"] })).unwrap();
        let index = local_asset_index(&store);
        assert_eq!(index.len(), 1);
        assert_eq!(index["SCUS94163"]["title"], "Drakan");
    }
}
