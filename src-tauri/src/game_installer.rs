//! `openGameInstaller` and `recheckInstalledExecutable`, ported from
//! src/main/events/library/open-game-installer.ts and recheck-installed-executable.ts,
//! with helpers/installed-executable-poller.ts, helpers/installer-target-directories.ts
//! and GameFilesManager.searchAndBindInstalledExecutable.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_opener::OpenerExt;

use crate::data_location::DataDir;
use crate::games::{game_key, with_executable_path};
use crate::installer;
use crate::library::now_ms;
use crate::scan::{rank_executable, Known};
use crate::store::Store;

const POLL_INTERVAL: Duration = Duration::from_secs(30);
const POLL_DURATION_MS: u64 = 10 * 60_000;
const RECHECK_INTERVAL_MS: u64 = 30_000;
const RECHECK_MAX_AGE_MS: u64 = 24 * 60 * 60_000;
const QUICK_INSTALLER_EXIT_MS: u64 = 15_000;
const MAX_INSTALLER_TRACKING: Duration = Duration::from_secs(2 * 60 * 60);
const INSTALLED_EXECUTABLE_MAX_DEPTH: usize = 4;
const UAC_DECLINED_EXIT_CODE: i32 = 1223;
const SUCCESS_EXIT_CODES: [i32; 3] = [0, 1641, 3010];
const INSTALLER_STARTED_MARKER: &str = "CHIMAERA_INSTALLER_STARTED";
const INSTALLER_EXIT_MARKER: &str = "CHIMAERA_INSTALLER_EXIT=";
const MODIFICATION_TIME_TOLERANCE_MS: u64 = 2000;
const MAX_CHANGED_FOLDERS: usize = 10;

const WINDOWS_GAME_DIRECTORIES: &[&str] = &[r"C:\Games", r"D:\Games"];
const WINDOWS_DRIVE_GAME_FOLDERS: &[&str] = &["Games", "GOG Games"];
const WINE_DRIVE_C_GAME_FOLDERS: &[&str] = &["", "Games", "GOG Games", "Program Files", "Program Files (x86)"];
const SKIPPED_FOLDERS: &[&str] = &[
    "steam", "steamlibrary", "steamapps", "epic games", "gog galaxy", "ubisoft", "ubisoft game launcher", "ea games", "ea app", "electronic arts", "origin",
    "origin games", "battle.net", "riot games", "rockstar games", "xboxgames", "windowsapps", "hydra", "common files", "internet explorer",
    "windows media player", "windows nt", "windowspowershell", "microsoft.net", "msbuild", "reference assemblies",
];
const ROOT_SKIPPED_FOLDERS: &[&str] = &[
    "games", "gog games", "program files", "program files (x86)", "programdata", "users", "windows", "windows.old", "openxr", "vrclient", "proton_shortcuts",
    "$recycle.bin", "$sysreset", "system volume information", "recovery", "perflogs", "msocache", "config.msi", "intel", "amd", "nvidia", "onedrivetemp",
    "$windows.~bt", "$windows.~ws", "$winreagent",
];

/// Per game key, the installerStartedAt of: the installer we still treat as running (released
/// after two hours), the run we still wait on, the poller, and the last renderer recheck.
#[derive(Default)]
struct Runs {
    running: HashMap<String, u64>,
    live: HashMap<String, u64>,
    pollers: HashMap<String, (u64, JoinHandle<()>)>,
    last_recheck: HashMap<String, u64>,
}

fn runs() -> MutexGuard<'static, Runs> {
    static RUNS: OnceLock<Mutex<Runs>> = OnceLock::new();
    RUNS.get_or_init(Default::default).lock().unwrap()
}

/// Removes `map[key]` only when it is still this run. True when it did.
fn release(map: &mut HashMap<String, u64>, key: &str, started_at: u64) -> bool {
    let ours = map.get(key) == Some(&started_at);
    if ours {
        map.remove(key);
    }
    ours
}

fn stop_polling(key: &str) {
    if let Some((_, handle)) = runs().pollers.remove(key) {
        handle.abort();
    }
}

// ---- Installed executable search (installer-target-directories.ts, find-game-executable.ts) ----

