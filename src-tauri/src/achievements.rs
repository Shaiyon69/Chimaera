//! Achievements: the catalogue from Hydra, unlocks kept in the store, and a watcher
//! that reads emulator achievement files while games run. Ported from
//! src/main/services/achievements/ (achievement-watcher-manager.ts, merge-achievements.ts,
//! get-game-achievement-data.ts) and events/user/get-unlocked-achievements.ts.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::achievement_files::{self, AchievementFile, Roots, Unlock};
use crate::achievement_popup;
use crate::games::{game_key, update};
use crate::library::{group_winners, now_ms};
use crate::store::Store;

const INTERVAL: Duration = Duration::from_secs(2);
/// Folder discovery walks every emulator location and the game folder, so reuse it between ticks.
const DISCOVERY_TTL: Duration = Duration::from_secs(30);
const CATALOGUE_TTL_MS: u64 = 60 * 60 * 1000;
const UNLOCKED: &str = "unlockedAchievements";
const CATALOGUE: &str = "achievementCatalogue";

#[derive(Default)]
pub struct AchievementWatcher {
    /// Last seen modification time per file; None when it could not be read.
    mtimes: Mutex<HashMap<PathBuf, Option<SystemTime>>>,
    files: Mutex<HashMap<String, (Instant, Vec<AchievementFile>)>>,
    /// Games whose files were read once this session for the game page (firstSyncWithRemoteIfNeeded).
    synced: Mutex<HashSet<String>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn language(store: &Store) -> String {
    store.get("", "language").ok().and_then(|v| v.as_str().map(str::to_string)).filter(|l| !l.is_empty()).unwrap_or_else(|| "en".into())
}

fn preferences(store: &Store) -> Value {
    store.get("", "userPreferences").unwrap_or(Value::Null)
}

/// The game's achievement list from Hydra, cached per language. `fresh` asks for a
/// refetch once the cache is an hour old; a failed fetch falls back to the cache.
pub async fn catalogue(app: &AppHandle, shop: &str, object_id: &str, fresh: bool) -> Vec<Value> {
    if shop == "custom" {
        // shortcut: bundled games (Bloodborne trophies) get their own list in the Bloodborne slice.
        return Vec::new();
    }
    let store = app.state::<Store>();
    let key = game_key(shop, object_id);
    let lang = language(&store);
    let cached = store.get(CATALOGUE, &key).unwrap_or(Value::Null);
    let cached_list = || cached["achievements"].as_array().cloned().unwrap_or_default();
    let same_language = cached["language"].as_str() == Some(lang.as_str());
    let young = cached["updatedAt"].as_u64().is_some_and(|at| at + CATALOGUE_TTL_MS > now_ms());
    if same_language && !cached_list().is_empty() && (!fresh || young) {
        return cached_list();
    }

    let version = app.package_info().version.to_string();
    let request = json!({
        "method": "get", "url": format!("/games/{shop}/{object_id}/achievements"),
        "params": { "language": lang }, "options": { "needsAuth": false },
    });
    match crate::hydra_api::call(&store, &version, &request).await {
        Ok(Value::Array(list)) if !list.is_empty() => {
            let _ = store.put(CATALOGUE, &key, &json!({ "language": lang, "achievements": list, "updatedAt": now_ms() }));
            list
        }
        Ok(_) => cached_list(),
        Err(error) => {
            eprintln!("[achievements] catalogue for {key} failed: {error}");
            cached_list()
        }
    }
}

pub fn unlocked(store: &Store, key: &str) -> Vec<Value> {
    store.get(UNLOCKED, key).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()
}

fn upper(v: &Value) -> String {
    v["name"].as_str().unwrap_or_default().to_uppercase()
}

/// The unlocks in `found` not yet stored, uppercased, first occurrence of each name kept.
pub fn new_unlocks(stored: &[Value], found: &[Unlock]) -> Vec<Value> {
    let mut seen: HashSet<String> = stored.iter().map(upper).collect();
    found
        .iter()
        .filter(|u| seen.insert(u.name.to_uppercase()))
        .map(|u| json!({ "name": u.name.to_uppercase(), "unlockTime": u.unlock_time }))
        .collect()
}

/// resolveUnlockedAchievementCount: real unlocks that are in the catalogue (all of them without one).
pub fn unlocked_count(unlocked: &[Value], catalogue: &[Value]) -> usize {
    let valid: HashSet<String> = catalogue.iter().map(upper).collect();
    unlocked.iter().filter(|u| u["unlockTime"].as_i64().unwrap_or(0) > 0 && (valid.is_empty() || valid.contains(&upper(u)))).count()
}

/// The library, profile and game cards read these counts off the game record.
fn store_counts(store: &Store, key: &str, unlocked: &[Value], catalogue: &[Value]) -> Result<(), String> {
    let count = unlocked_count(unlocked, catalogue);
    update(store, key, |game| {
        if !catalogue.is_empty() {
            game.insert("achievementCount".into(), json!(catalogue.len()));
        }
        game.insert("unlockedAchievementCount".into(), json!(count));
    })
    .map(|_| ())
}

/// mergeAchievements: stores new unlocks and refreshes the counts. `publish` also tells
/// the open game page; it is off for the silent startup scan. `popup` is off for unlocks
/// the Steam client found, since Steam already showed its own.
pub async fn merge(app: &AppHandle, shop: &str, object_id: &str, found: &[Unlock], publish: bool, popup: bool) -> Result<usize, String> {
    let key = game_key(shop, object_id);
    let fresh = {
        let store = app.state::<Store>();
        new_unlocks(&unlocked(&store, &key), found)
    };
    if fresh.is_empty() {
        return Ok(0);
    }
    let catalogue = catalogue(app, shop, object_id, false).await;
    let store = app.state::<Store>();
    let mut all = unlocked(&store, &key);
    let previously_unlocked = all.len();
    all.extend(fresh.iter().cloned());
    store.put(UNLOCKED, &key, &Value::Array(all.clone()))?;
    store_counts(&store, &key, &all, &catalogue)?;
    eprintln!("[achievements] {key}: {} new", fresh.len());
    if publish {
        let preferences = preferences(&store);
        let list = user_achievements(&catalogue, &all, &preferences);
        app.emit(&format!("on-update-achievements-{object_id}-{shop}"), [list]).map_err(|e| e.to_string())?;
        if popup && achievement_popup::enabled(&preferences) {
            let infos = achievement_popup::notification_infos(&fresh, previously_unlocked, &catalogue);
            achievement_popup::show_achievements(app, &achievement_popup::position(&preferences), infos);
        }
    }
    Ok(fresh.len())
}

/// The game page's list: catalogue entries marked unlocked, newest unlock first,
/// then locked ones with hidden ones last (getUnlockedAchievements).
pub fn user_achievements(catalogue: &[Value], unlocked: &[Value], preferences: &Value) -> Value {
    let show_hidden = preferences["showHiddenAchievementsDescription"].as_bool().unwrap_or(false);
    let times: HashMap<String, i64> = unlocked.iter().map(|u| (upper(u), u["unlockTime"].as_i64().unwrap_or(0))).collect();
    let mut list: Vec<Value> = catalogue
        .iter()
        .map(|achievement| {
            let mut a = achievement.clone();
            match times.get(&upper(achievement)) {
                Some(time) => {
                    a["unlocked"] = json!(true);
                    a["unlockTime"] = json!(time);
                    a["imageUrl"] = Value::Null;
                }
                None => {
                    a["unlocked"] = json!(false);
                    a["unlockTime"] = Value::Null;
                    if a["icongray"].as_str().is_some_and(|g| g.ends_with('/')) {
                        a["icongray"] = a["icon"].clone();
                    }
                    if a["hidden"].as_bool().unwrap_or(false) && !show_hidden {
                        if let Some(map) = a.as_object_mut() {
                            map.remove("description");
                        }
                    }
                }
            }
            a
        })
        .collect();
    list.sort_by(|a, b| {
        let (au, bu) = (a["unlocked"].as_bool().unwrap_or(false), b["unlocked"].as_bool().unwrap_or(false));
        bu.cmp(&au).then_with(|| {
            if au {
                b["unlockTime"].as_i64().cmp(&a["unlockTime"].as_i64())
            } else {
                a["hidden"].as_bool().unwrap_or(false).cmp(&b["hidden"].as_bool().unwrap_or(false))
            }
        })
    });
    Value::Array(list)
}

#[cfg(windows)]
fn steam_path() -> Option<PathBuf> {
    let key = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER).open_subkey(r"Software\Valve\Steam").ok()?;
    key.get_value::<String, _>("SteamPath").ok().map(PathBuf::from)
}

