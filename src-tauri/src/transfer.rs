//! `transferGameFiles`: moves an installed game's folder to another place and repoints the
//! library. Ported from src/main/events/library/transfer-game-files.ts and
//! events/helpers/find-game-root.ts. Unlike Electron, a move on the same drive is a rename,
//! and pause/resume work (Electron's preload exposed them without handlers).

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::games::{game_key, with_executable_path};
use crate::store::Store;

const BLOCK: usize = 4 * 1024 * 1024;
const REPORT_INTERVAL: Duration = Duration::from_millis(100);
const MAX_UPWARD_LEVELS: usize = 3;

#[derive(Default)]
struct Control {
    cancelled: AtomicBool,
    paused: AtomicBool,
}

fn transfers() -> std::sync::MutexGuard<'static, HashMap<String, Arc<Control>>> {
    static ACTIVE: OnceLock<Mutex<HashMap<String, Arc<Control>>>> = OnceLock::new();
    ACTIVE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

pub fn cancel(shop: &str, object_id: &str) {
    if let Some(c) = transfers().get(&game_key(shop, object_id)) {
        c.cancelled.store(true, Ordering::Relaxed);
    }
}

pub fn set_paused(shop: &str, object_id: &str, paused: bool) {
    if let Some(c) = transfers().get(&game_key(shop, object_id)) {
        c.paused.store(paused, Ordering::Relaxed);
    }
}

// ---- findGameRootFromExe ----

const NESTED_EXE_DIRS: &[&str] = &["bin", "bin32", "bin64", "binaries", "win32", "win64", "x64", "x86", "game", "runtime", "engine"];
const ROOT_DIRS: &[&str] = &[
    "data", "assets", "content", "paks", "pak", "resources", "localization", "languages", "saves", "mods", "dlc", "music", "sound", "sounds", "audio", "videos",
    "movies", "cinematics", "textures", "shaders", "configs", "config", "settings", "plugins", "native", "managed", "mono", "dotnet", "engine", "launcher",
];
const DATA_EXTENSIONS: &[&str] = &["pak", "dat", "bundle", "assets", "forge", "arc", "pck", "vpk", "wad", "bsa", "ba2", "big", "cpk", "fsb", "bank"];
const ROOT_FILES: &[&str] = &["steam_api.dll", "steam_api64.dll", "version.txt", "readme.txt", "eula.txt", "unins000.exe", "uninstall.exe"];
const UNSAFE_ROOTS: &[&str] =
    &["program files", "program files (x86)", "users", "windows", "system32", "appdata", "programdata", "steamapps", "common", "desktop", "documents", "downloads"];

fn lower_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// isUnsafePath: a well-known system or library folder, or a drive root.
fn is_unsafe(dir: &Path) -> bool {
    UNSAFE_ROOTS.contains(&lower_name(dir).as_str()) || dir.parent().is_none_or(|p| p.parent().is_none())
}

/// scoreDirectory: (score, has an .exe).
fn score(dir: &Path) -> (u32, bool) {
    let Ok(entries) = fs::read_dir(dir) else { return (0, false) };
    let (mut total, mut exe) = (0, false);
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            total += if ROOT_DIRS.contains(&name.as_str()) { 2 } else { 0 } + if name.ends_with("_data") { 3 } else { 0 } + if name == "binaries" || name == "content" { 2 } else { 0 };
        } else if kind.is_file() {
            if name.ends_with(".exe") {
                exe = true;
                total += 1;
            }
            let ext = Path::new(&name).extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
            total += if DATA_EXTENSIONS.contains(&ext.as_str()) { 2 } else { 0 } + if ROOT_FILES.contains(&name.as_str()) { 1 } else { 0 };
        }
    }
    (total, exe)
}