fn unique(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in values {
        if !v.is_empty() && !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

/// getInstallerTargetDirectories: where an installer may have put the game.
fn target_directories(download_root: &Path, wine_prefix: Option<&Path>) -> Vec<PathBuf> {
    if cfg!(windows) {
        let env = |name: &str| std::env::var(name).unwrap_or_default();
        let download_drive = download_root.to_string_lossy().split_once(':').map(|(d, _)| format!("{d}:")).unwrap_or_default();
        let drives = unique(["C:".into(), "D:".into(), env("SystemDrive"), download_drive].map(|d: String| d.to_uppercase()));
        let dirs = WINDOWS_GAME_DIRECTORIES
            .iter()
            .map(|d| d.to_string())
            .chain(drives.iter().map(|d| format!(r"{d}\")))
            .chain(drives.iter().flat_map(|d| WINDOWS_DRIVE_GAME_FOLDERS.iter().map(move |f| format!(r"{d}\{f}"))))
            .chain([env("ProgramFiles"), env("ProgramFiles(x86)")]);
        return unique(dirs).into_iter().map(PathBuf::from).collect();
    }
    if cfg!(target_os = "linux") {
        let default = std::env::var("WINEPREFIX").ok().or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.wine"))).unwrap_or_default();
        let prefixes = unique(wine_prefix.map(|p| p.to_string_lossy().into_owned()).into_iter().chain([default]));
        // the downloads folder is the installer's G: drive (map_games_drive), so games installed there are found too
        return prefixes
            .iter()
            .flat_map(|p| WINE_DRIVE_C_GAME_FOLDERS.iter().map(move |f| Path::new(p).join("drive_c").join(f)))
            .chain([download_root.to_path_buf()])
            .collect();
    }
    Vec::new()
}

fn is_skipped(dir: &Path, name: &str) -> bool {
    let name = name.to_lowercase();
    let is_root = dir.parent().is_none() || dir.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("drive_c"));
    SKIPPED_FOLDERS.contains(&name.as_str()) || (is_root && ROOT_SKIPPED_FOLDERS.contains(&name.as_str()))
}

fn same_folder(a: &Path, b: &Path) -> bool {
    let clean = |p: &Path| {
        let s = p.to_string_lossy().replace('/', std::path::MAIN_SEPARATOR_STR);
        let s = s.trim_end_matches(std::path::MAIN_SEPARATOR).to_string();
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    clean(a) == clean(b)
}

fn changed_at(meta: &fs::Metadata) -> u64 {
    let ms = |t: std::io::Result<std::time::SystemTime>| t.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as u64);
    ms(meta.created()).max(ms(meta.modified()))
}

/// listFoldersChangedSince: the newest subfolders of `dirs` touched since the installer started.
fn folders_changed_since(dirs: &[PathBuf], since: u64, excluded: &[&Path]) -> Vec<PathBuf> {
    let threshold = since.saturating_sub(MODIFICATION_TIME_TOLERANCE_MS);
    let mut changed: Vec<(u64, PathBuf)> = Vec::new();
    for dir in dirs {
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !entry.file_type().is_ok_and(|t| t.is_dir()) || is_skipped(dir, &name) {
                continue;
            }
            let path = entry.path();
            let at = fs::metadata(&path).map(|m| changed_at(&m)).unwrap_or(0);
            if at >= threshold && !excluded.iter().any(|e| same_folder(e, &path)) && !changed.iter().any(|(_, p)| *p == path) {
                changed.push((at, path));
            }
        }
    }
    changed.sort_by(|a, b| b.0.cmp(&a.0));
    changed.into_iter().take(MAX_CHANGED_FOLDERS).map(|(_, p)| p).collect()
}

fn collect_files(root: &Path, dir: &Path, depth_left: Option<usize>, out: &mut Vec<String>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        let path = entry.path();
        if kind.is_dir() && depth_left != Some(0) {
            collect_files(root, &path, depth_left.map(|d| d - 1), out);
        } else if kind.is_file() {
            out.push(path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned());
        }
    }
}

/// findGameExecutableInFolder: the ranked executable under `folder`, at most `max_depth` folders down.
fn find_in_folder(folder: &Path, executables: &[Known], max_depth: Option<usize>) -> Option<String> {
    let mut files = Vec::new();
    collect_files(folder, folder, max_depth, &mut files);
    rank_executable(&files, executables).map(|r| folder.join(r).to_string_lossy().into_owned())
}

/// The download folder first (searchAndBindExecutable), then one match among the folders
/// that changed since the installer started; several matches bind nothing.
fn find_installed(download_folder: Option<&Path>, download_root: &Path, executables: &[Known], since: u64, wine_prefix: Option<&Path>) -> Option<String> {
    if let Some(found) = download_folder.filter(|f| f.exists()).and_then(|f| find_in_folder(f, executables, None)) {
        return Some(found);
    }
    let changed = folders_changed_since(&target_directories(download_root, wine_prefix), since, &[download_root]);
    let matches = unique(changed.iter().filter_map(|f| find_in_folder(f, executables, Some(INSTALLED_EXECUTABLE_MAX_DEPTH))));
    if matches.len() > 1 {
        eprintln!("[GameInstaller] not binding, several installed copies match: {}", matches.join(", "));
        return None;
    }
    matches.into_iter().next()
}

fn has_executable(game: &Value) -> bool {
    game["executablePath"].as_str().is_some_and(|p| !p.is_empty())
}

/// GameFilesManager.searchAndBindInstalledExecutable.
async fn search_and_bind(app: &AppHandle, shop: &str, object_id: &str, since: u64, wine_prefix: Option<&str>) -> bool {
    let store = app.state::<Store>();
    let key = game_key(shop, object_id);
    let (Ok(download), Ok(game)) = (store.get("downloads", &key), store.get("games", &key)) else { return false };
    if download.is_null() || game.is_null() {
        return false;
    }
    if has_executable(&game) {
        return true;
    }
    let version = app.package_info().version.to_string();
    let Some(executables) = crate::scan::catalogue(&store, &version).await.and_then(|c| c.get(object_id).map(<[Known]>::to_vec)) else { return false };
    let default = app.path().download_dir().unwrap_or_default();
    let root = installer::downloads_path(&store, &default);
    let folder = installer::folder_path(&download, &root);
    let download_root = download["downloadPath"].as_str().map(PathBuf::from).unwrap_or(root);
    let prefix = wine_prefix.map(PathBuf::from);
    let pick = tauri::async_runtime::spawn_blocking(move || find_installed(folder.as_deref(), &download_root, &executables, since, prefix.as_deref())).await;
    match pick {
        Ok(Some(exe)) => bind(app, shop, object_id, &exe).await,
        _ => false,
    }
}

/// searchAndBindExecutable: the download folder only (nothing changed after `u64::MAX`).
pub async fn search_and_bind_download(app: &AppHandle, shop: &str, object_id: &str) -> bool {
    search_and_bind(app, shop, object_id, u64::MAX, None).await
}

/// bindExecutable: links the executable unless the game got one meanwhile.
async fn bind(app: &AppHandle, shop: &str, object_id: &str, exe: &str) -> bool {
    let store = app.state::<Store>();
    let mut bound = false;
    let updated = crate::games::update(&store, &game_key(shop, object_id), |game| {
        if game.get("executablePath").and_then(Value::as_str).is_some_and(|p| !p.is_empty()) {
            return;
        }
        bound = true;
        if let Value::Object(next) = with_executable_path(Value::Object(std::mem::take(game)), Some(exe)) {
            *game = next;
        }
    });
    let Ok(Some(game)) = updated else { return false };
    if !bound {
        return has_executable(&game);
    }
    eprintln!("[GameInstaller] auto-detected executable for {object_id}: {exe}");
    // shortcut: no achievement metadata export; the achievement watcher picks the game up on its next pass.
    let _ = app.emit("on-library-batch-complete", ());
    let wants_shortcuts = store.get("", "userPreferences").ok().and_then(|p| p["createStartMenuShortcut"].as_bool()).unwrap_or(true);
    if wants_shortcuts {
        let locations: &[&str] = if cfg!(windows) { &["desktop", "start_menu"] } else { &["desktop"] };
        for location in locations {
            if let Err(e) = crate::shortcuts::create_game_shortcut(app, shop, object_id, location).await {
                eprintln!("[GameInstaller] could not create the {location} shortcut for {object_id}: {e}");
            }
        }
    }
    true
}

// ---- Poller (installed-executable-poller.ts) ----

fn still_waiting(store: &Store, key: &str, started_at: u64) -> bool {
    let download = store.get("downloads", key).unwrap_or(Value::Null);
    let game = store.get("games", key).unwrap_or(Value::Null);
    download["installerLaunched"].as_bool() == Some(true)
        && download["installerRunning"].as_bool() != Some(true)
        && download["installerStartedAt"].as_u64() == Some(started_at)
        && !has_executable(&game)
}

/// Some installers start the real setup and exit at once, so look again every 30 s for 10 minutes.
fn start_polling(app: AppHandle, shop: String, object_id: String, started_at: u64, wine_prefix: Option<String>) {
    let key = game_key(&shop, &object_id);
    stop_polling(&key);
    let task_key = key.clone();
    let handle = tauri::async_runtime::spawn(async move {
        let deadline = now_ms() + POLL_DURATION_MS;
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            if now_ms() > deadline || !still_waiting(&app.state::<Store>(), &task_key, started_at) {
                break;
            }
            if search_and_bind(&app, &shop, &object_id, started_at, wine_prefix.as_deref()).await {
                eprintln!("[GameInstaller] found the installed executable for {object_id} while polling");
                let _ = app.emit("on-library-batch-complete", ());
                let _ = app.emit("on-game-installer-closed", json!([shop, object_id, true]));
                break;
            }
        }
        let mut runs = runs();
        if runs.pollers.get(&task_key).is_some_and(|(s, _)| *s == started_at) {
            runs.pollers.remove(&task_key);
        }
    });
    runs().pollers.insert(key, (started_at, handle));
}

// ---- Launch (open-game-installer.ts) ----

enum Outcome {
    Exit(Option<i32>),
    /// Started, but nothing to wait on.
    Untracked,
    /// The elevated launch failed; open the installer through the shell instead.
    OpenUntracked,
}

type Waiter = Box<dyn FnOnce() -> Outcome + Send>;

/// The installer's exit code from the elevated PowerShell wrapper's output.
fn read_elevated_output(output: &str) -> Outcome {
    if let Some(code) = output.lines().find_map(|l| l.strip_prefix(INSTALLER_EXIT_MARKER)) {
        return Outcome::Exit(code.trim().parse().ok());
    }
    if output.lines().any(|l| l.trim() == INSTALLER_STARTED_MARKER) {
        return Outcome::Untracked;
    }
    eprintln!("[GameInstaller] failed to launch the installer with elevation: {output}");
    Outcome::OpenUntracked
}

#[cfg(windows)]
fn spawn_installer(installer: &Path, _compat: &Compat, _games_root: &Path) -> Option<Waiter> {
    use std::os::windows::process::CommandExt;
    const ERROR_ELEVATION_REQUIRED: i32 = 740;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let cwd = installer.parent().unwrap_or(Path::new("."));
    match Command::new(installer).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        Ok(mut child) => return Some(Box::new(move || Outcome::Exit(child.wait().ok().and_then(|s| s.code())))),
        Err(e) if e.raw_os_error() == Some(ERROR_ELEVATION_REQUIRED) || e.kind() == std::io::ErrorKind::PermissionDenied => {}
        Err(e) => {
            eprintln!("[GameInstaller] failed to run {} directly: {e}", installer.display());
            return None;
        }
    }
    // The path goes through an environment variable so no quoting reaches PowerShell.
    let script = [
        "$ErrorActionPreference = 'Stop'",
        "try {",
        "$info = New-Object System.Diagnostics.ProcessStartInfo",
        "$info.FileName = $env:CHIMAERA_INSTALLER_PATH",
        "$info.WorkingDirectory = [System.IO.Path]::GetDirectoryName($env:CHIMAERA_INSTALLER_PATH)",
        "$info.Verb = 'runas'",
        "$info.UseShellExecute = $true",
        "$installer = [System.Diagnostics.Process]::Start($info)",
        &format!("Write-Output '{INSTALLER_STARTED_MARKER}'"),
        &format!("if ($installer) {{ $installer.WaitForExit(); Write-Output ('{INSTALLER_EXIT_MARKER}' + $installer.ExitCode) }}"),
        "} catch {",
        "$failure = $_.Exception.GetBaseException()",
        &format!("if ($failure.NativeErrorCode -eq {UAC_DECLINED_EXIT_CODE}) {{ Write-Output '{INSTALLER_EXIT_MARKER}{UAC_DECLINED_EXIT_CODE}' }} else {{ Write-Output $failure.Message }}"),
        "}",
    ]
    .join("; ");
    let child = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script])
        .env("CHIMAERA_INSTALLER_PATH", installer)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    match child {
        Ok(child) => Some(Box::new(move || match child.wait_with_output() {
            Ok(out) => read_elevated_output(&format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))),
            Err(_) => Outcome::OpenUntracked,
        })),
        Err(e) => {
            eprintln!("[GameInstaller] failed to run the installer with elevation: {e}");
            None
        }
    }
}

