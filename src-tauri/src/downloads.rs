//! Download queue and torrent transfers, ported from src/main/services/download-orchestrator.ts,
//! download/download-manager.ts, download-layout-state.ts and the torrenting channels that drive
//! them. Torrents go to the libtorrent engine (`torrent`); file hosters and debrid services
//! (`hosters`, `debrid`) to an HTTP `Batch`.
//!
//! Records live in the `downloads` sublevel (`<shop>:<objectId>`), the queue and paused order in
//! `downloadLayoutState/layout`. One download holds the transfer slot at a time.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tauri::window::{ProgressBarState, ProgressBarStatus};
use tauri::{AppHandle, Emitter, Manager};

use crate::games::{self, game_key};
use crate::library::now_ms;
use crate::store::Store;
use crate::debrid::{self, Entry, Link};
use crate::http_download::{self, Batch, State};
use crate::{hosters, torrent, trackers};

pub const TORRENT: i64 = 1;
const ENGINE_CHECKING_FILES: i64 = 1;
const ENGINE_DOWNLOADING_METADATA: i64 = 2;
const ENGINE_CHECKING_RESUME_DATA: i64 = 7;
const WATCH_INTERVAL: Duration = Duration::from_secs(2);
const SEED_SIZE_CHECK_INTERVAL_MS: u64 = 60_000;
const TORRENT_START_TIMEOUT_MS: u64 = 10_000;
const SELECTIVE_TORRENT_START_TIMEOUT_MS: u64 = 60_000;
const SELECTIVE_TORRENT_METADATA_TIMEOUT_MS: u64 = 60_000;
const ENGINE_CALL_TIMEOUT_MS: u64 = 10_000;
const NOT_PORTED: &str = "This download host is not supported in the Tauri build yet.";

#[derive(Default)]
struct Slot {
    /// `downloadingGameId`: the download the engine is transferring now.
    active: Option<String>,
    /// Bumped by pause and cancel, so a slow start that returns afterwards knows it is stale.
    generation: u64,
    /// A slot whose record vanished; cancelled when it is still missing on the next tick.
    orphan: Option<(String, u64)>,
    seed_empty_sent: bool,
    seed_checked: HashMap<String, u64>,
    /// The HTTP download running in the slot, by download key.
    http: Option<(String, Arc<Batch>)>,
    /// The speed limit for new HTTP downloads, in bytes per second.
    limit: Option<u64>,
}

#[derive(Default)]
pub struct Downloads {
    /// `withDownloadActivation`: queue completion, starts and manual actions claim the slot one at a time.
    activation: tokio::sync::Mutex<()>,
    slot: Mutex<Slot>,
}

impl Downloads {
    fn slot(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }
}

// ---- Download record helpers (src/types/download-contract.ts) ----

fn id(d: &Value) -> String {
    game_key(d["shop"].as_str().unwrap_or_default(), d["objectId"].as_str().unwrap_or_default())
}

fn status(d: &Value) -> &str {
    d["status"].as_str().unwrap_or_default()
}

fn flag(d: &Value, key: &str) -> bool {
    d[key].as_bool().unwrap_or(false)
}

fn num(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))
}

fn is_active_like(d: &Value) -> bool {
    matches!(status(d), "active" | "extracting") || flag(d, "extracting")
}

fn is_completed_like(d: &Value) -> bool {
    matches!(status(d), "complete" | "seeding")
}

fn is_terminal(d: &Value) -> bool {
    matches!(status(d), "removed" | "complete" | "seeding" | "error")
}

fn in_queue(d: &Value) -> bool {
    status(d) == "paused" && flag(d, "queued") && !is_active_like(d)
}

fn in_paused(d: &Value) -> bool {
    ((status(d) == "paused" && !flag(d, "queued")) || status(d) == "error") && !is_active_like(d) && !is_completed_like(d)
}

fn is_torrent(d: &Value) -> bool {
    d["downloader"].as_i64() == Some(TORRENT)
}

fn is_selective(d: &Value) -> bool {
    d["fileIndices"].as_array().is_some_and(|a| !a.is_empty())
}

fn with(d: &Value, patch: Value) -> Value {
    let mut next = d.clone();
    if let (Some(target), Value::Object(patch)) = (next.as_object_mut(), patch) {
        target.extend(patch);
    }
    next
}

fn as_queued(d: &Value) -> Value {
    with(d, json!({ "status": "paused", "queued": true, "pinnedToHero": false, "extracting": false, "extractionProgress": 0 }))
}

fn as_paused(d: &Value, status: &str) -> Value {
    with(d, json!({ "status": status, "queued": false, "pinnedToHero": false, "extracting": false, "extractionProgress": 0 }))
}

// ---- Layout (src/main/services/download-layout-state.ts) ----

fn ids(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect()
}

/// The preferred order first, then the rest oldest first.
fn merge_ids(downloads: &[&Value], preferred: &[String]) -> Vec<String> {
    let mut candidates: HashSet<String> = downloads.iter().map(|d| id(d)).collect();
    let mut ordered: Vec<String> = preferred.iter().filter(|i| candidates.remove(*i)).cloned().collect();
    let mut by_time = downloads.to_vec();
    by_time.sort_by(|a, b| a["timestamp"].as_f64().unwrap_or(0.0).total_cmp(&b["timestamp"].as_f64().unwrap_or(0.0)));
    for d in by_time {
        let i = id(d);
        if candidates.remove(&i) {
            ordered.push(i);
        }
    }
    ordered
}

fn normalize(downloads: &[Value], layout: &Value) -> Value {
    let queue: Vec<&Value> = downloads.iter().filter(|d| in_queue(d)).collect();
    let paused: Vec<&Value> = downloads.iter().filter(|d| in_paused(d)).collect();
    json!({
        "version": 1,
        "queueOrder": merge_ids(&queue, &ids(&layout["queueOrder"])),
        "pausedOrder": merge_ids(&paused, &ids(&layout["pausedOrder"])),
    })
}

fn next_queued(downloads: &[Value], layout: &Value) -> Option<Value> {
    let first = ids(&layout["queueOrder"]).into_iter().next()?;
    downloads.iter().find(|d| id(d) == first).cloned()
}

fn with_inserted(list: &[String], item: &str, index: Option<i64>) -> Vec<String> {
    let mut next: Vec<String> = list.iter().filter(|i| *i != item).cloned().collect();
    let at = index.unwrap_or(next.len() as i64).clamp(0, next.len() as i64) as usize;
    next.insert(at, item.to_string());
    next
}

fn without(list: &[String], item: &str) -> Vec<String> {
    list.iter().filter(|i| *i != item).cloned().collect()
}

fn all(store: &Store) -> Result<Vec<Value>, String> {
    Ok(store.entries("downloads")?.into_iter().map(|(_, v)| v).filter(Value::is_object).collect())
}

fn get(store: &Store, key: &str) -> Result<Option<Value>, String> {
    let d = store.get("downloads", key)?;
    Ok(d.is_object().then_some(d))
}

fn put(store: &Store, d: &Value) -> Result<(), String> {
    store.put("downloads", &id(d), d)
}

fn normalized_layout(store: &Store, downloads: &[Value]) -> Result<Value, String> {
    Ok(normalize(downloads, &store.get("downloadLayoutState", "layout")?))
}

