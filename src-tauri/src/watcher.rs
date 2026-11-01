//! Process watcher and playtime: the Windows path of src/main/services/process-watcher.ts.
//! Every 2 s it matches running exes against each game's executable and tracking paths.

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

use crate::games::update;
use crate::launch::{close_launcher_window, is_batch_file, LauncherStatuses};
use crate::library::{group_winners, iso_now, now_ms, rank_better};
use crate::store::Store;

const INTERVAL: Duration = Duration::from_secs(2);

pub struct Session {
    first: Instant,
    last: Instant,
    started_at_ms: u64,
}

/// Running games by store key.
#[derive(Default)]
pub struct Sessions(Mutex<HashMap<String, Session>>);

impl Sessions {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Session>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// getGamesRunning.
    pub fn running(&self) -> Value {
        let now = Instant::now();
        Value::Array(
            self.lock()
                .iter()
                .map(|(id, s)| json!({ "id": id, "sessionDurationInMillis": now.duration_since(s.first).as_millis() as u64 }))
                .collect(),
        )
    }
}

/// Windows paths are case-insensitive and may mix separators.
pub(crate) fn path_key(path: &str) -> String {
    path.replace('/', "\\").to_lowercase()
}

/// Whether `game` is running, given the lowercased exe paths of all processes.
pub fn game_matches(game: &Value, running: &HashSet<String>) -> bool {
    let Some(exe) = game["executablePath"].as_str().filter(|e| !e.is_empty()) else { return false };
    let tracking: Vec<&str> = game["trackingExecutablePaths"].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|p| !p.is_empty()).collect();
    if is_batch_file(exe) && tracking.is_empty() {
        // A .bat exits or hands off, so with no exe to follow, watch its folder.
        let exe = path_key(exe);
        let Some((dir, _)) = exe.rsplit_once('\\') else { return false };
        let prefix = format!("{}\\", dir.trim_end_matches('\\'));
        return running.iter().any(|p| p.starts_with(&prefix));
    }
    let mut paths = tracking;
    if !is_batch_file(exe) {
        paths.push(exe);
    }
    paths.iter().any(|p| running.contains(&path_key(p)))
}

/// Patches the current record, so a rename or path change made mid-session survives the tick.
fn add_playtime(store: &Store, key: &str, delta: Duration) -> Result<(), String> {
    update(store, key, |game| {
        let total = game.get("playTimeInMilliseconds").and_then(Value::as_f64).unwrap_or(0.0) + delta.as_millis() as f64;
        game.insert("playTimeInMilliseconds".into(), json!(total));
        game.insert("lastTimePlayed".into(), json!(iso_now()));
    })
    .map(|_| ())
}

/// Ends a session: adds the last stretch of playtime and records the session, like onCloseGame.
pub fn close_session(store: &Store, key: &str, session: Session, now: Instant) -> Result<(), String> {
    add_playtime(store, key, now.duration_since(session.last))?;
    let duration = now.duration_since(session.first).as_millis() as u64;
    if duration > 0 {
        let record = json!({ "gameKey": key, "startedAt": session.started_at_ms, "endedAt": session.started_at_ms + duration, "durationMs": duration });
        // Millisecond keys sort after the imported zero-padded row ids.
        store.put("playSessions", &format!("{:013}", session.started_at_ms), &record)?;
    }
    Ok(())
}

fn running_exes(system: &mut sysinfo::System) -> HashSet<String> {
    // Always, not OnlyIfNotSet: sysinfo keys processes by PID only, and Windows recycles PIDs
    // within the 2 s tick, so a cached exe makes an unrelated process look like a game.
    system.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, true, sysinfo::ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::Always));
    system.processes().values().filter_map(|p| p.exe()).map(|exe| path_key(&exe.to_string_lossy())).collect()
}