// shortcut: Linux runs the installer with plain `wine`, not umu-run/Proton, and only in the game's
// own prefix setting (no per-game default prefix); port both with the Linux launch path.
/// Maps the prefix's G: drive to the games folder, so an installer's folder picker offers it.
/// Leaves an existing G: alone: it may be the user's own mapping.
#[cfg(not(windows))]
fn map_games_drive(prefix: &Path, games_root: &Path) {
    let devices = prefix.join("dosdevices");
    let drive = devices.join("g:");
    if drive.symlink_metadata().is_ok() || !games_root.is_dir() {
        return;
    }
    if let Err(e) = fs::create_dir_all(&devices).and_then(|_| std::os::unix::fs::symlink(games_root, &drive)) {
        eprintln!("[GameInstaller] could not map G: to {}: {e}", games_root.display());
    }
}

/// Where a Windows installer runs on Linux: the game's prefix and Proton, as Play uses them.
#[derive(Default)]
struct Compat {
    prefix: Option<PathBuf>,
    proton: Option<PathBuf>,
    log: Option<PathBuf>,
    steam_id: Option<String>,
}

/// launchInstallerWithUmu: umu-run first, plain wine when umu-run is missing or fails.
#[cfg(not(windows))]
fn spawn_installer(installer: &Path, compat: &Compat, games_root: &Path) -> Option<Waiter> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let prefix = compat.prefix.clone()
        .or_else(|| std::env::var_os("WINEPREFIX").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".wine")));
    if let Some(prefix) = &prefix {
        let _ = fs::create_dir_all(prefix);
        map_games_drive(prefix, games_root);
    }
    let cwd = installer.parent().unwrap_or(Path::new("."));
    if let (Some(prefix), Some(log), true) = (&prefix, &compat.log, crate::proton::on_path("umu-run")) {
        let exe = installer.to_string_lossy().into_owned();
        let plain = crate::launch::LaunchCommand { command: exe.clone(), args: Vec::new(), env: Vec::new() };
        let command = crate::proton::umu_command(plain, &exe, compat.proton.as_deref(), prefix, compat.steam_id.as_deref(), false, false);
        let (tx, rx) = std::sync::mpsc::channel();
        let hooks = crate::umu_launch::Hooks { on_status: Box::new(|_| {}), game_detected: Box::new(|| false), on_exit: Box::new(move |code| drop(tx.send(code))) };
        match crate::umu_launch::launch(&command, cwd, log, crate::umu_launch::QUICK_EXIT, hooks).map(crate::umu_launch::Pending::wait) {
            Ok(Ok(_)) => return Some(Box::new(move || Outcome::Exit(rx.recv().ok().flatten()))),
            // A quick exit with nothing wrong in umu's output is an installer that finished.
            Ok(Err(early)) if early.failure.is_none() => {
                eprintln!("[GameInstaller] installer exited early under umu-run: {early}");
                return Some(Box::new(move || Outcome::Exit(early.code)));
            }
            Ok(Err(early)) => eprintln!("[GameInstaller] umu-run failed, trying wine: {early}"),
            Err(e) => eprintln!("[GameInstaller] could not start umu-run, trying wine: {e}"),
        }
    }
    let mut command = Command::new("wine");
    crate::proton::use_host_env(&mut command).arg(installer).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if let Some(prefix) = &compat.prefix {
        command.env("WINEPREFIX", prefix);
    }
    match command.spawn() {
        Ok(mut child) => Some(Box::new(move || Outcome::Exit(child.wait().ok().and_then(|s| s.code())))),
        Err(e) => {
            eprintln!("[GameInstaller] failed to run {} with wine: {e}", installer.display());
            None
        }
    }
}