fn sync_layout(store: &Store, downloads: &[Value]) -> Result<Value, String> {
    let layout = normalized_layout(store, downloads)?;
    store.put("downloadLayoutState", "layout", &layout)?;
    Ok(layout)
}

fn set_queues(store: &Store, downloads: &[Value], queue: Vec<String>, paused: Vec<String>) -> Result<(), String> {
    let visible: HashSet<String> = downloads.iter().map(id).collect();
    let keep = |list: Vec<String>| -> Vec<String> { list.into_iter().filter(|i| visible.contains(i)).collect() };
    let layout = normalize(downloads, &json!({ "queueOrder": keep(queue), "pausedOrder": keep(paused) }));
    store.put("downloadLayoutState", "layout", &layout)
}

fn remove_from_layout(store: &Store, key: &str, downloads: &[Value]) -> Result<(), String> {
    let layout = normalized_layout(store, downloads)?;
    let next = json!({
        "version": 1,
        "queueOrder": without(&ids(&layout["queueOrder"]), key),
        "pausedOrder": without(&ids(&layout["pausedOrder"]), key),
    });
    store.put("downloadLayoutState", "layout", &next)
}

/// `getDownloadLayoutState`.
pub fn layout_state(store: &Store) -> Result<Value, String> {
    sync_layout(store, &all(store)?)
}

// ---- Events ----

fn updated(app: &AppHandle) {
    let _ = app.emit("on-downloads-updated", ());
}

fn progress_event(app: &AppHandle, value: Value) {
    let _ = app.emit("on-download-progress", [value]);
}

/// The taskbar bar; None or a finished download clears it.
fn taskbar(app: &AppHandle, progress: Option<f64>) {
    let Some(window) = app.get_webview_window("main") else { return };
    let state = match progress {
        Some(p) if p < 1.0 => ProgressBarState { status: Some(ProgressBarStatus::Normal), progress: Some((p.max(0.0) * 100.0) as u64) },
        _ => ProgressBarState { status: Some(ProgressBarStatus::None), progress: None },
    };
    let _ = window.set_progress_bar(state);
}

// ---- Engine runtime (DownloadManager) ----

async fn engine_action(params: Value, timeout_ms: u64) -> Result<Value, String> {
    torrent::request("action", params, timeout_ms).await
}

/// `startDownload`: claims the slot, then starts the torrent engine or an HTTP download.
async fn start_runtime(app: &AppHandle, d: &Value) -> Result<(), String> {
    if is_torrent(d) {
        start_torrent(app, d).await
    } else {
        start_http(app, d).await
    }
}

/// Takes the slot for `key`. Returns the previous holder and the new generation.
fn claim_slot(app: &AppHandle, key: &str) -> (Option<String>, u64) {
    let state = app.state::<Downloads>();
    let mut slot = state.slot();
    slot.generation += 1;
    (slot.active.replace(key.to_string()), slot.generation)
}

/// `startTorrentDownload`.
async fn start_torrent(app: &AppHandle, d: &Value) -> Result<(), String> {
    let state = app.state::<Downloads>();
    let key = id(d);
    let selective = is_selective(d);
    let (previous, generation) = claim_slot(app, &key);
    let mut payload = json!({
        "action": "start",
        "game_id": key,
        "url": d["uri"],
        "save_path": d["downloadPath"],
        "trackers": d["customTrackers"],
    });
    if selective {
        payload["file_indices"] = d["fileIndices"].clone();
        payload["metadata_timeout_ms"] = json!(SELECTIVE_TORRENT_METADATA_TIMEOUT_MS);
    }
    let timeout = if selective { SELECTIVE_TORRENT_START_TIMEOUT_MS } else { TORRENT_START_TIMEOUT_MS };
    // The engine may start only now, so it gets the interface and speed limit before the torrent.
    apply_network_interface(saved_interface(app).as_deref()).await;
    let limit = state.slot().limit;
    apply_speed_limit(app, &json!(limit)).await;
    let result = engine_action(payload, timeout).await;
    let (current, same_game) = {
        let slot = state.slot();
        (slot.generation == generation && slot.active.as_deref() == Some(key.as_str()), slot.active.as_deref() == Some(key.as_str()))
    };
    match result {
        Ok(_) if current => Ok(()),
        Ok(_) => {
            // Cancelled or replaced while starting; a restart of the same game keeps its torrent.
            if !same_game {
                let _ = engine_action(json!({ "action": "cancel", "game_id": key }), ENGINE_CALL_TIMEOUT_MS).await;
            }
            Ok(())
        }
        Err(e) => {
            if current {
                state.slot().active = previous;
            }
            Err(e)
        }
    }
}

fn ranges_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join("DownloadRanges"))
}

/// The files of an HTTP download: one hoster file, checked to answer with a file
/// (`validateDownloadUrl`), or a debrid service's files.
async fn prepare_http(store: &Store, d: &Value) -> Result<(Vec<Entry>, Option<String>), String> {
    let downloader = d["downloader"].as_i64().unwrap_or(-1);
    let uri = d["uri"].as_str().unwrap_or_default();
    // A resumed download keeps the name it already has on disk.
    let resumed = d["folderName"].as_str().filter(|f| !f.is_empty()).map(str::to_string);
    if debrid::is_debrid(downloader) {
        let mut plan = debrid::plan(store, d).await?;
        if let ([entry], Some(name)) = (plan.entries.as_mut_slice(), &resumed) {
            entry.path = Some(name.clone());
        }
        return Ok((plan.entries, plan.folder));
    }
    // shortcut: Hydra cloud downloads (Downloader.Hydra) are not supported; Chimaera has no sign-in.
    if !hosters::is_hoster(downloader) {
        return Err(NOT_PORTED.into());
    }
    let resolved = hosters::resolve(downloader, uri).await?;
    http_download::preflight(&resolved.url, &resolved.headers).await?;
    let entry = Entry {
        link: Link::Direct(resolved.url),
        path: resumed.or(resolved.filename),
        size: None,
        exact_size: false,
        resource_id: uri.to_string(),
        headers: resolved.headers,
    };
    Ok((vec![entry], None))
}

/// `startHttpDownload`: the slot is held while the links resolve, so a pause or cancel then wins.
async fn start_http(app: &AppHandle, d: &Value) -> Result<(), String> {
    let key = id(d);
    let (previous, generation) = claim_slot(app, &key);
    let prepared = prepare_http(&app.state::<Store>(), d).await;
    let ranges = ranges_dir(app)?;
    let state = app.state::<Downloads>();
    let mut slot = state.slot();
    let current = slot.generation == generation && slot.active.as_deref() == Some(key.as_str());
    match prepared {
        Err(e) => {
            if current {
                slot.active = previous;
            }
            Err(e)
        }
        // Superseded while resolving; nothing was started.
        Ok(_) if !current => Ok(()),
        Ok((entries, folder)) => {
            let save_path = Path::new(d["downloadPath"].as_str().unwrap_or_default()).to_path_buf();
            let job = Batch::start(entries, folder, save_path, ranges, slot.limit);
            slot.http = Some((key, job));
            Ok(())
        }
    }
}