/// The records that earn this tick's playtime. A running copy of a grouped game is credited
/// to its group's winner, so the grouped tile shows it running. When separate records share
/// one exe, the one launched last (then the best-ranked) gets it, never both.
pub fn credited_keys(games: &[(String, Value)], running: &HashSet<String>) -> HashSet<String> {
    let live: Vec<(String, Value)> = games.iter().filter(|(_, g)| !g["isDeleted"].as_bool().unwrap_or(false)).cloned().collect();
    let winners = group_winners(&live);
    let launched_later = |a: &Value, b: &Value| {
        let at = |g: &Value| g["lastLaunchedAt"].as_str().unwrap_or_default().to_string();
        at(a) > at(b) || (at(a) == at(b) && rank_better(a, b))
    };
    let mut by_exe: HashMap<String, &(String, Value)> = HashMap::new();
    for entry in live.iter().filter(|(_, g)| game_matches(g, running)) {
        let exe = path_key(entry.1["executablePath"].as_str().unwrap_or_default());
        match by_exe.get(&exe) {
            Some(best) if !launched_later(&entry.1, &best.1) => {}
            _ => {
                by_exe.insert(exe, entry);
            }
        }
    }
    by_exe.into_values().map(|(key, _)| winners.get(key).cloned().unwrap_or_else(|| key.clone())).collect()
}

// shortcut: no Steam playtime policy, Hydra sync, achievement watcher or souvenir capture on exit;
// local playtime always counts. Those hook in with Steam (F7) and achievements (F6).
fn tick(app: &AppHandle, running: &HashSet<String>, was_idle: &mut bool) -> Result<(), String> {
    let store = app.state::<Store>();
    let sessions = app.state::<Sessions>();
    let now = Instant::now();
    let games = store.entries("games")?;
    let credited = credited_keys(&games, running);
    for (key, game) in games {
        if game["isDeleted"].as_bool().unwrap_or(false) {
            continue;
        }
        let matched = credited.contains(&key);
        let mut all = sessions.lock();
        match (matched, all.get_mut(&key)) {
            (true, Some(session)) => {
                let delta = now.duration_since(session.last);
                session.last = now;
                drop(all);
                add_playtime(&store, &key, delta)?;
            }
            (true, None) => {
                all.insert(key.clone(), Session { first: now, last: now, started_at_ms: now_ms() });
                drop(all);
                app.state::<LauncherStatuses>().send(app, &key, "game_started");
                close_launcher_window(app);
                if store.get("", "userPreferences")?["hideToTrayOnGameStart"].as_bool().unwrap_or(false) {
                    if let Some(main) = app.get_webview_window("main") {
                        let _ = main.hide();
                    }
                }
            }
            (false, Some(_)) => {
                let session = all.remove(&key).unwrap();
                drop(all);
                close_session(&store, &key, session, now)?;
                if game["automaticCloudSync"].as_bool().unwrap_or(false) {
                    let (app, shop, object_id) = (app.clone(), game["shop"].as_str().unwrap_or_default().to_string(), game["objectId"].as_str().unwrap_or_default().to_string());
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = crate::google_drive::auto_backup(&app, &shop, &object_id).await {
                            eprintln!("[google-drive] auto backup of {shop}:{object_id} failed: {e}");
                        }
                    });
                }
            }
            (false, None) => {}
        }
    }
    // Running sessions carry a live duration, so only the idle case can be deduped.
    let running_games = sessions.running();
    let idle = running_games.as_array().is_some_and(Vec::is_empty);
    if !idle || !*was_idle {
        app.emit("on-games-running", [running_games]).map_err(|e| e.to_string())?;
    }
    *was_idle = idle;
    Ok(())
}

pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut system = sysinfo::System::new();
        let mut was_idle = false;
        loop {
            tokio::time::sleep(INTERVAL).await;
            let running = running_exes(&mut system);
            if let Err(error) = tick(&app, &running, &mut was_idle) {
                eprintln!("[watcher] tick failed: {error}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|p| path_key(p)).collect()
    }

    fn keys(set: HashSet<String>) -> Vec<String> {
        let mut keys: Vec<String> = set.into_iter().collect();
        keys.sort();
        keys
    }

    fn shared_pair() -> Vec<(String, Value)> {
        vec![
            ("steam:1".into(), json!({ "shop": "steam", "executablePath": r"C:\G\a.exe" })),
            ("custom:gog-1".into(), json!({ "shop": "custom", "executablePath": "c:/g/A.exe" })),
        ]
    }

    #[test]
    fn shared_exe_credits_only_the_steam_record() {
        assert_eq!(keys(credited_keys(&shared_pair(), &running(&[r"C:\G\a.exe"]))), ["steam:1"]);
    }

    #[test]
    fn different_exes_are_both_credited() {
        let games = vec![
            ("steam:1".into(), json!({ "shop": "steam", "executablePath": r"C:\G\a.exe" })),
            ("custom:x".into(), json!({ "shop": "custom", "executablePath": r"C:\H\b.exe" })),
        ];
        assert_eq!(keys(credited_keys(&games, &running(&[r"C:\G\a.exe", r"C:\H\b.exe"]))), ["custom:x", "steam:1"]);
    }

    const LN2_ART: &str = "https://shared.steamstatic.com/store_item_assets/steam/apps/860510/library_600x900_2x.jpg";

    #[test]
    fn running_copy_is_credited_to_its_group_winner() {
        let games = vec![
            ("steam:860510".into(), json!({ "shop": "steam", "objectId": "860510", "title": "LN2", "executablePath": r"C:\Steam\ln2.exe" })),
            ("custom:gog-1".into(), json!({ "shop": "custom", "launcher": "gog", "iconUrl": LN2_ART, "title": "LN2", "executablePath": r"C:\GOG\ln2.exe" })),
        ];
        assert_eq!(keys(credited_keys(&games, &running(&[r"C:\GOG\ln2.exe"]))), ["steam:860510"]);
    }

    #[test]
    fn split_copy_launched_last_gets_the_credit() {
        let games = vec![
            ("steam:860510".into(), json!({ "shop": "steam", "objectId": "860510", "title": "LN2", "executablePath": r"C:\GOG\ln2.exe", "lastLaunchedAt": "2026-10-01T00:00:00.000Z" })),
            (
                "custom:gog-1".into(),
                json!({ "shop": "custom", "launcher": "gog", "iconUrl": LN2_ART, "title": "LN2", "executablePath": r"C:\GOG\ln2.exe", "dedupeExcluded": true, "lastLaunchedAt": "2026-10-31T00:00:00.000Z" }),
            ),
        ];
        assert_eq!(keys(credited_keys(&games, &running(&[r"C:\GOG\ln2.exe"]))), ["custom:gog-1"]);
    }

    #[test]
    fn deleted_records_are_ignored() {
        let mut games = shared_pair();
        games[0].1["isDeleted"] = json!(true);
        assert_eq!(keys(credited_keys(&games, &running(&[r"C:\G\a.exe"]))), ["custom:gog-1"]);
    }

    #[test]
    fn matches_exe_and_tracking_paths_case_insensitively() {
        let game = json!({ "executablePath": "C:\\Games\\Foo\\Foo.exe", "trackingExecutablePaths": ["C:\\Games\\Foo\\bin\\FooGame.exe"] });
        assert!(game_matches(&game, &running(&["c:/games/foo/foo.EXE"])));
        assert!(game_matches(&game, &running(&["C:\\Games\\Foo\\bin\\foogame.exe"])));
        assert!(!game_matches(&game, &running(&["C:\\Games\\Bar\\Foo.exe"])));
        assert!(!game_matches(&json!({}), &running(&["C:\\x.exe"])));
    }

    #[test]
    fn batch_launchers_watch_their_folder() {
        let game = json!({ "executablePath": "C:\\Games\\Foo\\start.bat" });
        assert!(game_matches(&game, &running(&["C:\\Games\\Foo\\sub\\game.exe"])));
        assert!(!game_matches(&game, &running(&["C:\\Games\\Foobar\\game.exe"])));
        let tracked = json!({ "executablePath": "C:\\Games\\Foo\\start.bat", "trackingExecutablePaths": ["C:\\Games\\Foo\\game.exe"] });
        assert!(!game_matches(&tracked, &running(&["C:\\Games\\Foo\\other.exe"])));
        assert!(game_matches(&tracked, &running(&["C:\\Games\\Foo\\game.exe"])));
    }

    #[test]
    fn closing_a_session_adds_playtime_and_records_it() {
        let store = Store::memory().unwrap();
        store.put("games", "steam:1", &json!({ "playTimeInMilliseconds": 1000 })).unwrap();
        let first = Instant::now();
        let session = Session { first, last: first + Duration::from_secs(4), started_at_ms: 1_793_404_800_000 };
        close_session(&store, "steam:1", session, first + Duration::from_secs(5)).unwrap();
        assert_eq!(store.get("games", "steam:1").unwrap()["playTimeInMilliseconds"], 2000.0);
        let record = store.get("playSessions", "1793404800000").unwrap();
        assert_eq!((record["durationMs"].as_u64(), record["endedAt"].as_u64()), (Some(5000), Some(1_793_404_805_000)));
    }
}