fn describe_exit(code: Option<i32>) -> String {
    match code {
        Some(0) => "completed".into(),
        Some(c) if SUCCESS_EXIT_CODES.contains(&c) => format!("completed and asked for a restart (exit code {c})"),
        Some(UAC_DECLINED_EXIT_CODE) => "UAC prompt declined".into(),
        None => "terminated without an exit code".into(),
        Some(c) => format!("cancelled or failed with exit code {c}"),
    }
}

/// updateInstallerState: merges `state` into the download record and tells the renderer.
fn record(app: &AppHandle, key: &str, state: Value) {
    let store = app.state::<Store>();
    let Ok(Value::Object(mut download)) = store.get("downloads", key) else { return };
    download.extend(state.as_object().cloned().unwrap_or_else(Map::new));
    if let Err(e) = store.put("downloads", key, &Value::Object(download)) {
        eprintln!("[GameInstaller] failed to update the installer state of {key}: {e}");
    }
    let _ = app.emit("on-downloads-updated", ());
}

struct Run {
    app: AppHandle,
    shop: String,
    object_id: String,
    key: String,
    started_at: u64,
    installer: PathBuf,
    wine_prefix: Option<String>,
}

impl Run {
    async fn finish(self, outcome: Outcome) {
        {
            let mut runs = runs();
            release(&mut runs.running, &self.key, self.started_at);
            release(&mut runs.live, &self.key, self.started_at);
        }
        let opened = |run: &Run| run.app.opener().open_path(run.installer.to_string_lossy(), None::<&str>).is_ok();
        match outcome {
            Outcome::Exit(code) => self.handle_exit(code).await,
            Outcome::Untracked => record(&self.app, &self.key, json!({ "installerLaunched": true, "installerRunning": false })),
            Outcome::OpenUntracked if opened(&self) => record(&self.app, &self.key, json!({ "installerLaunched": true, "installerRunning": false })),
            Outcome::OpenUntracked => {
                eprintln!("[GameInstaller] could not open {}", self.installer.display());
                record(&self.app, &self.key, json!({ "installerLaunched": false, "installerRunning": false }));
            }
        }
    }