#[cfg(not(windows))]
fn steam_path() -> Option<PathBuf> {
    None
}

fn roots(app: &AppHandle) -> Roots {
    Roots::from_env(app.path().document_dir().unwrap_or_default())
}

/// Steam-catalogue games still in the library: (shop, objectId, every install's exe).
fn watched_games(store: &Store) -> Vec<(String, String, Vec<String>)> {
    let live = live_games(store);
    let winners = group_winners(&live);
    live.iter()
        .filter(|(_, g)| g["shop"] == "steam")
        .filter_map(|(key, g)| Some(("steam".to_string(), g["objectId"].as_str()?.to_string(), group_executables(&live, &winners, key))))
        .collect()
}

fn live_games(store: &Store) -> Vec<(String, Value)> {
    store.entries("games").unwrap_or_default().into_iter().filter(|(_, g)| !g["isDeleted"].as_bool().unwrap_or(false)).collect()
}

/// The exe of `key` and of every copy grouped with it; a copy played instead of the Steam
/// record writes its achievement files next to its own exe.
fn group_executables(live: &[(String, Value)], winners: &HashMap<String, String>, key: &str) -> Vec<String> {
    let group = winners.get(key);
    let own = live.iter().filter(|(k, _)| k == key);
    let others = live.iter().filter(|(k, _)| k != key && group.is_some() && winners.get(k) == group);
    own.chain(others).filter_map(|(_, g)| g["executablePath"].as_str().map(str::to_string)).collect()
}

