//! Archive extraction after a download (`extractGameDownload`, automatic extraction).
//! Ported from src/main/events/library/extract-game-download.ts, the extraction half of
//! services/game-files-manager.ts and services/7zip.ts. 7-Zip ships in `binaries/`.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::games::game_key;
use crate::installer;
use crate::store::Store;

const ARCHIVE_EXTENSIONS: &[&str] = &[".rar", ".zip", ".7z"];
/// Tried in order, like Electron; a wrong password on a plain archive is harmless.
const PASSWORDS: &[&str] = &["online-fix.me", "steamrip.com"];
const MAX_PASSES: usize = 2;
const PROGRESS_THROTTLE: Duration = Duration::from_secs(1);

fn active() -> std::sync::MutexGuard<'static, HashSet<String>> {
    static ACTIVE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    ACTIVE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

pub fn is_archive(name: &str) -> bool {
    let name = name.to_lowercase();
    ARCHIVE_EXTENSIONS.iter().any(|ext| name.ends_with(ext))
}

pub(crate) fn seven_zip(app: &AppHandle) -> Result<PathBuf, String> {
    let name = if cfg!(windows) { "7z.exe" } else { "7zzs" };
    Ok(app.path().resource_dir().map_err(|e| e.to_string())?.join("binaries").join(name))
}

fn default_downloads(app: &AppHandle) -> PathBuf {
    app.path().download_dir().unwrap_or_default()
}

/// Per extraction: sends throttled, never-decreasing progress to the renderer.
struct Job {
    app: AppHandle,
    shop: String,
    object_id: String,
    last_sent: Option<Instant>,
    last_value: f64,
}

impl Job {
    fn new(app: &AppHandle, shop: &str, object_id: &str) -> Self {
        Self { app: app.clone(), shop: shop.into(), object_id: object_id.into(), last_sent: None, last_value: 0.0 }
    }

    fn key(&self) -> String {
        game_key(&self.shop, &self.object_id)
    }

    fn progress(&mut self, value: f64, force: bool) {
        if !force && (self.last_sent.is_some_and(|t| t.elapsed() < PROGRESS_THROTTLE) || value < self.last_value) {
            return;
        }
        self.last_value = value;
        self.last_sent = Some(Instant::now());
        let _ = self.app.emit("on-extraction-progress", json!([self.shop, self.object_id, value]));
    }

    fn download(&self) -> Option<Value> {
        self.app.state::<Store>().get("downloads", &self.key()).ok().filter(Value::is_object)
    }

    fn put_download(&self, patch: Value) {
        let Some(mut download) = self.download() else { return };
        if let (Some(target), Value::Object(patch)) = (download.as_object_mut(), patch) {
            target.extend(patch);
        }
        if let Err(e) = self.app.state::<Store>().put("downloads", &self.key(), &download) {
            eprintln!("[extraction] could not save {}: {e}", self.key());
        }
        let _ = self.app.emit("on-downloads-updated", ());
    }

    /// resetExtractingState + on-extraction-failed.
    fn fail(&mut self, error: &str, failure: Value) {
        eprintln!("[extraction] {} failed: {error}", self.key());
        if let Some(d) = self.download() {
            let done = d["progress"].as_f64() == Some(1.0);
            let status = if !done {
                d["status"].clone()
            } else if d["shouldSeed"].as_bool() == Some(true) && d["downloader"].as_i64() == Some(crate::downloads::TORRENT) {
                json!("seeding")
            } else {
                json!("complete")
            };
            let has_installer = installer::download_installer(&d, &default_downloads(&self.app)).is_some();
            self.put_download(json!({ "status": status, "queued": false, "extracting": false, "hasInstaller": has_installer }));
        }
        let _ = self.app.emit("on-extraction-failed", json!([self.shop, self.object_id, failure]));
        self.last_sent = None;
        self.last_value = 0.0;
    }

    fn fail_missing_source(&mut self) {
        self.fail("no downloaded file was found to extract", json!({ "reason": "file-not-found" }));
    }

    /// setExtractionComplete: binds the executable, records sizes, tells the renderer.
    async fn complete(&mut self, notify: bool) {
        let Some(d) = self.download() else { return };
        crate::game_installer::search_and_bind_download(&self.app, &self.shop, &self.object_id).await;
        let has_installer = installer::download_installer(&d, &default_downloads(&self.app)).is_some();
        self.put_download(json!({ "extracting": false, "hasInstaller": has_installer }));
        let store = self.app.state::<Store>();
        let game = store.get("games", &self.key()).unwrap_or(Value::Null);
        if let (true, Some(folder)) = (game.is_object(), installer::folder_path(&d, &default_downloads(&self.app))) {
            let size = tauri::async_runtime::spawn_blocking(move || crate::downloads::dir_size(&folder)).await.unwrap_or(0);
            let _ = crate::games::update(&store, &self.key(), |g| {
                g.insert("installedSizeInBytes".into(), json!(size));
            });
        }
        let _ = self.app.emit("on-extraction-complete", json!([self.shop, self.object_id]));
        if notify && game.is_object() {
            crate::notifications::extraction_complete(&self.app, &game);
        }
        self.last_sent = None;
        self.last_value = 0.0;
        // shortcut: classics disc auto-linking after extraction waits for the emulators milestone.
    }