/// Bumps the generation when `key` holds the slot. Returns the new generation.
fn invalidate_slot(app: &AppHandle, key: &str) -> Option<u64> {
    let state = app.state::<Downloads>();
    let mut slot = state.slot();
    (slot.active.as_deref() == Some(key)).then(|| {
        slot.generation += 1;
        slot.generation
    })
}

/// Frees the slot if `key` still holds it at `generation`.
fn release_slot(app: &AppHandle, key: &str, generation: u64) -> bool {
    let state = app.state::<Downloads>();
    let mut slot = state.slot();
    let current = slot.generation == generation && slot.active.as_deref() == Some(key);
    if current {
        slot.active = None;
    }
    current
}

/// The running HTTP download of `key`, taken out of the slot.
fn take_http(app: &AppHandle, key: &str) -> Option<Arc<Batch>> {
    let state = app.state::<Downloads>();
    let mut slot = state.slot();
    if slot.http.as_ref().is_some_and(|(k, _)| k == key) {
        slot.http.take().map(|(_, job)| job)
    } else {
        None
    }
}

fn http_of(app: &AppHandle, key: &str) -> Option<Arc<Batch>> {
    let state = app.state::<Downloads>();
    let slot = state.slot();
    slot.http.as_ref().filter(|(k, _)| k == key).map(|(_, job)| job.clone())
}

/// Saves what an HTTP download has on disk into its record (`persistPausedJsProgress`).
fn save_http_progress(app: &AppHandle, key: &str, job: &Batch) {
    let store = app.state::<Store>();
    let p = job.progress();
    if let Ok(Some(d)) = get(&store, key) {
        let size = if p.file_size > 0 { json!(p.file_size) } else { d["fileSize"].clone() };
        let progress = if p.file_size > 0 { (p.bytes as f64 / p.file_size as f64).min(1.0) } else { 0.0 };
        let folder = if p.folder_name.is_empty() { d["folderName"].clone() } else { json!(p.folder_name) };
        let _ = put(&store, &with(&d, json!({ "bytesDownloaded": p.bytes, "fileSize": size, "progress": progress, "folderName": folder })));
    }
}

/// Stops the transfer of `key` and keeps its files. `torrent` says which engine owns it.
async fn pause_runtime(app: &AppHandle, key: &str, torrent: bool) {
    let generation = invalidate_slot(app, key);
    if let Some(job) = take_http(app, key) {
        job.stop().await;
        save_http_progress(app, key, &job);
    } else if torrent {
        let _ = engine_action(json!({ "action": "pause", "game_id": key }), ENGINE_CALL_TIMEOUT_MS).await;
    }
    if generation.is_some_and(|g| release_slot(app, key, g)) {
        taskbar(app, None);
    }
}

/// Stops the transfer of `key`; an unfinished HTTP file is deleted.
async fn cancel_runtime(app: &AppHandle, key: &str, torrent: bool) {
    let generation = invalidate_slot(app, key);
    if let Some(job) = take_http(app, key) {
        job.cancel().await;
    } else if torrent {
        if let Err(e) = engine_action(json!({ "action": "cancel", "game_id": key }), ENGINE_CALL_TIMEOUT_MS).await {
            eprintln!("[downloads] cancel {key}: {e}");
        }
    }
    if generation.is_some_and(|g| release_slot(app, key, g)) {
        taskbar(app, None);
        progress_event(app, Value::Null);
    }
}

async fn resume_seeding(d: &Value) -> Result<(), String> {
    let payload = json!({
        "action": "resume_seeding",
        "game_id": id(d),
        "url": d["uri"],
        "save_path": d["downloadPath"],
        "trackers": d["customTrackers"],
    });
    engine_action(payload, ENGINE_CALL_TIMEOUT_MS).await.map(|_| ())
}

/// Extraction reads the files a seeding torrent holds open, so seeding stops around it.
pub async fn stop_seeding_for_extraction(key: &str) {
    if let Err(e) = pause_seeding(key).await {
        eprintln!("[downloads] could not pause seeding {key} for extraction: {e}");
    }
}

pub async fn resume_seeding_after_extraction(app: &AppHandle, key: &str) {
    let Ok(Some(d)) = get(&app.state::<Store>(), key) else { return };
    if let Err(e) = resume_seeding(&d).await {
        eprintln!("[downloads] could not resume seeding {key} after extraction: {e}");
    }
}

async fn pause_seeding(key: &str) -> Result<(), String> {
    engine_action(json!({ "action": "pause_seeding", "game_id": key }), ENGINE_CALL_TIMEOUT_MS).await.map(|_| ())
}

fn speed_limit(value: &Value) -> Option<u64> {
    value.as_f64().filter(|v| v.is_finite() && *v >= 1.0).map(|v| v.floor() as u64)
}

/// applyNetworkInterface: binds libtorrent to the chosen interface ("" = any).
pub async fn apply_network_interface(name: Option<&str>) {
    let payload = json!({ "action": "set_network_interface", "interface": crate::misc::interface_binding(name) });
    if let Err(e) = engine_action(payload, ENGINE_CALL_TIMEOUT_MS).await {
        eprintln!("[downloads] network interface: {e}");
    }
}

fn saved_interface(app: &AppHandle) -> Option<String> {
    app.state::<Store>().get("", "userPreferences").ok()?["torrentNetworkInterface"].as_str().map(str::to_string)
}

/// `applyDownloadSpeedLimit`, from the `maxDownloadSpeedBytesPerSecond` preference.
pub async fn apply_speed_limit(app: &AppHandle, value: &Value) {
    let limit = speed_limit(value);
    let job = {
        let state = app.state::<Downloads>();
        let mut slot = state.slot();
        slot.limit = limit;
        slot.http.as_ref().map(|(_, job)| job.clone())
    };
    if let Some(job) = job {
        job.set_limit(limit);
    }
    let payload = json!({ "action": "set_download_limit", "max_download_speed_bytes_per_second": limit });
    if let Err(e) = engine_action(payload, ENGINE_CALL_TIMEOUT_MS).await {
        eprintln!("[downloads] speed limit: {e}");
    }
}

// ---- Orchestrator ----

fn has_active_transfer(app: &AppHandle, downloads: &[Value]) -> bool {
    app.state::<Downloads>().slot().active.is_some() || downloads.iter().any(|d| status(d) == "active")
}

/// `activateDownload`: marks the record active and starts it. A failed start parks it at the
/// front of the paused list as an error and returns the error.
async fn activate(app: &AppHandle, d: &Value) -> Result<Value, String> {
    let store = app.state::<Store>();
    let key = id(d);
    // A repeated manual action keeps the transfer that already owns the slot.
    if app.state::<Downloads>().slot().active.as_deref() == Some(key.as_str()) {
        return Ok(d.clone());
    }
    let active = with(
        d,
        json!({ "status": "active", "queued": false, "pinnedToHero": false, "extracting": false, "extractionProgress": 0, "timestamp": now_ms() }),
    );
    put(&store, &active)?;
    if let Err(e) = start_runtime(app, &active).await {
        put(&store, &with(&active, json!({ "status": "error", "queued": false })))?;
        let downloads = all(&store)?;
        let layout = normalized_layout(&store, &downloads)?;
        let paused = [vec![key.clone()], without(&ids(&layout["pausedOrder"]), &key)].concat();
        set_queues(&store, &downloads, without(&ids(&layout["queueOrder"]), &key), paused)?;
        updated(app);
        return Err(e);
    }
    Ok(active)
}