    async fn handle_exit(self, code: Option<i32>) {
        let Run { app, shop, object_id, key, started_at, wine_prefix, .. } = self;
        eprintln!("[GameInstaller] installer for {object_id} {}", describe_exit(code));
        let download = app.state::<Store>().get("downloads", &key).unwrap_or(Value::Null);
        if download["installerStartedAt"].as_u64() != Some(started_at) {
            eprintln!("[GameInstaller] ignoring the exit of an older installer run for {object_id}");
            return;
        }
        stop_polling(&key);
        if !code.is_some_and(|c| SUCCESS_EXIT_CODES.contains(&c)) {
            record(&app, &key, json!({ "installerRunning": false, "installerLaunched": false }));
            return;
        }
        let found = search_and_bind(&app, &shop, &object_id, started_at, wine_prefix.as_deref()).await;
        let exited_quickly = now_ms().saturating_sub(started_at) < QUICK_INSTALLER_EXIT_MS;
        record(&app, &key, json!({ "installerRunning": false }));
        let _ = app.emit("on-library-batch-complete", ());
        if !found {
            eprintln!("[GameInstaller] no installed executable for {object_id} yet, polling for it");
            start_polling(app.clone(), shop.clone(), object_id.clone(), started_at, wine_prefix);
            // A quick exit usually hands off to a second setup process; stay quiet while it runs.
            if exited_quickly {
                return;
            }
        }
        let _ = app.emit("on-game-installer-closed", json!([shop, object_id, found]));
    }
}