    /// The outer archive or the root archives after extraction: delete when the download or the
    /// preferences say so, else ask the renderer (it answers through `deleteArchive`).
    fn handle_archives(&self, paths: Vec<PathBuf>) {
        let paths: Vec<PathBuf> = paths.into_iter().filter(|p| p.exists()).collect();
        if paths.is_empty() {
            return;
        }
        let store = self.app.state::<Store>();
        let preferences = store.get("", "userPreferences").unwrap_or(Value::Null);
        let delete = self.download().and_then(|d| d["automaticallyDeleteArchiveFiles"].as_bool()).or(preferences["deleteArchiveFilesAfterExtractionByDefault"].as_bool()).unwrap_or(false);
        if delete {
            for path in &paths {
                if let Err(e) = installer::delete_archive(&store, &default_downloads(&self.app), &path.to_string_lossy()) {
                    eprintln!("[extraction] could not delete {}: {e}", path.display());
                }
            }
        } else {
            let list: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
            let _ = self.app.emit("on-archive-deletion-prompt", json!([list]));
        }
    }
}

/// The percentage 7-Zip prints with -bsp1 (" 45% 12 - file"), from a chunk of its output.
fn last_percent(chunk: &str) -> Option<f64> {
    chunk.split(['\r', '\n', '\u{8}']).filter_map(|part| part.trim().split_once('%').and_then(|(n, _)| n.trim().parse::<f64>().ok())).last()
}

fn is_password_error(stderr: &str) -> bool {
    let text = stderr.to_lowercase();
    text.contains("wrong password") || text.contains("can not open encrypted archive") || text.contains("encrypted")
}

/// `7z x` into `destination`, trying each password; `progress` gets 0..=1 per attempt.
pub(crate) fn run_7z(binary: &Path, archive: &Path, destination: &Path, progress: &mut dyn FnMut(f64)) -> Result<(), String> {
    std::fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    for (index, password) in PASSWORDS.iter().enumerate() {
        let mut command = Command::new(binary);
        command
            .arg("x")
            .arg(archive)
            .arg(format!("-o{}", destination.display()))
            .args(["-y", "-bsp1", "-bso0", "-spd", &format!("-p{password}")])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().map_err(|e| format!("could not run 7-Zip: {e}"))?;
        let mut stderr = child.stderr.take().unwrap();
        let errors = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });
        let mut stdout = child.stdout.take().unwrap();
        let mut buffer = [0u8; 4096];
        while let Ok(n) = stdout.read(&mut buffer) {
            if n == 0 {
                break;
            }
            if let Some(percent) = last_percent(&String::from_utf8_lossy(&buffer[..n])) {
                progress(percent / 100.0);
            }
        }
        let status = child.wait().map_err(|e| e.to_string())?;
        let stderr = errors.join().unwrap_or_default();
        // 1 is "warning (non-fatal errors)": the files are out, as node-7z let through.
        if matches!(status.code(), Some(0 | 1)) {
            if status.code() == Some(1) {
                eprintln!("[extraction] 7-Zip warnings for {}: {}", archive.display(), stderr.trim());
            }
            return Ok(());
        }
        if index + 1 < PASSWORDS.len() && is_password_error(&stderr) {
            eprintln!("[extraction] wrong password for {}, trying the next one", archive.display());
            continue;
        }
        return Err(format!("Failed to extract file: {} ({})", archive.display(), stderr.trim()));
    }
    unreachable!("PASSWORDS is not empty")
}

/// The volume number of a multi-part RAR ("x.part2.rar" is 2).
fn rar_part(name: &str) -> Option<u32> {
    let stem = name.to_lowercase().strip_suffix(".rar")?.to_string();
    let digits: String = stem.chars().rev().take_while(char::is_ascii_digit).collect::<Vec<_>>().into_iter().rev().collect();
    let before = &stem[..stem.len() - digits.len()];
    (!digits.is_empty() && before.ends_with("part")).then(|| digits.parse().ok()).flatten()
}

/// Root archives worth extracting in a pass: only the first volume of a multi-part RAR.
/// Unlike Electron's /part1\.rar$/, "part01" counts as the first volume too.
fn archives_to_extract(names: &[String], done: &HashSet<String>) -> Vec<String> {
    names.iter().filter(|n| !done.contains(*n) && rar_part(n).is_none_or(|p| p == 1)).cloned().collect()
}

