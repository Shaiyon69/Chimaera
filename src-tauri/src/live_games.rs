//! Live checks of the Linux launch path against games installed on this machine.
//! Opt-in: `CHIMAERA_LIVE_DB=<chimaera.db> cargo test --lib live_games -- --ignored --nocapture --test-threads=1`.
//! Each test runs Chimaera's own launch, watcher and close code on a library game, then closes it.

use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::launch::{resolve_launch_command, LaunchCommand};
use crate::store::Store;
use crate::umu_launch::{Hooks, UmuStatus};

fn library_game(key: &str) -> Option<Value> {
    let db = std::env::var("CHIMAERA_LIVE_DB").ok()?;
    let conn = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let text: String = conn.query_row("select value from kv where sublevel = 'games' and key = ?1", [key], |r| r.get(0)).ok()?;
    serde_json::from_str(&text).ok()
}

fn detected(game: &Value) -> bool {
    let mut system = sysinfo::System::new();
    crate::watcher::game_matches(game, &crate::watcher::running_exes(&mut system))
}

fn wait_for(limit: Duration, mut done: impl FnMut() -> bool) -> Option<Duration> {
    let start = Instant::now();
    while start.elapsed() < limit {
        if done() {
            return Some(start.elapsed());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    None
}

fn close(key: &str, game: &Value) {
    let store = Store::memory().unwrap();
    store.put("games", key, game).unwrap();
    let (shop, id) = key.split_once(':').unwrap();
    crate::launch::close_game(&store, shop, id).unwrap();
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chimaera-live-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A Windows game Chimaera runs itself: umu-run with the game's Proton and prefix, as `proton_launch`.
fn umu_game(key: &str) {
    let Some(game) = library_game(key) else { return eprintln!("skip {key}: not in CHIMAERA_LIVE_DB") };
    let exe = game["executablePath"].as_str().unwrap().to_string();
    let prefs = json!({});
    let umu = crate::umu_updater::umu_program(&scratch()).expect("umu-run");
    let proton = crate::proton::resolve(&game, &prefs, &crate::proton::installed_versions());
    let prefix = crate::proton::prefix_for(&game, &prefs, key);
    let launch = resolve_launch_command(&exe, game["launchOptions"].as_str());
    let steam_id = key.strip_prefix("steam:");
    let command = crate::proton::umu_command(&umu, launch, &exe, proton.as_deref(), &prefix, steam_id, false, false);
    eprintln!("{key}: proton {:?}, prefix {}", proton, prefix.display());

    // Pretend to be the AppImage, the way the installed build runs: its Python paths must not reach umu-run.
    std::env::set_var("APPDIR", "/tmp/.mount_chimaeraLIVE");
    std::env::set_var("PYTHONHOME", "/tmp/.mount_chimaeraLIVE/usr/");
    std::env::set_var("PYTHONPATH", "/tmp/.mount_chimaeraLIVE/usr/share/pyshared/:");

    let log = scratch().join("umu.log");
    let statuses = Arc::new(Mutex::new(Vec::new()));
    let (exit_tx, exit_rx) = std::sync::mpsc::channel();
    let seen = statuses.clone();
    // The app answers this from the watcher's game_started; the test sets it the same way.
    let was_seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = was_seen.clone();
    let hooks = Hooks {
        on_status: Box::new(move |s| seen.lock().unwrap().push(s)),
        game_detected: Box::new(move || flag.load(std::sync::atomic::Ordering::SeqCst)),
        on_exit: Box::new(move |code| drop(exit_tx.send(code))),
    };
    let cwd = Path::new(&exe).parent().unwrap().to_path_buf();
    let started = Instant::now();
    let result = crate::umu_launch::launch(&command, &cwd, &log, crate::umu_launch::QUICK_EXIT, hooks).unwrap().wait();
    std::env::remove_var("APPDIR");
    std::env::remove_var("PYTHONHOME");
    std::env::remove_var("PYTHONPATH");
    assert!(result.is_ok(), "{key} early exit: {result:?}\n{}", std::fs::read_to_string(&log).unwrap_or_default());

    let seen_after = wait_for(Duration::from_secs(120), || detected(&game));
    was_seen.store(seen_after.is_some(), std::sync::atomic::Ordering::SeqCst);
    let mut system = sysinfo::System::new();
    system.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, true, sysinfo::ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::Always).with_cwd(sysinfo::UpdateKind::Always));
    for p in system.processes().values().filter(|p| p.cwd().is_some_and(|c| c.starts_with(&cwd))) {
        eprintln!("  in folder: name={:?} exe={:?} counts={}", p.name(), p.exe(), crate::watcher::counts_by_folder(&p.name().to_string_lossy(), p.exe()));
    }
    eprintln!("{key}: watcher saw the game after {:?} (launch at {:?})", seen_after, started.elapsed());
    std::thread::sleep(Duration::from_secs(10));
    let still_running = detected(&game);
    close(key, &game);
    let exited = exit_rx.recv_timeout(Duration::from_secs(60));
    let gone = wait_for(Duration::from_secs(30), || !detected(&game));
    let log_text = std::fs::read_to_string(&log).unwrap_or_default();
    eprintln!("{key}: statuses {:?}\n{key}: umu exit {exited:?}, gone after close {gone:?}", statuses.lock().unwrap());
    eprintln!("{key}: log tail:\n{}", log_text.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"));
    assert!(seen_after.is_some(), "{key}: the watcher never saw the game");
    assert!(still_running, "{key}: the game quit on its own within 10 s");
    assert!(gone.is_some(), "{key}: close_game left the game running");
    assert!(exited.is_ok(), "{key}: umu-run was not reaped after close");
    assert!(!statuses.lock().unwrap().iter().any(|s| matches!(s, UmuStatus::Failed(_))), "{key}: a failure was reported");
}

/// A game Steam installed: Chimaera hands it to Steam, then only the watcher follows it.
fn steam_game(key: &str) {
    let Some(game) = library_game(key) else { return eprintln!("skip {key}: not in CHIMAERA_LIVE_DB") };
    let id = key.strip_prefix("steam:").unwrap();
    assert!(crate::proton::is_steam_install(game["executablePath"].as_str().unwrap()));
    // No stdio: a Steam started by xdg-open would otherwise hold the test's output open.
    let opened = std::process::Command::new("xdg-open").arg(format!("steam://rungameid/{id}")).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().unwrap();
    assert!(opened.success());
    let seen_after = wait_for(Duration::from_secs(180), || detected(&game));
    let mut system = sysinfo::System::new();
    let running: HashSet<String> = crate::watcher::running_exes(&mut system);
    let folder = std::fs::canonicalize(Path::new(game["executablePath"].as_str().unwrap()).parent().unwrap()).unwrap();
    let root = folder.ancestors().find(|a| a.parent().is_some_and(|p| p.ends_with("steamapps/common"))).unwrap().to_path_buf();
    let in_game_dir: Vec<&String> = running.iter().filter(|r| r.contains(&crate::watcher::path_key(&root.to_string_lossy()))).collect();
    eprintln!("{key}: watcher saw it after {seen_after:?}; processes in the game folder: {in_game_dir:?}");
    if seen_after.is_some() {
        close(key, &game);
        eprintln!("{key}: gone after close {:?}", wait_for(Duration::from_secs(30), || !detected(&game)));
    }
    assert!(seen_after.is_some(), "{key}: the watcher never saw the game (playtime would stay 0)");
}

#[test]
#[ignore]
fn live_rimworld() {
    umu_game("steam:294100");
}

#[test]
#[ignore]
fn live_terraria() {
    steam_game("steam:105600");
}

#[test]
#[ignore]
fn live_tmodloader() {
    steam_game("steam:1281930");
}

/// A broken Proton must fail the launch with umu's own message, not silently.
#[test]
#[ignore]
fn live_bad_proton_fails_loudly() {
    let Some(game) = library_game("steam:294100") else { return eprintln!("skip: not in CHIMAERA_LIVE_DB") };
    let exe = game["executablePath"].as_str().unwrap().to_string();
    let umu = crate::umu_updater::umu_program(&scratch()).expect("umu-run");
    let prefix = scratch().join("pfx");
    let command = crate::proton::umu_command(&umu, LaunchCommand { command: exe.clone(), args: vec![], env: vec![] }, &exe, Some(Path::new("/nonexistent/Proton")), &prefix, None, false, false);
    let hooks = Hooks { on_status: Box::new(|_| {}), game_detected: Box::new(|| false), on_exit: Box::new(|_| {}) };
    let result = crate::umu_launch::launch(&command, Path::new(&exe).parent().unwrap(), &scratch().join("umu.log"), crate::umu_launch::QUICK_EXIT, hooks).unwrap().wait();
    eprintln!("bad proton: {result:?}");
    let early = result.expect_err("a missing Proton must not look like a started game");
    assert!(early.failure.is_some(), "no message for the launcher: {early}");
}