/// `activateNextQueuedDownload`; the caller holds the activation lock.
async fn activate_next(app: &AppHandle) -> Result<(), String> {
    let store = app.state::<Store>();
    let downloads = all(&store)?;
    if has_active_transfer(app, &downloads) {
        return Ok(());
    }
    let layout = normalized_layout(&store, &downloads)?;
    if let Some(next) = next_queued(&downloads, &layout) {
        activate(app, &next).await?;
    }
    updated(app);
    Ok(())
}

/// `processNextQueuedDownload`. A failed start is already marked as an error, so try the next one.
async fn start_next(app: &AppHandle) {
    let state = app.state::<Downloads>();
    let _activation = state.activation.lock().await;
    let attempts = all(&app.state::<Store>()).map(|d| d.len()).unwrap_or(0) + 1;
    for _ in 0..attempts {
        match activate_next(app).await {
            Ok(()) => return,
            Err(e) => eprintln!("[downloads] queued start failed: {e}"),
        }
    }
}

/// `queueDownload`: into the queue at `index` (the end when None), out of the paused list.
fn queue_download(store: &Store, d: &Value, index: Option<i64>) -> Result<Value, String> {
    let next = as_queued(d);
    put(store, &next)?;
    let key = id(d);
    let downloads = all(store)?;
    let layout = normalized_layout(store, &downloads)?;
    set_queues(store, &downloads, with_inserted(&ids(&layout["queueOrder"]), &key, index), without(&ids(&layout["pausedOrder"]), &key))?;
    Ok(next)
}

#[derive(Default)]
struct PauseOptions {
    error: bool,
    queue_replacement: bool,
    start_next: bool,
}

/// The orchestrator's `pauseDownload`.
async fn pause(app: &AppHandle, d: &Value, options: PauseOptions) -> Result<Value, String> {
    let store = app.state::<Store>();
    let key = id(d);
    let runtime = is_active_like(d);
    if runtime {
        pause_runtime(app, &key, is_torrent(d)).await;
        progress_event(app, Value::Null);
    }
    // The watcher saved the latest progress while it ran.
    let saved = if runtime { get(&store, &key)?.unwrap_or_else(|| d.clone()) } else { d.clone() };
    let next = if options.queue_replacement {
        queue_download(&store, &saved, Some(0))?
    } else {
        let next = as_paused(&saved, if options.error { "error" } else { "paused" });
        put(&store, &next)?;
        let downloads = all(&store)?;
        let layout = normalized_layout(&store, &downloads)?;
        set_queues(&store, &downloads, without(&ids(&layout["queueOrder"]), &key), with_inserted(&ids(&layout["pausedOrder"]), &key, Some(0)))?;
        next
    };
    if options.start_next && !has_active_transfer(app, &all(&store)?) {
        start_next(app).await;
    }
    updated(app);
    Ok(next)
}

/// `parseBytes` (src/shared/index.ts): "12.5 GB" to bytes, 1024-based.
fn parse_bytes(text: &str) -> Option<i64> {
    const UNITS: [&str; 9] = ["B", "KB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"];
    let text = text.trim();
    let split = text.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','))?;
    let (number, unit) = (&text[..split], text[split..].trim_start());
    if number.is_empty() || unit.is_empty() || !unit.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    // parseFloat reads the longest valid prefix, so "1.2.3" is 1.2.
    let number = number.replace(',', ".");
    let mut parts = number.splitn(3, '.');
    let whole = parts.next().unwrap_or_default();
    let value: f64 = match parts.next() {
        Some(fraction) => format!("{whole}.{fraction}").parse().ok()?,
        None => whole.parse().ok()?,
    };
    let power = UNITS.iter().position(|u| *u == unit.to_ascii_uppercase())?;
    Some((value * 1024f64.powi(power as i32)).round() as i64)
}

/// The record `startGameDownload` and `addGameToQueue` write, before anything is stored.
fn new_download(app: &AppHandle, payload: &Value) -> Result<Value, String> {
    let text = |k: &str| payload[k].as_str().filter(|s| !s.is_empty());
    let (Some(shop), Some(object_id), Some(path)) = (text("shop"), text("objectId"), text("downloadPath")) else {
        return Err("Invalid download request".into());
    };
    let downloader = payload["downloader"].as_i64().unwrap_or(-1);
    // shortcut: Hydra cloud downloads are not supported; Chimaera has no sign-in.
    if downloader != TORRENT && !hosters::is_hoster(downloader) && !debrid::is_debrid(downloader) {
        return Err(NOT_PORTED.into());
    }
    let Some(uri) = text("uri") else { return Err("Invalid download request".into()) };
    if downloader == TORRENT && !uri.starts_with("magnet:") {
        return Err("invalid_magnet".into());
    }
    let file_size = match num(&payload["selectedFilesSize"]) {
        Some(size) => json!(size),
        None => json!(payload["fileSize"].as_str().and_then(parse_bytes)),
    };
    let mut download = json!({
        "shop": shop,
        "objectId": object_id,
        "status": "paused",
        "progress": 0,
        "bytesDownloaded": 0,
        "downloadPath": path,
        "downloader": downloader,
        "uri": payload["uri"],
        "folderName": null,
        "fileSize": file_size,
        "shouldSeed": false,
        "timestamp": now_ms(),
        "queued": false,
        "pinnedToHero": false,
        "extracting": false,
        "customTrackers": trackers::global_trackers(app)?,
    });
    for key in ["automaticallyExtract", "automaticallyDeleteArchiveFiles", "fileIndices", "selectedFilesSize"] {
        if let Some(v) = payload.get(key).filter(|v| !v.is_null()) {
            download[key] = v.clone();
        }
    }
    Ok(download)
}

/// `prepareGameEntry`: drops the old download record and revives or creates the library entry.
fn prepare_game_entry(store: &Store, d: &Value, title: &str) -> Result<(), String> {
    let key = id(d);
    store.del("downloads", &key)?;
    let revived = games::update(store, &key, |game| {
        game.insert("isDeleted".into(), json!(false));
    })?;
    if revived.is_none() {
        games::add_to_library(store, d["shop"].as_str().unwrap_or_default(), d["objectId"].as_str().unwrap_or_default(), title, None)?;
    }
    Ok(())
}

/// DownloadError values (src/shared/constants.ts) for engine codes, as `mapTorrentErrorCode`.
fn start_error(code: &str) -> String {
    match code {
        "invalid_magnet" => "download_error_invalid_magnet",
        "metadata_timeout" => "download_error_torrent_metadata_timeout",
        "metadata_incomplete" => "download_error_torrent_metadata_incomplete",
        "empty_selection" => "download_error_torrent_no_files_selected",
        "invalid_file_indices" => "download_error_torrent_invalid_file_selection",
        "too_many_files" => "download_error_torrent_too_many_files",
        "invalid_trackers" => "download_error_torrent_invalid_trackers",
        other => other,
    }
    .to_string()
}

fn failed(error: &str) -> Value {
    json!({ "ok": false, "error": start_error(error) })
}