fn root_archives(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut names: Vec<String> = std::fs::read_dir(dir)?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| is_archive(n))
        .collect();
    names.sort();
    Ok(names)
}

/// extractFilesInDirectory: two passes over the folder's root archives (a pass can unpack
/// more archives). Subfolders are left alone: their archives may be game assets.
async fn extract_in_directory(job: &mut Job, dir: &Path, outer_archive: Option<&Path>) -> bool {
    if !dir.exists() {
        job.fail_missing_source();
        return false;
    }
    if !dir.is_dir() {
        job.fail(&format!("expected an extraction folder at {}", dir.display()), Value::Null);
        return false;
    }
    let binary = match seven_zip(&job.app) {
        Ok(b) => b,
        Err(e) => {
            job.fail(&e, Value::Null);
            return false;
        }
    };
    let (mut seen, mut done) = (HashSet::new(), HashSet::new());
    for pass in 0..MAX_PASSES {
        let names = match root_archives(dir) {
            Ok(names) => names,
            Err(e) => {
                job.fail(&e.to_string(), Value::Null);
                return false;
            }
        };
        seen.extend(names.iter().cloned());
        let todo = archives_to_extract(&names, &done);
        if todo.is_empty() {
            continue;
        }
        if pass == 0 {
            job.progress(0.0, true);
        }
        for (i, name) in todo.iter().enumerate() {
            let (archive, cwd) = (dir.join(name), dir.to_path_buf());
            let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            let binary = binary.clone();
            let run = tauri::async_runtime::spawn_blocking(move || run_7z(&binary, &archive, &cwd, &mut |p| { let _ = sender.send(p); }));
            while let Some(p) = receiver.recv().await {
                job.progress((pass as f64 + (i as f64 + p) / todo.len() as f64) / MAX_PASSES as f64, false);
            }
            match run.await.map_err(|e| e.to_string()).and_then(|r| r) {
                Ok(()) => {
                    done.insert(name.clone());
                    job.progress((pass as f64 + (i + 1) as f64 / todo.len() as f64) / MAX_PASSES as f64, true);
                }
                Err(e) => {
                    job.fail(&e, Value::Null);
                    return false;
                }
            }
        }
    }
    if !done.is_empty() {
        job.progress(1.0, true);
        // The caller decides about the outer archive.
        if outer_archive.is_none() {
            job.handle_archives(seen.iter().map(|n| dir.join(n)).collect());
        }
    }
    true
}

/// extractDownloadedFile: the download is one archive; it unpacks beside itself into a folder
/// of the same name, which becomes the download's folder.
async fn extract_downloaded_file(job: &mut Job) -> bool {
    let Some(d) = job.download() else { return false };
    let Some(folder_name) = d["folderName"].as_str().map(str::to_string) else {
        job.fail_missing_source();
        return false;
    };
    let root = d["downloadPath"].as_str().map(PathBuf::from).unwrap_or_else(|| default_downloads(&job.app));
    let file = root.join(&folder_name);
    if !file.exists() {
        job.fail_missing_source();
        return false;
    }
    let stem = Path::new(&folder_name).with_extension("");
    let extracted_name = stem.to_string_lossy().into_owned();
    let destination = root.join(&stem);
    let binary = match seven_zip(&job.app) {
        Ok(b) => b,
        Err(e) => {
            job.fail(&e, Value::Null);
            return false;
        }
    };
    job.progress(0.0, true);
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let (archive, out) = (file.clone(), destination.clone());
    let run = tauri::async_runtime::spawn_blocking(move || run_7z(&binary, &archive, &out, &mut |p| { let _ = sender.send(p); }));
    while let Some(p) = receiver.recv().await {
        job.progress(p, false);
    }
    if let Err(e) = run.await.map_err(|e| e.to_string()).and_then(|r| r) {
        job.fail(&e, Value::Null);
        return false;
    }
    if !extract_in_directory(job, &destination, Some(&file)).await {
        return false;
    }
    if destination.exists() {
        job.handle_archives(vec![file]);
    }
    job.put_download(json!({ "folderName": extracted_name, "sourceArchiveName": folder_name }));
    job.complete(true).await;
    true
}

/// The download is already a folder or a non-archive file.
async fn extract_folder_or_skip(job: &mut Job, target: &Path, notify_unsupported: bool, notify_complete: bool) {
    if target.is_file() {
        // handleUnsupportedExtraction: nothing to unpack; bind what is there.
        crate::game_installer::search_and_bind_download(&job.app, &job.shop, &job.object_id).await;
        if notify_unsupported {
            let format = target.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase())).unwrap_or_default();
            let failure = if format.is_empty() { Value::Null } else { json!({ "reason": "unsupported-format", "format": format }) };
            job.fail(&format!("unsupported extraction format for {}", target.display()), failure);
        } else {
            job.put_download(json!({ "extracting": false }));
        }
        return;
    }
    if extract_in_directory(job, target, None).await {
        job.complete(notify_complete).await;
    }
}