/// `openGameInstaller`: runs the download's installer and binds the executable it installs.
/// False only when nothing could be launched.
pub async fn open(app: &AppHandle, shop: &str, object_id: &str) -> Result<bool, String> {
    let store = app.state::<Store>();
    let key = game_key(shop, object_id);
    let download = store.get("downloads", &key)?;
    let game = store.get("games", &key)?;
    let compat = if cfg!(target_os = "linux") {
        let prefs = store.get("", "userPreferences")?;
        Compat {
            prefix: Some(crate::proton::prefix_for(&game, &prefs, &key)),
            proton: crate::proton::resolve(&game, &prefs, &crate::proton::installed_versions()),
            log: app.path().chimaera_data_dir().ok().map(|d| crate::umu_launch::log_path(&d.join("logs"), &key)),
            steam_id: (shop == "steam").then(|| object_id.to_string()),
        }
    } else {
        Compat::default()
    };
    let wine_prefix = compat.prefix.as_ref().map(|p| p.to_string_lossy().into_owned()).or_else(|| game["winePrefixPath"].as_str().filter(|p| !p.is_empty()).map(String::from));
    let default = app.path().download_dir().map_err(|e| e.to_string())?;
    let root = installer::downloads_path(&store, &default);

    let Some(game_path) = installer::folder_path(&download, &root).filter(|p| p.exists()) else { return Ok(true) };
    if game_path.is_file() {
        app.opener().reveal_item_in_dir(&game_path).map_err(|e| e.to_string())?;
        return Ok(true);
    }
    if runs().running.contains_key(&key) || download["installerRunning"].as_bool() == Some(true) {
        eprintln!("[GameInstaller] installer for {object_id} is already running");
        return Ok(true);
    }

    let started_at = now_ms();
    runs().running.insert(key.clone(), started_at);
    stop_polling(&key);

    let games_root = download["downloadPath"].as_str().map(PathBuf::from).unwrap_or_else(|| root.clone());
    let lookup = download.clone();
    let found = tauri::async_runtime::spawn_blocking(move || installer::download_installer(&lookup, &root)).await.ok().flatten();
    let Some(installer_path) = found else {
        release(&mut runs().running, &key, started_at);
        let _ = app.opener().open_path(game_path.to_string_lossy(), None::<&str>);
        return Ok(true);
    };
    let launched_state = |running: bool| {
        json!({ "installerLaunched": true, "installerRunning": running, "installerStartedAt": started_at, "installerPath": installer_path.to_string_lossy() })
    };

    // umu-run blocks for its quick-exit check, so it starts off the async runtime.
    let (spawn_path, spawn_root) = (installer_path.clone(), games_root.clone());
    let waiter = tauri::async_runtime::spawn_blocking(move || spawn_installer(&spawn_path, &compat, &spawn_root)).await.ok().flatten();
    let Some(waiter) = waiter else {
        release(&mut runs().running, &key, started_at);
        let launched = app.opener().open_path(installer_path.to_string_lossy(), None::<&str>).is_ok();
        if launched {
            eprintln!("[GameInstaller] opened {} untracked", installer_path.display());
            record(app, &key, launched_state(false));
        } else {
            eprintln!("[GameInstaller] could not open {}", installer_path.display());
        }
        return Ok(launched);
    };

    eprintln!("[GameInstaller] launched {}", installer_path.display());
    runs().live.insert(key.clone(), started_at);
    // Recorded before the waiter starts, so an instant exit still finds this run's state.
    record(app, &key, launched_state(true));

    let run = Run { app: app.clone(), shop: shop.into(), object_id: object_id.into(), key: key.clone(), started_at, installer: installer_path, wine_prefix };
    std::thread::spawn(move || {
        let outcome = waiter();
        tauri::async_runtime::spawn(run.finish(outcome));
    });

    let app = app.clone();
    let object_id = object_id.to_string();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(MAX_INSTALLER_TRACKING).await;
        if release(&mut runs().running, &key, started_at) {
            eprintln!("[GameInstaller] installer for {object_id} is still open after two hours, allowing it to be relaunched");
            record(&app, &key, json!({ "installerRunning": false }));
        }
    });
    Ok(true)
}