/// Undoes a half-written start: the record goes, the layout forgets it.
async fn discard(app: &AppHandle, key: &str, torrent: bool) {
    cancel_runtime(app, key, torrent).await;
    let store = app.state::<Store>();
    let _ = store.del("downloads", key);
    if let Ok(downloads) = all(&store) {
        let _ = remove_from_layout(&store, key, &downloads);
    }
}

/// `startGameDownload(payload)`: queue it, then start it when the slot is free.
// shortcut: no Hydra download count or library sync (local mode, no sign-in), as elsewhere in the port.
pub async fn start_game_download(app: &AppHandle, payload: &Value) -> Value {
    let download = match new_download(app, payload) {
        Ok(d) => d,
        Err(e) => return failed(&e),
    };
    let key = id(&download);
    let store = app.state::<Store>();
    if let Err(e) = prepare_game_entry(&store, &download, payload["title"].as_str().unwrap_or_default()) {
        return failed(&e);
    }
    cancel_runtime(app, &key, is_torrent(&download)).await;
    if let Err(e) = put(&store, &download) {
        return failed(&e);
    }
    let started = async {
        let state = app.state::<Downloads>();
        let _activation = state.activation.lock().await;
        if let Some(current) = get(&store, &key)? {
            if is_active_like(&current) || is_terminal(&current) {
                return Ok(());
            }
        }
        queue_download(&store, &download, None)?;
        activate_next(app).await
    }
    .await;
    match started {
        Ok(()) => {
            updated(app);
            json!({ "ok": true })
        }
        Err(e) => {
            eprintln!("[downloads] start {key} failed: {e}");
            discard(app, &key, is_torrent(&download)).await;
            failed(&e)
        }
    }
}

/// `addGameToQueue(payload)`.
pub async fn add_game_to_queue(app: &AppHandle, payload: &Value) -> Value {
    let download = match new_download(app, payload) {
        Ok(d) => d,
        Err(e) => return failed(&e),
    };
    // A queued hoster link is checked now, so a dead link fails here and not when its turn comes.
    if !is_torrent(&download) {
        if let Err(e) = prepare_http(&app.state::<Store>(), &download).await {
            return failed(&e);
        }
    }
    let key = id(&download);
    let store = app.state::<Store>();
    if let Err(e) = prepare_game_entry(&store, &download, payload["title"].as_str().unwrap_or_default()) {
        return failed(&e);
    }
    cancel_runtime(app, &key, is_torrent(&download)).await;
    if let Err(e) = put(&store, &download).and_then(|_| queue_download(&store, &download, None)) {
        discard(app, &key, is_torrent(&download)).await;
        return json!({ "ok": false, "error": e });
    }
    updated(app);
    json!({ "ok": true })
}

/// `resumeGameDownload(shop, objectId, strategy)`.
pub async fn resume(app: &AppHandle, shop: &str, object_id: &str, strategy: &str) -> Result<bool, String> {
    let store = app.state::<Store>();
    let key = game_key(shop, object_id);
    let Some(d) = get(&store, &key)? else { return Ok(false) };
    if !matches!(status(&d), "paused" | "active" | "error") || d["progress"].as_f64() == Some(1.0) {
        return Ok(false);
    }
    let state = app.state::<Downloads>();
    let _activation = state.activation.lock().await;
    let current = all(&store)?.into_iter().find(|e| is_active_like(e) && id(e) != key);
    if let Some(current) = current {
        if strategy == "queueIfActive" {
            queue_download(&store, &d, Some(0))?;
            updated(app);
            return Ok(true);
        }
        pause(app, &current, PauseOptions::default()).await?;
    }
    activate(app, &d).await?;
    remove_from_layout(&store, &key, &all(&store)?)?;
    updated(app);
    Ok(true)
}

/// `pauseGameDownload(shop, objectId, confirmed)`.
pub async fn pause_by_id(app: &AppHandle, shop: &str, object_id: &str) -> Result<bool, String> {
    let Some(d) = get(&app.state::<Store>(), &game_key(shop, object_id))? else { return Ok(false) };
    pause(app, &d, PauseOptions { start_next: true, ..Default::default() }).await?;
    Ok(true)
}

/// `cancelGameDownload(shop, objectId)`: the record stays with status "removed".
pub async fn cancel_by_id(app: &AppHandle, shop: &str, object_id: &str) -> Result<bool, String> {
    let store = app.state::<Store>();
    let key = game_key(shop, object_id);
    let Some(d) = get(&store, &key)? else { return Ok(false) };
    let was_active = is_active_like(&d);
    cancel_runtime(app, &key, is_torrent(&d)).await;
    progress_event(app, Value::Null);
    put(&store, &with(&d, json!({ "status": "removed", "queued": false, "pinnedToHero": false, "shouldSeed": false, "extracting": false })))?;
    let others: Vec<Value> = all(&store)?.into_iter().filter(|e| id(e) != key).collect();
    remove_from_layout(&store, &key, &others)?;
    if was_active {
        start_next(app).await;
    } else {
        updated(app);
    }
    Ok(true)
}