/// The folder holding the whole game, walking up from the executable's folder.
pub fn find_game_root(exe: &Path) -> Option<PathBuf> {
    let exe_dir = exe.parent()?.to_path_buf();
    if is_unsafe(&exe_dir) {
        return None;
    }
    let mut candidates: Vec<(PathBuf, u32, bool)> = vec![];
    let (mut dir, mut up) = (exe_dir.clone(), 0);
    while up <= MAX_UPWARD_LEVELS && !is_unsafe(&dir) {
        let (s, has_exe) = score(&dir);
        candidates.push((dir.clone(), s, has_exe));
        if up == 0 && NESTED_EXE_DIRS.contains(&lower_name(&dir).as_str()) {
            up += 1;
            dir = dir.parent()?.to_path_buf();
            continue;
        }
        if s >= 3 && has_exe {
            break;
        }
        let Some(parent) = dir.parent().map(Path::to_path_buf) else { break };
        dir = parent;
        up += 1;
    }
    let Some(mut best) = candidates.first().cloned() else { return Some(exe_dir) };
    for c in &candidates {
        let better_with_exe = c.1 >= 3 && c.2 && (!best.2 || c.1 > best.1);
        let better_without = !best.2 && c.1 > best.1;
        if better_with_exe || better_without {
            best = c.clone();
        }
    }
    if best.1 >= 2 {
        return Some(best.0);
    }
    // getFallbackPath
    if NESTED_EXE_DIRS.contains(&lower_name(&exe_dir).as_str()) {
        if let Some(parent) = exe_dir.parent().filter(|p| !is_unsafe(p)) {
            return Some(parent.to_path_buf());
        }
    }
    Some(exe_dir)
}

// ---- Copy engine ----

struct Mover<'a> {
    /// (progress 0..=1, stats) for on-game-transfer-progress.
    send: &'a mut dyn FnMut(f64, Value),
    control: &'a Control,
    total: u64,
    done: u64,
    started: Instant,
    last_report: Option<Instant>,
}

impl Mover<'_> {
    fn report(&mut self, force: bool) {
        if !force && self.last_report.is_some_and(|t| t.elapsed() < REPORT_INTERVAL) {
            return;
        }
        self.last_report = Some(Instant::now());
        let seconds = self.started.elapsed().as_secs_f64();
        let mbps = if seconds > 0.0 { self.done as f64 / seconds / 1_048_576.0 } else { 0.0 };
        let eta = if mbps > 0.0 { ((self.total.saturating_sub(self.done)) as f64 / (mbps * 1_048_576.0)).ceil() } else { 0.0 };
        let stats = json!({ "speed": mbps.max(0.0), "eta": eta, "transferred": self.done, "total": self.total });
        (self.send)(self.done as f64 / self.total.max(1) as f64, stats);
    }

    /// Waits out a pause; Err once cancelled.
    fn checkpoint(&self) -> Result<(), String> {
        while self.control.paused.load(Ordering::Relaxed) && !self.control.cancelled.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
        }
        if self.control.cancelled.load(Ordering::Relaxed) { Err("cancelled".into()) } else { Ok(()) }
    }

    fn copy_file(&mut self, from: &Path, to: &Path) -> Result<(), String> {
        let mut input = fs::File::open(from).map_err(|e| format!("{}: {e}", from.display()))?;
        let mut output = fs::File::create(to).map_err(|e| format!("{}: {e}", to.display()))?;
        let mut buffer = vec![0u8; BLOCK];
        loop {
            self.checkpoint()?;
            let n = input.read(&mut buffer).map_err(|e| format!("{}: {e}", from.display()))?;
            if n == 0 {
                break;
            }
            output.write_all(&buffer[..n]).map_err(|e| format!("{}: {e}", to.display()))?;
            self.done += n as u64;
            self.report(false);
        }
        if let Ok(modified) = fs::metadata(from).and_then(|m| m.modified()) {
            let _ = output.set_modified(modified);
        }
        Ok(())
    }

    fn copy_dir(&mut self, from: &Path, to: &Path) -> Result<(), String> {
        fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
        let mut entries: Vec<fs::DirEntry> = fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            self.checkpoint()?;
            let (src, dst) = (entry.path(), to.join(entry.file_name()));
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() {
                self.copy_dir(&src, &dst)?;
            } else if kind.is_file() {
                self.copy_file(&src, &dst)?;
            }
        }
        Ok(())
    }
}

fn free_space(dir: &Path) -> Option<u64> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let target = dir.to_string_lossy().to_lowercase();
    disks
        .iter()
        .filter(|d| target.starts_with(&d.mount_point().to_string_lossy().to_lowercase()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| d.available_space())
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase();
    if cfg!(windows) { norm(a) == norm(b) } else { a == b }
}