/// Runs one extraction for the download, pausing a torrent's seeding around it.
async fn run(app: AppHandle, shop: String, object_id: String, automatic: bool) {
    let key = game_key(&shop, &object_id);
    let mut job = Job::new(&app, &shop, &object_id);
    let Some(d) = job.download() else {
        active().remove(&key);
        return;
    };
    let seeding = d["downloader"].as_i64() == Some(crate::downloads::TORRENT) && d["shouldSeed"].as_bool() == Some(true) && d["status"].as_str() == Some("seeding");
    if seeding {
        crate::downloads::stop_seeding_for_extraction(&key).await;
    }
    let folder = d["folderName"].as_str().unwrap_or_default().to_string();
    let target = installer::folder_path(&d, &default_downloads(&app));
    match target {
        None => job.fail_missing_source(),
        Some(target) if !target.exists() => job.fail_missing_source(),
        Some(_) if is_archive(&folder) => {
            extract_downloaded_file(&mut job).await;
        }
        Some(target) => extract_folder_or_skip(&mut job, &target, !automatic, automatic).await,
    }
    if seeding {
        crate::downloads::resume_seeding_after_extraction(&app, &key).await;
    }
    active().remove(&key);
}

/// `extractGameDownload`: starts the extraction and answers at once.
pub fn start(app: &AppHandle, shop: &str, object_id: &str, automatic: bool) -> bool {
    let key = game_key(shop, object_id);
    let store = app.state::<Store>();
    let download = store.get("downloads", &key).unwrap_or(Value::Null);
    let game = store.get("games", &key).unwrap_or(Value::Null);
    if !download.is_object() || !game.is_object() {
        Job::new(app, shop, object_id).fail("could not start extraction because download metadata is missing", Value::Null);
        return false;
    }
    if !active().insert(key.clone()) {
        return true;
    }
    let mut job = Job::new(app, shop, object_id);
    job.put_download(json!({ "extracting": true }));
    if download["folderName"].as_str().is_none_or(str::is_empty) {
        active().remove(&key);
        job.fail_missing_source();
        return false;
    }
    tauri::async_runtime::spawn(run(app.clone(), shop.into(), object_id.into(), automatic));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_first_rar_parts_and_skips_done() {
        let names: Vec<String> = ["a.part1.rar", "a.part2.rar", "b.zip", "c.part01.rar", "d.7z"].iter().map(|s| s.to_string()).collect();
        let done: HashSet<String> = ["d.7z".to_string()].into();
        assert_eq!(archives_to_extract(&names, &done), ["a.part1.rar", "b.zip", "c.part01.rar"]);
        assert!(is_archive("Game.RAR") && !is_archive("game.exe"));
    }

    #[test]
    fn reads_7zip_progress_and_password_errors() {
        assert_eq!(last_percent("  3% 1 - a\r\u{8}\u{8} 45% 12 - b"), Some(45.0));
        assert_eq!(last_percent("Everything is Ok"), None);
        assert!(is_password_error("ERROR: Wrong password : x.rar"));
        assert!(!is_password_error("ERROR: Data Error"));
    }

    /// Needs binaries/7z.exe: zips a file with 7-Zip, extracts it back.
    #[test]
    #[cfg(windows)]
    fn extracts_with_the_bundled_7zip() {
        let binary = Path::new(env!("CARGO_MANIFEST_DIR")).join("../binaries/7z.exe");
        if !binary.exists() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("chimaera-7z-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src").join("game.exe"), b"hello").unwrap();
        let archive = dir.join("game.7z");
        let made = Command::new(&binary).arg("a").arg(&archive).arg(dir.join("src").join("game.exe")).stdout(Stdio::null()).status().unwrap();
        assert!(made.success());
        let mut seen = Vec::new();
        run_7z(&binary, &archive, &dir.join("out"), &mut |p| seen.push(p)).unwrap();
        assert_eq!(std::fs::read(dir.join("out").join("game.exe")).unwrap(), b"hello");
        // The second password: the first attempt fails as a wrong password, then it retries.
        let locked = dir.join("locked.7z");
        let made = Command::new(&binary).arg("a").arg("-psteamrip.com").arg("-mhe=on").arg(&locked).arg(dir.join("src").join("game.exe")).stdout(Stdio::null()).status().unwrap();
        assert!(made.success());
        run_7z(&binary, &locked, &dir.join("locked-out"), &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(dir.join("locked-out").join("game.exe")).unwrap(), b"hello");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