/// `moveDownloadPlacement(shop, objectId, targetArea, targetIndex)`.
pub async fn move_placement(app: &AppHandle, shop: &str, object_id: &str, area: &str, index: Option<i64>) -> Result<bool, String> {
    let state = app.state::<Downloads>();
    // Only a move to the hero claims the slot.
    let _activation = if area == "hero" { Some(state.activation.lock().await) } else { None };
    let store = app.state::<Store>();
    let key = game_key(shop, object_id);
    let Some(d) = get(&store, &key)? else { return Ok(false) };
    if is_completed_like(&d) || status(&d) == "removed" {
        return Ok(false);
    }
    let downloads = all(&store)?;
    let layout = sync_layout(&store, &downloads)?;
    let current = downloads.iter().find(|e| is_active_like(e) && id(e) != key).cloned();
    let is_hero = downloads.iter().find(|e| is_active_like(e) && status(e) != "removed").is_some_and(|e| id(e) == key);
    let queue_ids = without(&ids(&layout["queueOrder"]), &key);
    let paused_ids = without(&ids(&layout["pausedOrder"]), &key);
    match area {
        "hero" => {
            if let Some(current) = current {
                pause(app, &current, PauseOptions { queue_replacement: true, ..Default::default() }).await?;
            }
            activate(app, &d).await?;
            set_queues(&store, &all(&store)?, queue_ids, paused_ids)?;
            updated(app);
        }
        "queue" => {
            if is_hero && is_active_like(&d) {
                pause_runtime(app, &key, is_torrent(&d)).await;
                progress_event(app, Value::Null);
            }
            put(&store, &as_queued(&d))?;
            set_queues(&store, &all(&store)?, with_inserted(&queue_ids, &key, index), paused_ids)?;
            if is_hero {
                start_next(app).await;
            } else {
                updated(app);
            }
        }
        "paused" => {
            if is_hero && is_active_like(&d) {
                pause(app, &d, PauseOptions { start_next: true, ..Default::default() }).await?;
            } else {
                put(&store, &as_paused(&d, "paused"))?;
            }
            set_queues(&store, &all(&store)?, queue_ids, with_inserted(&paused_ids, &key, index))?;
            updated(app);
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// `setDownloadQueuePosition` (queue = true) and `setPausedDownloadPosition`.
pub fn set_position(app: &AppHandle, shop: &str, object_id: &str, index: i64, queue: bool) -> Result<bool, String> {
    let store = app.state::<Store>();
    let key = game_key(shop, object_id);
    let downloads = all(&store)?;
    let layout = sync_layout(&store, &downloads)?;
    let (queue_ids, paused_ids) = (ids(&layout["queueOrder"]), ids(&layout["pausedOrder"]));
    let list = if queue { &queue_ids } else { &paused_ids };
    if !list.contains(&key) {
        return Ok(false);
    }
    if queue {
        set_queues(&store, &downloads, with_inserted(&queue_ids, &key, Some(index)), paused_ids)?;
    } else {
        set_queues(&store, &downloads, queue_ids, with_inserted(&paused_ids, &key, Some(index)))?;
    }
    updated(app);
    Ok(true)
}

/// `updateDownloadQueuePosition(shop, objectId, "up" | "down")`.
pub fn step_position(app: &AppHandle, shop: &str, object_id: &str, direction: &str) -> Result<bool, String> {
    let store = app.state::<Store>();
    let key = game_key(shop, object_id);
    let downloads = all(&store)?;
    let queue = ids(&normalized_layout(&store, &downloads)?["queueOrder"]);
    let Some(at) = queue.iter().position(|i| *i == key) else { return Ok(false) };
    let target = if direction == "up" { at as i64 - 1 } else { at as i64 + 1 };
    set_position(app, shop, object_id, target, true)
}

/// `pauseGameSeed` (seed = false) and `resumeGameSeed`.
pub async fn set_seeding(app: &AppHandle, shop: &str, object_id: &str, seed: bool) -> Result<(), String> {
    let store = app.state::<Store>();
    let key = game_key(shop, object_id);
    let Some(d) = get(&store, &key)? else { return Ok(()) };
    let next = with(&d, json!({ "status": if seed { "seeding" } else { "complete" }, "shouldSeed": seed }));
    put(&store, &next)?;
    updated(app);
    if seed {
        resume_seeding(&next).await
    } else {
        pause_seeding(&key).await
    }
}

// ---- Watchers (main loop) ----

pub fn dir_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path).into_iter().flatten().flatten().map(|entry| dir_size(&entry.path())).sum()
}

async fn folder_size(d: &Value, folder: &str) -> u64 {
    let path = Path::new(d["downloadPath"].as_str().unwrap_or_default()).join(folder);
    tauri::async_runtime::spawn_blocking(move || dir_size(&path)).await.unwrap_or(0)
}

/// `watchDownloads`: polls the engine, saves progress, sends it to the renderer, finishes a done download.
async fn watch(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<Downloads>();
    let store = app.state::<Store>();
    let (key, generation) = {
        let mut slot = state.slot();
        let Some(key) = slot.active.clone() else {
            slot.orphan = None;
            return Ok(());
        };
        (key, slot.generation)
    };
    let Some(record) = get(&store, &key)? else {
        let confirmed = {
            let mut slot = state.slot();
            let seen = slot.orphan.as_ref() == Some(&(key.clone(), generation));
            slot.orphan = if seen { None } else { Some((key.clone(), generation)) };
            seen
        };
        if confirmed {
            eprintln!("[downloads] {key} no longer exists, cancelling its transfer");
            cancel_runtime(app, &key, true).await;
        }
        return Ok(());
    };
    state.slot().orphan = None;
    if let Some(job) = http_of(app, &key) {
        return watch_http(app, &key, &job).await;
    }
    if !is_torrent(&record) {
        // The hoster link is still resolving: the renderer shows "preparing", as Electron does.
        let game = store.get("games", &key)?;
        if !game.is_null() {
            progress_event(
                app,
                json!({ "numPeers": 0, "numSeeds": 0, "downloadSpeed": 0, "timeRemaining": -1, "isDownloadingMetadata": true,
                        "isCheckingFiles": false, "progress": 0, "gameId": key, "download": record, "game": game }),
            );
        }
        return Ok(());
    }
    let engine = match torrent::request("status", Value::Null, ENGINE_CALL_TIMEOUT_MS).await {
        Ok(v) if v.is_object() => v,
        Ok(_) => return Ok(()),
        Err(e) => {
            eprintln!("[downloads] status poll failed: {e}");
            return Ok(());
        }
    };
    // Paused, cancelled or replaced while the engine answered.
    if state.slot().active.as_deref() != Some(key.as_str()) {
        return Ok(());
    }
    let engine_state = engine["status"].as_i64().unwrap_or(0);
    let metadata = engine_state == ENGINE_DOWNLOADING_METADATA;
    let checking = engine_state == ENGINE_CHECKING_FILES || engine_state == ENGINE_CHECKING_RESUME_DATA;
    let Some(mut download) = get(&store, &key)? else { return Ok(()) };
    let size = num(&engine["fileSize"]).filter(|s| *s > 0).or_else(|| num(&download["selectedFilesSize"])).or_else(|| num(&download["fileSize"])).unwrap_or(0);
    let progress = engine["progress"].as_f64().unwrap_or(0.0);
    let bytes = num(&engine["bytesDownloaded"]).unwrap_or(0);
    if !metadata && !checking {
        download = with(
            &download,
            json!({ "bytesDownloaded": bytes, "fileSize": size, "progress": progress, "folderName": engine["folderName"], "status": "active" }),
        );
        put(&store, &download)?;
    }
    let game = store.get("games", &key)?;
    if game.is_null() {
        return Ok(());
    }
    // shortcut: no low-disk-space halt yet (getDownloadDiskSpace); the engines report the disk error instead.
    let speed = engine["downloadSpeed"].as_f64().unwrap_or(0.0);
    let remaining = (size - bytes) as f64;
    let eta = if remaining >= 0.0 && speed > 0.0 { remaining / speed * 1000.0 } else { -1.0 };
    taskbar(app, Some(progress));
    progress_event(
        app,
        json!({
            "numPeers": engine["numPeers"],
            "numSeeds": engine["numSeeds"],
            "downloadSpeed": speed,
            "timeRemaining": eta,
            "isDownloadingMetadata": metadata,
            "isCheckingFiles": checking,
            "progress": progress,
            "gameId": key,
            "download": download,
            "game": game,
        }),
    );
    if !metadata && !checking && (progress >= 1.0 || status(&download) == "complete") {
        complete(app, &download).await?;
    }
    Ok(())
}

/// `getDownloadStatusFromJs` plus the watcher: saves HTTP progress, finishes or fails the download.
async fn watch_http(app: &AppHandle, key: &str, job: &Batch) -> Result<(), String> {
    let store = app.state::<Store>();
    let p = job.progress();
    match p.state {
        State::Error => return runtime_error(app, key, p.error.as_deref().unwrap_or("Download failed")).await,
        State::Paused => return Ok(()),
        State::Active | State::Complete => {}
    }
    let Some(download) = get(&store, key)? else { return Ok(()) };
    let done = p.state == State::Complete;
    let size = if p.file_size > 0 { p.file_size as i64 } else { num(&download["fileSize"]).unwrap_or(0) };
    let progress = if done { 1.0 } else if size > 0 { (p.bytes as f64 / size as f64).clamp(0.0, 1.0) } else { 0.0 };
    let download = with(
        &download,
        json!({ "bytesDownloaded": p.bytes, "fileSize": size, "progress": progress, "folderName": p.folder_name,
                "status": if done { "complete" } else { "active" } }),
    );
    put(&store, &download)?;
    let game = store.get("games", key)?;
    if game.is_null() {
        return Ok(());
    }
    let remaining = (size - p.bytes as i64) as f64;
    let eta = if remaining >= 0.0 && p.speed > 0.0 { remaining / p.speed * 1000.0 } else { -1.0 };
    taskbar(app, Some(progress));
    progress_event(
        app,
        json!({
            "numPeers": 0,
            "numSeeds": 0,
            "downloadSpeed": p.speed,
            "timeRemaining": eta,
            "isDownloadingMetadata": false,
            "isCheckingFiles": false,
            "isReconnecting": p.reconnecting,
            "progress": progress,
            "gameId": key,
            "download": download,
            "game": game,
        }),
    );
    if done {
        complete(app, &download).await?;
    }
    Ok(())
}

/// `handleRuntimeDownloadError`: the transfer died; park it as an error and move on.
async fn runtime_error(app: &AppHandle, key: &str, message: &str) -> Result<(), String> {
    eprintln!("[downloads] download failed for {key}: {message}");
    let store = app.state::<Store>();
    if let Some(job) = take_http(app, key) {
        save_http_progress(app, key, &job);
    }
    if let Some(generation) = invalidate_slot(app, key) {
        release_slot(app, key, generation);
    }
    taskbar(app, None);
    progress_event(app, Value::Null);
    if let Some(d) = get(&store, key)? {
        put(&store, &with(&d, json!({ "status": "error", "queued": false, "pinnedToHero": false, "extracting": false })))?;
        let downloads = all(&store)?;
        let layout = normalized_layout(&store, &downloads)?;
        let paused = [vec![key.to_string()], without(&ids(&layout["pausedOrder"]), key)].concat();
        set_queues(&store, &downloads, without(&ids(&layout["queueOrder"]), key), paused)?;
    }
    updated(app);
    start_next(app).await;
    Ok(())
}

/// `handleDownloadCompletion`: notify, seed or finish, record the installer size, start the next in queue.
async fn complete(app: &AppHandle, d: &Value) -> Result<(), String> {
    let store = app.state::<Store>();
    let key = id(d);
    let game = store.get("games", &key)?;
    if game.is_object() {
        crate::notifications::download_complete(app, &game);
    }
    let prefs = store.get("", "userPreferences")?;
    let seed = flag(&prefs, "seedAfterDownloadComplete") && is_torrent(d) && !is_selective(d);
    let extract = flag(d, "automaticallyExtract");
    let finished = with(
        d,
        json!({ "status": if seed { "seeding" } else { "complete" }, "shouldSeed": seed, "queued": false, "pinnedToHero": false, "extracting": extract }),
    );
    put(&store, &finished)?;
    updated(app);
    if seed {
        // The torrent keeps seeding in the engine; it only gives up the download slot.
        if let Some(generation) = invalidate_slot(app, &key) {
            release_slot(app, &key, generation);
        }
        taskbar(app, None);
    } else {
        cancel_runtime(app, &key, is_torrent(d)).await;
    }
    if let Some(folder) = d["folderName"].as_str().filter(|f| !f.is_empty()) {
        let size = folder_size(d, folder).await;
        games::update(&store, &key, |game| {
            game.insert("installerSizeInBytes".into(), json!(size));
        })?;
    }
    let (shop, object_id) = (d["shop"].as_str().unwrap_or_default(), d["objectId"].as_str().unwrap_or_default());
    if extract {
        crate::extraction::start(app, shop, object_id, true);
    } else {
        crate::game_installer::search_and_bind_download(app, shop, object_id).await;
    }
    start_next(app).await;
    Ok(())
}

/// `getSeedStatus`: sends seeding stats, and stops seeding a game whose files were deleted.
async fn seed_status(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<Downloads>();
    let store = app.state::<Store>();
    let send_empty = || {
        let mut slot = state.slot();
        if !slot.seed_empty_sent {
            slot.seed_empty_sent = true;
            let _ = app.emit("on-seeding-status", [json!([])]);
        }
    };
    // Nothing seeds without a seeding record; skip the engine so it does not start for nothing.
    if !all(&store)?.iter().any(|d| status(d) == "seeding") {
        send_empty();
        return Ok(());
    }
    let list = match torrent::request("seed_status", Value::Null, ENGINE_CALL_TIMEOUT_MS).await {
        Ok(Value::Array(list)) => list,
        Ok(_) => vec![],
        Err(e) => {
            eprintln!("[downloads] seed status poll failed: {e}");
            vec![]
        }
    };
    if list.is_empty() {
        send_empty();
        return Ok(());
    }
    state.slot().seed_empty_sent = false;
    for seed in &list {
        let Some(key) = seed["gameId"].as_str() else { continue };
        // Walking the folder is the expensive part; a deleted folder can wait a minute.
        let due = {
            let mut slot = state.slot();
            let last = slot.seed_checked.get(key).copied().unwrap_or(0);
            let due = now_ms().saturating_sub(last) >= SEED_SIZE_CHECK_INTERVAL_MS;
            if due {
                slot.seed_checked.insert(key.to_string(), now_ms());
            }
            due
        };
        if !due {
            continue;
        }
        let Some(d) = get(&store, key)? else { continue };
        let expected = num(&seed["fileSize"]).unwrap_or(0);
        let total = folder_size(&d, seed["folderName"].as_str().unwrap_or_default()).await as i64;
        if total < expected {
            pause_seeding(key).await?;
            let progress = if expected > 0 { (total as f64 / expected as f64).min(1.0) } else { d["progress"].as_f64().unwrap_or(0.0) };
            put(&store, &with(&d, json!({ "status": "paused", "shouldSeed": false, "pinnedToHero": false, "progress": progress })))?;
            updated(app);
            let _ = app.emit("on-hard-delete", ());
        }
    }
    let _ = app.emit("on-seeding-status", [Value::Array(list)]);
    Ok(())
}

/// `hasMissingSeedFiles` (src/main/main.ts).
async fn missing_seed_files(d: &Value) -> bool {
    let Some(folder) = d["folderName"].as_str().filter(|f| !f.is_empty()) else { return false };
    let path = Path::new(d["downloadPath"].as_str().unwrap_or_default()).join(folder);
    if !path.exists() {
        return true;
    }
    let expected = num(&d["selectedFilesSize"]).or_else(|| num(&d["fileSize"])).unwrap_or(0);
    expected > 0 && (folder_size(d, folder).await as i64) < expected
}

/// `normalizeStartupDownload`: nothing runs before startup, so active becomes paused; the first
/// interrupted download goes back in the queue.
fn normalize_startup(d: &Value, queue_interrupted: bool) -> Value {
    let mut next = d.clone();
    if flag(&next, "extracting") {
        next["extracting"] = json!(false);
    }
    if flag(&next, "pinnedToHero") {
        next["pinnedToHero"] = json!(false);
    }
    if status(&next) == "active" {
        next["status"] = json!("paused");
        next["queued"] = json!(queue_interrupted);
    }
    if is_terminal(&next) && flag(&next, "queued") {
        next["queued"] = json!(false);
    }
    next
}

/// `bootstrapDownloadsOnStartup` plus the startup part of src/main/main.ts: resume seeding and the
/// interrupted (or next queued) download.
async fn bootstrap(app: &AppHandle) -> Result<(), String> {
    let store = app.state::<Store>();
    let mut interrupted: Option<String> = None;
    for d in all(&store)? {
        let next = normalize_startup(&d, interrupted.is_none());
        if status(&d) == "active" {
            interrupted.get_or_insert_with(|| id(&d));
        }
        if next != d {
            put(&store, &next)?;
        }
    }
    let downloads = all(&store)?;
    let layout = sync_layout(&store, &downloads)?;
    let resume = match &interrupted {
        Some(key) => downloads.iter().find(|d| id(d) == *key).cloned(),
        None => next_queued(&downloads, &layout),
    };
    let mut seeds = vec![];
    let mut candidates: Vec<&Value> = downloads
        .iter()
        .filter(|d| flag(d, "shouldSeed") && is_torrent(d) && d["progress"].as_f64() == Some(1.0) && status(d) == "seeding" && d["uri"].is_string())
        .collect();
    candidates.sort_by(|a, b| b["timestamp"].as_f64().unwrap_or(0.0).total_cmp(&a["timestamp"].as_f64().unwrap_or(0.0)));
    for d in candidates {
        if !missing_seed_files(d).await {
            seeds.push(d.clone());
            continue;
        }
        let expected = num(&d["selectedFilesSize"]).or_else(|| num(&d["fileSize"])).unwrap_or(0);
        let progress = match d["folderName"].as_str().filter(|f| !f.is_empty()) {
            Some(folder) if expected > 0 => (folder_size(d, folder).await as f64 / expected as f64).min(1.0),
            _ => d["progress"].as_f64().unwrap_or(0.0),
        };
        put(&store, &with(d, json!({ "status": "paused", "shouldSeed": false, "queued": false, "pinnedToHero": false, "progress": progress })))?;
        eprintln!("[downloads] seed files missing for {}; seeding was disabled", id(d));
    }
    let resume = resume.filter(|d| is_torrent(d) || d["downloader"].as_i64().is_some_and(|n| hosters::is_hoster(n) || debrid::is_debrid(n)));
    let limit = store.get("", "userPreferences")?["maxDownloadSpeedBytesPerSecond"].clone();
    app.state::<Downloads>().slot().limit = speed_limit(&limit);
    // Start libtorrent only when there is something for it to do.
    if !seeds.is_empty() || resume.as_ref().is_some_and(is_torrent) {
        apply_network_interface(saved_interface(app).as_deref()).await;
        apply_speed_limit(app, &limit).await;
    }
    for d in &seeds {
        if let Err(e) = resume_seeding(d).await {
            eprintln!("[downloads] resume seeding {}: {e}", id(d));
        }
    }
    if let Some(d) = resume {
        if let Err(e) = start_runtime(app, &d).await {
            eprintln!("[downloads] resume {}: {e}", id(&d));
        }
    }
    updated(app);
    Ok(())
}

/// The download half of the main loop: startup, then the 2-second progress and seed watchers.
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        if let Err(e) = bootstrap(&app).await {
            eprintln!("[downloads] startup failed: {e}");
        }
        loop {
            tokio::time::sleep(WATCH_INTERVAL).await;
            if let Err(e) = watch(&app).await {
                eprintln!("[downloads] watch: {e}");
            }
            if let Err(e) = seed_status(&app).await {
                eprintln!("[downloads] seed status: {e}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dl(object_id: &str, status: &str, queued: bool, timestamp: u64) -> Value {
        json!({ "shop": "steam", "objectId": object_id, "status": status, "queued": queued, "timestamp": timestamp })
    }

    #[test]
    fn layout_keeps_preferred_order_then_oldest_first() {
        let downloads = vec![
            dl("a", "paused", true, 30),
            dl("b", "paused", true, 10),
            dl("c", "paused", true, 20),
            dl("d", "paused", false, 5),
            dl("e", "error", true, 1),
            dl("f", "active", true, 1),
            dl("g", "complete", false, 1),
        ];
        let layout = normalize(&downloads, &json!({ "queueOrder": ["steam:a", "steam:gone", "steam:a"], "pausedOrder": [] }));
        assert_eq!(ids(&layout["queueOrder"]), ["steam:a", "steam:b", "steam:c"]);
        assert_eq!(ids(&layout["pausedOrder"]), ["steam:e", "steam:d"]);
        assert_eq!(next_queued(&downloads, &layout).map(|d| id(&d)), Some("steam:a".into()));
    }

    #[test]
    fn inserts_clamped() {
        let list = vec!["a".to_string(), "b".into(), "c".into()];
        assert_eq!(with_inserted(&list, "c", Some(0)), ["c", "a", "b"]);
        assert_eq!(with_inserted(&list, "a", Some(-1)), ["a", "b", "c"]);
        assert_eq!(with_inserted(&list, "a", Some(99)), ["b", "c", "a"]);
        assert_eq!(with_inserted(&list, "x", None), ["a", "b", "c", "x"]);
    }

    #[test]
    fn parses_sizes_like_parse_bytes() {
        assert_eq!(parse_bytes("1 KB"), Some(1024));
        assert_eq!(parse_bytes("12,5 gb"), Some(13_421_772_800));
        assert_eq!(parse_bytes("700MB"), Some(734_003_200));
        assert_eq!(parse_bytes("5 parsecs"), None);
        assert_eq!(parse_bytes("GB"), None);
    }

    #[test]
    fn startup_requeues_only_the_first_interrupted_download() {
        let first = normalize_startup(&dl("a", "active", false, 1), true);
        let second = normalize_startup(&dl("b", "active", false, 1), false);
        assert_eq!((status(&first), flag(&first, "queued")), ("paused", true));
        assert_eq!((status(&second), flag(&second, "queued")), ("paused", false));
        let done = normalize_startup(&json!({ "status": "complete", "queued": true, "extracting": true }), true);
        assert!(!flag(&done, "queued") && !flag(&done, "extracting"));
    }

    #[test]
    fn maps_torrent_start_errors() {
        assert_eq!(start_error("empty_selection"), "download_error_torrent_no_files_selected");
        assert_eq!(start_error("torrent_timeout"), "torrent_timeout");
    }

    #[test]
    fn layout_writes_go_through_the_store() {
        let store = Store::memory().unwrap();
        for d in [dl("a", "paused", true, 1), dl("b", "paused", true, 2)] {
            put(&store, &d).unwrap();
        }
        let downloads = all(&store).unwrap();
        set_queues(&store, &downloads, vec!["steam:b".into(), "steam:a".into(), "steam:ghost".into()], vec![]).unwrap();
        assert_eq!(ids(&layout_state(&store).unwrap()["queueOrder"]), ["steam:b", "steam:a"]);
        remove_from_layout(&store, "steam:b", &downloads).unwrap();
        assert_eq!(ids(&store.get("downloadLayoutState", "layout").unwrap()["queueOrder"]), ["steam:a"]);
    }
}