fn is_inside(child: &Path, parent: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase();
    if cfg!(windows) { norm(child).starts_with(&format!("{}\\", norm(parent))) } else { child.starts_with(parent) && child != parent }
}

fn failure(error: &str) -> Value {
    json!({ "ok": false, "error": error })
}

/// `transferGameFiles`: answers when the move is done, cancelled or failed.
pub async fn transfer(app: &AppHandle, shop: &str, object_id: &str, dest_parent: &str) -> Value {
    let key = game_key(shop, object_id);
    let control = Arc::new(Control::default());
    transfers().insert(key.clone(), control.clone());
    let result = run(app, shop, object_id, dest_parent, &control).await;
    transfers().remove(&key);
    result
}

async fn run(app: &AppHandle, shop: &str, object_id: &str, dest_parent: &str, control: &Arc<Control>) -> Value {
    let key = game_key(shop, object_id);
    let store = app.state::<Store>();
    let game = store.get("games", &key).unwrap_or(Value::Null);
    let Some(exe) = game["executablePath"].as_str().filter(|e| !e.is_empty()).map(PathBuf::from) else {
        return failure("Game not found or has no executable path");
    };
    let Some(root) = find_game_root(&exe) else { return failure("Cannot determine game root folder") };
    let Some(folder) = root.file_name() else { return failure("Cannot determine game root folder") };
    let dest_parent = PathBuf::from(dest_parent);
    let target = dest_parent.join(folder);
    if same_path(&root, &target) {
        return failure("Game is already in this location");
    }
    if is_inside(&target, &root) {
        return failure("Destination is inside source folder");
    }
    match fs::metadata(&target) {
        Ok(_) => return failure("Destination folder already exists"),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return failure("Cannot access destination folder"),
        Err(_) => {}
    }
    let _ = app.emit("on-game-transfer-progress", json!([shop, object_id, 0, { "speed": 0, "eta": 0, "transferred": 0, "total": 0 }]));
    let size_root = root.clone();
    let size = tauri::async_runtime::spawn_blocking(move || crate::downloads::dir_size(&size_root)).await.unwrap_or(0);
    if let Err(e) = fs::create_dir_all(&dest_parent) {
        return failure(&e.to_string());
    }

    // Same drive: a rename moves it at once and keeps everything as it was.
    let renamed = fs::rename(&root, &target).is_ok();
    if !renamed {
        if let Some(available) = free_space(&dest_parent).filter(|a| *a < size) {
            return json!({ "ok": false, "error": "not_enough_space", "needed": size, "available": available });
        }
        let (app2, shop2, id2, control2, root2, target2) = (app.clone(), shop.to_string(), object_id.to_string(), control.clone(), root.clone(), target.clone());
        let copied = tauri::async_runtime::spawn_blocking(move || {
            let mut send = |progress: f64, stats: Value| {
                let _ = app2.emit("on-game-transfer-progress", json!([shop2, id2, progress, stats]));
            };
            let mut copy = Mover { send: &mut send, control: &control2, total: size, done: 0, started: Instant::now(), last_report: None };
            let result = copy.copy_dir(&root2, &target2);
            if result.is_ok() {
                copy.report(true);
            }
            result
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r);
        if let Err(message) = copied {
            let _ = fs::remove_dir_all(&target);
            if message == "cancelled" {
                let _ = app.emit("on-game-transfer-cancelled", json!([shop, object_id]));
                return failure("Transfer cancelled");
            }
            let _ = app.emit("on-game-transfer-error", json!([shop, object_id, message]));
            return failure(&message);
        }
        // The copy is complete; a source that will not go away is only left behind.
        if let Err(e) = fs::remove_dir_all(&root) {
            eprintln!("[transfer] could not remove {}: {e}", root.display());
        }
    }

    let new_exe = target.join(exe.strip_prefix(&root).unwrap_or(exe.file_name().map(Path::new).unwrap_or(Path::new(""))));
    let new_exe_text = new_exe.to_string_lossy().into_owned();
    let updated = crate::games::update(&store, &key, |g| {
        let installed = g.get("installedSizeInBytes").filter(|v| !v.is_null()).cloned().unwrap_or(json!(size));
        if let Value::Object(next) = with_executable_path(Value::Object(std::mem::take(g)), Some(&new_exe_text)) {
            *g = next;
        }
        g.insert("installedSizeInBytes".into(), installed);
    });
    let download = store.get("downloads", &key).unwrap_or(Value::Null);
    let saved = updated.and_then(|_| {
        if !download.is_object() {
            return Ok(());
        }
        let mut d = download;
        d["downloadPath"] = json!(dest_parent.to_string_lossy());
        d["folderName"] = json!(folder.to_string_lossy());
        store.put("downloads", &key, &d)
    });
    if saved.is_err() {
        let _ = app.emit("on-game-transfer-error", json!([shop, object_id, "Failed to update database"]));
        return failure("Failed to update database");
    }
    let _ = app.emit("on-game-transfer-complete", json!([shop, object_id, new_exe_text]));
    json!({ "ok": true, "newExePath": new_exe_text })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chimaera-transfer-{name}-{}", uuid::Uuid::new_v4())).join("Games");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn finds_the_root_above_a_bin_folder() {
        let games = temp("root");
        let game = games.join("Cool Game");
        fs::create_dir_all(game.join("Binaries").join("Win64")).unwrap();
        fs::create_dir_all(game.join("Content")).unwrap();
        fs::write(game.join("Binaries").join("Win64").join("cool.exe"), b"").unwrap();
        fs::write(game.join("cool.exe"), b"").unwrap();
        fs::write(game.join("data.pak"), b"").unwrap();
        assert_eq!(find_game_root(&game.join("Binaries").join("Win64").join("cool.exe")), Some(game.clone()));
        assert_eq!(find_game_root(&game.join("cool.exe")), Some(game.clone()));
        fs::remove_dir_all(games.parent().unwrap()).unwrap();
    }

    #[test]
    fn copies_a_tree_and_stops_when_cancelled() {
        let games = temp("copy");
        let src = games.join("G");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("a.bin"), vec![7u8; BLOCK + 10]).unwrap();
        fs::write(src.join("sub").join("b.txt"), b"hi").unwrap();
        let control = Control::default();
        let mut last = 0.0;
        let mut send = |p: f64, _: Value| last = p;
        let mut mover = Mover { send: &mut send, control: &control, total: (BLOCK + 12) as u64, done: 0, started: Instant::now(), last_report: None };
        mover.copy_dir(&src, &games.join("H")).unwrap();
        mover.report(true);
        assert_eq!(last, 1.0);
        assert_eq!(fs::read(games.join("H").join("sub").join("b.txt")).unwrap(), b"hi");
        assert_eq!(fs::metadata(games.join("H").join("a.bin")).unwrap().len(), (BLOCK + 10) as u64);

        control.cancelled.store(true, Ordering::Relaxed);
        let mut ignore = |_: f64, _: Value| {};
        let mut mover = Mover { send: &mut ignore, control: &control, total: 1, done: 0, started: Instant::now(), last_report: None };
        assert_eq!(mover.copy_dir(&src, &games.join("I")), Err("cancelled".to_string()));
        fs::remove_dir_all(games.parent().unwrap()).unwrap();
    }

    #[test]
    fn refuses_unsafe_folders() {
        assert!(find_game_root(Path::new(r"C:\Program Files\game.exe")).is_none());
        assert!(is_unsafe(Path::new(r"C:\")));
        // Paths compare case-insensitively only on Windows.
        if cfg!(windows) {
            assert!(is_inside(Path::new(r"C:\a\b\c"), Path::new(r"C:\A\b")));
            assert!(!is_inside(Path::new(r"C:\a\bc"), Path::new(r"C:\a\b")));
            assert!(same_path(Path::new(r"C:\A\b\"), Path::new(r"c:\a\B")));
        } else {
            assert!(is_inside(Path::new("/a/b/c"), Path::new("/a/b")));
            assert!(!is_inside(Path::new("/a/bc"), Path::new("/a/b")));
            assert!(!is_inside(Path::new("/A/b/c"), Path::new("/a/b")));
            assert!(same_path(Path::new("/a/b/"), Path::new("/a/b")));
        }
    }
}