fn executables_for(store: &Store, key: &str) -> Vec<String> {
    let live = live_games(store);
    group_executables(&live, &group_winners(&live), key)
}

fn mtime(path: &PathBuf) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl AchievementWatcher {
    fn files_for(&self, roots: &Roots, steam: Option<&std::path::Path>, object_id: &str, exes: &[String]) -> Vec<AchievementFile> {
        if let Some((at, files)) = lock(&self.files).get(object_id) {
            if at.elapsed() < DISCOVERY_TTL {
                return files.clone();
            }
        }
        let files = achievement_files::game_files(roots, object_id, exes, steam);
        lock(&self.files).insert(object_id.to_string(), (Instant::now(), files.clone()));
        files
    }

    /// Files whose modification time moved since last look; a file seen for the first time counts.
    fn changed(&self, files: &[AchievementFile]) -> Vec<AchievementFile> {
        let mut mtimes = lock(&self.mtimes);
        files
            .iter()
            .filter(|f| {
                let now = mtime(&f.path);
                let before = mtimes.insert(f.path.clone(), now);
                now.is_some() && before != Some(now)
            })
            .cloned()
            .collect()
    }
}

/// The unlocks in `files` as (Steam client cache, every other source). Steam shows its
/// own pop-up for the first list, so only the second gets Chimaera's.
fn parse_all(files: &[AchievementFile]) -> (Vec<Unlock>, Vec<Unlock>) {
    let (steam, other): (Vec<&AchievementFile>, Vec<&AchievementFile>) = files.iter().partition(|f| f.cracker == achievement_files::Cracker::Steam);
    let parse = |list: Vec<&AchievementFile>| list.into_iter().flat_map(achievement_files::parse_file).collect();
    (parse(steam), parse(other))
}