/// `recheckInstalledExecutable`: the renderer asks again later for an installer that closed
/// without a detected executable (at most every 30 s, for a day).
pub async fn recheck(app: &AppHandle, shop: &str, object_id: &str) -> bool {
    let key = game_key(shop, object_id);
    let (download, game) = {
        let store = app.state::<Store>();
        (store.get("downloads", &key).unwrap_or(Value::Null), store.get("games", &key).unwrap_or(Value::Null))
    };
    let now = now_ms();
    let Some(started_at) = download["installerStartedAt"].as_u64().filter(|&s| s > 0) else { return false };
    let waiting = download["installerLaunched"].as_bool() == Some(true)
        && download["installerRunning"].as_bool() != Some(true)
        && now.saturating_sub(started_at) <= RECHECK_MAX_AGE_MS
        && !has_executable(&game);
    {
        let mut runs = runs();
        if !waiting || runs.pollers.contains_key(&key) || runs.live.contains_key(&key) {
            return false;
        }
        if now.saturating_sub(runs.last_recheck.get(&key).copied().unwrap_or(0)) < RECHECK_INTERVAL_MS {
            return false;
        }
        runs.last_recheck.insert(key.clone(), now);
    }
    let wine_prefix = game["winePrefixPath"].as_str().filter(|p| !p.is_empty());
    let found = search_and_bind(app, shop, object_id, started_at, wine_prefix).await;
    if found {
        runs().last_recheck.remove(&key);
        eprintln!("[GameInstaller] found the installed executable for {object_id} on a later check");
        let _ = app.emit("on-library-batch-complete", ());
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chimaera-game-installer-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn known(exe: &str) -> Vec<Known> {
        vec![Known { exe: exe.into(), name: exe.into() }]
    }

    #[test]
    fn elevated_output_reads_the_installer_exit() {
        assert!(matches!(read_elevated_output("CHIMAERA_INSTALLER_STARTED\r\nCHIMAERA_INSTALLER_EXIT=3010\r\n"), Outcome::Exit(Some(3010))));
        assert!(matches!(read_elevated_output("CHIMAERA_INSTALLER_EXIT=1223"), Outcome::Exit(Some(UAC_DECLINED_EXIT_CODE))));
        assert!(matches!(read_elevated_output("CHIMAERA_INSTALLER_STARTED\n"), Outcome::Untracked));
        assert!(matches!(read_elevated_output("Access is denied."), Outcome::OpenUntracked));
    }

    #[test]
    fn installed_search_stops_at_depth_four() {
        let dir = temp_dir("depth");
        let deep = dir.join("a/b/c/d/e");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("game.exe"), "").unwrap();
        assert_eq!(find_in_folder(&dir, &known("game.exe"), Some(INSTALLED_EXECUTABLE_MAX_DEPTH)), None);
        assert!(find_in_folder(&dir, &known("game.exe"), None).is_some());
        fs::write(dir.join("a/b/c/d/game.exe"), "").unwrap();
        assert_eq!(find_in_folder(&dir, &known("game.exe"), Some(INSTALLED_EXECUTABLE_MAX_DEPTH)), Some(["a", "b", "c", "d", "game.exe"].iter().fold(dir.clone(), |p, s| p.join(s)).to_string_lossy().into_owned()));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn changed_folders_skip_launchers_old_folders_and_the_downloads() {
        let dir = temp_dir("changed");
        for name in ["New Game", "Steam", "Downloads"] {
            fs::create_dir_all(dir.join(name)).unwrap();
        }
        let since = now_ms();
        assert_eq!(folders_changed_since(&[dir.clone()], since, &[&dir.join("Downloads")]), vec![dir.join("New Game")]);
        assert!(folders_changed_since(&[dir.clone()], since + 60_000, &[]).is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn games_drive_maps_once_and_keeps_an_existing_mapping() {
        let dir = temp_dir("games-drive");
        let (prefix, games, other) = (dir.join("pfx"), dir.join("Games"), dir.join("Other"));
        fs::create_dir_all(&games).unwrap();
        fs::create_dir_all(&other).unwrap();
        map_games_drive(&prefix, &games);
        assert_eq!(fs::read_link(prefix.join("dosdevices/g:")).unwrap(), games);
        fs::remove_file(prefix.join("dosdevices/g:")).unwrap();
        std::os::unix::fs::symlink(&other, prefix.join("dosdevices/g:")).unwrap();
        map_games_drive(&prefix, &games);
        assert_eq!(fs::read_link(prefix.join("dosdevices/g:")).unwrap(), other);
        assert!(target_directories(&games, Some(&prefix)).contains(&games));
        fs::remove_dir_all(&dir).unwrap();
    }
}