/// Reads every game's files once without notifications (preSearchAchievements), then
/// every 2 s merges the files that changed.
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut first = true;
        loop {
            let (games, steam) = {
                let store = app.state::<Store>();
                let steam = preferences(&store)["enableSteamAchievements"].as_bool().unwrap_or(false).then(steam_path).flatten();
                (watched_games(&store), steam)
            };
            let scan_app = app.clone();
            // File discovery and parsing are blocking disk work.
            let found = tauri::async_runtime::spawn_blocking(move || {
                let watcher = scan_app.state::<AchievementWatcher>();
                let roots = roots(&scan_app);
                games
                    .into_iter()
                    .filter_map(|(shop, id, exes)| {
                        let changed = watcher.changed(&watcher.files_for(&roots, steam.as_deref(), &id, &exes));
                        (!changed.is_empty()).then(|| (shop, id, parse_all(&changed)))
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
            let (mut games_with_new, mut total_new) = (0, 0);
            for (shop, id, (steam_unlocks, other_unlocks)) in found {
                // Steam first: an unlock both report is then stored without a Chimaera pop-up.
                if let Err(error) = merge(&app, &shop, &id, &steam_unlocks, !first, false).await {
                    eprintln!("[achievements] merge {shop}:{id} failed: {error}");
                }
                // Only these count toward the startup summary pop-up.
                match merge(&app, &shop, &id, &other_unlocks, !first, !first).await {
                    Ok(0) => {}
                    Ok(count) => {
                        games_with_new += 1;
                        total_new += count;
                    }
                    Err(error) => eprintln!("[achievements] merge {shop}:{id} failed: {error}"),
                }
            }
            // Unlocks earned while Chimaera was closed get one summary pop-up after startup.
            if first && total_new > 0 {
                let preferences = preferences(&app.state::<Store>());
                if achievement_popup::enabled(&preferences) {
                    tokio::time::sleep(Duration::from_secs(4)).await;
                    achievement_popup::show_combined(&app, &achievement_popup::position(&preferences), games_with_new, total_new);
                }
            }
            first = false;
            tokio::time::sleep(INTERVAL).await;
        }
    });
}

/// `getUnlockedAchievements`: reads the game's files once per session, then answers with a fresh catalogue.
pub async fn get_unlocked_achievements(app: &AppHandle, object_id: &str, shop: &str) -> Result<Value, String> {
    let key = game_key(shop, object_id);
    let first_look = shop == "steam" && lock(&app.state::<AchievementWatcher>().synced).insert(key.clone());
    if first_look {
        let exes = executables_for(&app.state::<Store>(), &key);
        let (scan_app, id) = (app.clone(), object_id.to_string());
        let (steam_unlocks, other_unlocks) = tauri::async_runtime::spawn_blocking(move || {
            let store = scan_app.state::<Store>();
            let steam = preferences(&store)["enableSteamAchievements"].as_bool().unwrap_or(false).then(steam_path).flatten();
            parse_all(&achievement_files::game_files(&roots(&scan_app), &id, &exes, steam.as_deref()))
        })
        .await
        .map_err(|e| e.to_string())?;
        merge(app, shop, object_id, &steam_unlocks, true, false).await?;
        merge(app, shop, object_id, &other_unlocks, true, true).await?;
    }
    let catalogue = catalogue(app, shop, object_id, true).await;
    let store = app.state::<Store>();
    let all = unlocked(&store, &key);
    if !catalogue.is_empty() {
        store_counts(&store, &key, &all, &catalogue)?;
    }
    Ok(user_achievements(&catalogue, &all, &preferences(&store)))
}

/// The profile's per-game list (LocalApi `gameAchievements`), from what is cached.
pub fn profile_game_achievements(store: &Store, shop: &str, object_id: &str) -> Value {
    let key = game_key(shop, object_id);
    let times: HashMap<String, i64> = unlocked(store, &key).iter().map(|u| (upper(u), u["unlockTime"].as_i64().unwrap_or(0))).collect();
    let catalogue = store.get(CATALOGUE, &key).ok().and_then(|c| c["achievements"].as_array().cloned()).unwrap_or_default();
    Value::Array(
        catalogue
            .into_iter()
            .map(|mut a| {
                let time = times.get(&upper(&a)).copied();
                a["unlocked"] = json!(time.is_some());
                a["unlockTime"] = json!(time);
                a["imageUrl"] = Value::Null;
                a
            })
            .collect(),
    )
}

/// `resetGameAchievements`: deletes the game's emulator achievement files (never the Steam
/// client's cache), forgets the stored unlocks and tells the open game page, like Electron.
pub async fn reset(app: &AppHandle, shop: &str, object_id: &str) -> Result<(), String> {
    let key = game_key(shop, object_id);
    let game = app.state::<Store>().get("games", &key)?;
    if game.is_null() {
        return Ok(());
    }
    // Every grouped copy: their unlocks merge into this record, so leftovers would come back.
    let exes = executables_for(&app.state::<Store>(), &key);
    let files = achievement_files::game_files(&roots(app), object_id, &exes, None);
    let watcher = app.state::<AchievementWatcher>();
    {
        let mut mtimes = lock(&watcher.mtimes);
        for file in &files {
            eprintln!("[achievements] deleting {}", file.path.display());
            let _ = std::fs::remove_file(&file.path);
            mtimes.remove(&file.path);
        }
    }
    lock(&watcher.files).remove(object_id);
    lock(&watcher.synced).remove(&key);
    let store = app.state::<Store>();
    store.del(UNLOCKED, &key)?;
    update(&store, &key, |game| {
        game.insert("unlockedAchievementCount".into(), json!(0));
    })?;
    let list = get_unlocked_achievements(app, object_id, shop).await?;
    app.emit(&format!("on-update-achievements-{object_id}-{shop}"), [list]).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(name: &str, t: i64) -> Unlock {
        Unlock { name: name.into(), unlock_time: t }
    }

    #[test]
    fn keeps_only_unseen_names_uppercased() {
        let stored = vec![json!({ "name": "ACH_A", "unlockTime": 1 })];
        let fresh = new_unlocks(&stored, &[u("ach_a", 5), u("ach_b", 2), u("ACH_B", 9)]);
        assert_eq!(fresh, vec![json!({ "name": "ACH_B", "unlockTime": 2 })]);
    }

    fn names(unlocks: &[Unlock]) -> Vec<&str> {
        unlocks.iter().map(|u| u.name.as_str()).collect()
    }

    #[test]
    fn parse_all_splits_steam_client_files() {
        let dir = std::env::temp_dir().join(format!("chimaera-split-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("860510.json");
        std::fs::write(&cache, r#"[["achievements", { "data": { "vecHighlight": [{ "strID": "S1", "bAchieved": true, "rtUnlocked": 3 }] } }]]"#).unwrap();
        let goldberg = dir.join("achievements.json");
        std::fs::write(&goldberg, r#"{ "G1": { "earned": true, "earned_time": 5 } }"#).unwrap();
        let files = [
            AchievementFile { cracker: achievement_files::Cracker::Steam, path: cache },
            AchievementFile { cracker: achievement_files::Cracker::Goldberg, path: goldberg },
        ];
        let (steam, other) = parse_all(&files);
        assert_eq!(names(&steam), ["S1"]);
        assert_eq!(names(&other), ["G1"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn watches_every_install_of_a_grouped_game() {
        let store = Store::memory().unwrap();
        store.put("games", "steam:860510", &json!({ "shop": "steam", "objectId": "860510", "title": "LN2", "executablePath": r"C:\Steam\ln2.exe" })).unwrap();
        let art = "https://shared.steamstatic.com/store_item_assets/steam/apps/860510/library_600x900_2x.jpg";
        let gog = json!({ "shop": "custom", "objectId": "gog-1", "launcher": "gog", "iconUrl": art, "title": "LN2", "executablePath": r"C:\GOG\ln2.exe" });
        store.put("games", "custom:gog-1", &gog).unwrap();
        // Playing the GOG copy leaves the Steam record's exe alone, so its folder is scanned too.
        assert_eq!(watched_games(&store), vec![("steam".to_string(), "860510".to_string(), vec![r"C:\Steam\ln2.exe".to_string(), r"C:\GOG\ln2.exe".to_string()])]);
    }

    #[test]
    fn same_unlock_from_both_sources_is_new_once() {
        // The watcher merges the Steam list first, so a crack file repeating it adds nothing (and shows no pop-up).
        let from_steam = new_unlocks(&[], &[u("ACH_A", 3)]);
        assert_eq!(from_steam.len(), 1);
        assert!(new_unlocks(&from_steam, &[u("ach_a", 4)]).is_empty());
    }

    #[test]
    fn counts_only_real_catalogue_unlocks() {
        let unlocked = vec![json!({ "name": "A", "unlockTime": 1 }), json!({ "name": "GONE", "unlockTime": 1 }), json!({ "name": "B", "unlockTime": 0 })];
        let catalogue = vec![json!({ "name": "a" }), json!({ "name": "b" })];
        assert_eq!(unlocked_count(&unlocked, &catalogue), 1);
        assert_eq!(unlocked_count(&unlocked, &[]), 2);
    }

    #[test]
    fn orders_like_the_game_page() {
        let catalogue = vec![
            json!({ "name": "hidden", "hidden": true, "description": "secret", "icon": "i", "icongray": "https://x/" }),
            json!({ "name": "old", "hidden": false }),
            json!({ "name": "new", "hidden": false }),
            json!({ "name": "locked", "hidden": false, "description": "d" }),
        ];
        let unlocked = vec![json!({ "name": "OLD", "unlockTime": 1 }), json!({ "name": "NEW", "unlockTime": 2 })];
        let list = user_achievements(&catalogue, &unlocked, &Value::Null);
        let names: Vec<&str> = list.as_array().unwrap().iter().map(|a| a["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["new", "old", "locked", "hidden"]);
        assert!(list[3].get("description").is_none());
        assert_eq!(list[3]["icongray"], "i");
        assert_eq!(list[2]["description"], "d");
    }
}
