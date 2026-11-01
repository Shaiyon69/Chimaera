//! RetroAchievements (ports of `services/retro-achievements/` and the `user/` events):
//! progress from retroachievements.org merged into the stored unlocks, the LaunchBox
//! reset, and the connection window.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::achievements::{self, CATALOGUE, UNLOCKED};
use crate::games::game_key;
use crate::store::Store;

const API: &str = "https://retroachievements.org/API/API_GetGameInfoAndUserProgress.php";
const WINDOW_LABEL: &str = "retroachievements-connection";

/// RetroAchievements dates (`2024-01-31 18:05:00`, UTC) to Unix milliseconds.
fn to_millis(date: Option<&str>) -> Option<i64> {
    let (day, time) = date?.trim().split_once([' ', 'T'])?;
    let d: Vec<i64> = day.split('-').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    let t: Vec<i64> = time.trim_end_matches('Z').split(':').map(|p| p.parse::<f64>().ok().map(|v| v as i64)).collect::<Option<_>>()?;
    if d.len() != 3 || t.len() < 2 {
        return None;
    }
    let days = crate::http_download::days_from_civil(d[0], d[1], d[2]);
    Some((days * 86_400 + t[0] * 3600 + t[1] * 60 + t.get(2).copied().unwrap_or(0)) * 1000)
}

fn upper(v: &Value) -> String {
    v["name"].as_str().unwrap_or_default().to_uppercase()
}

/// Unlocked first (newest first), then locked with hidden ones last.
fn sort_key(a: &Value, b: &Value) -> std::cmp::Ordering {
    let (ua, ub) = (a["unlocked"] == json!(true), b["unlocked"] == json!(true));
    match (ua, ub) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        (true, true) => b["unlockTime"].as_i64().cmp(&a["unlockTime"].as_i64()),
        _ => a["hidden"].as_bool().unwrap_or(false).cmp(&b["hidden"].as_bool().unwrap_or(false)),
    }
}

/// buildRetroAchievementsView. Souvenir images live on Hydra's servers, so `imageUrl` stays null.
fn view(catalogue: &[Value], unlocked: &[Value]) -> Value {
    let by_name: HashMap<String, &Value> = unlocked.iter().map(|u| (upper(u), u)).collect();
    let mut list: Vec<Value> = catalogue
        .iter()
        .map(|a| {
            let hit = by_name.get(&upper(a));
            let mut out = a.clone();
            out["unlocked"] = json!(hit.is_some());
            out["unlockTime"] = hit.map_or(Value::Null, |u| u["unlockTime"].clone());
            out["hardcoreUnlockTime"] = hit.map_or(Value::Null, |u| u.get("hardcoreUnlockTime").cloned().unwrap_or(Value::Null));
            out["imageUrl"] = Value::Null;
            out["source"] = json!("retroachievements");
            out
        })
        .collect();
    list.sort_by(sort_key);
    Value::Array(list)
}

fn stored_catalogue(store: &Store, key: &str) -> Vec<Value> {
    store.get(CATALOGUE, key).ok().and_then(|c| c["achievements"].as_array().cloned()).unwrap_or_default()
}

fn cache_view(store: &Store, key: &str) -> Value {
    view(&stored_catalogue(store, key), &achievements::unlocked(store, key))
}

/// Merges the site's progress into the stored unlocks; returns the new-unlock count.
fn merge_remote(stored: Vec<Value>, remote: &Value) -> (Vec<Value>, usize) {
    let mut order: Vec<String> = stored.iter().map(upper).collect();
    let mut by_name: HashMap<String, Value> = stored.into_iter().map(|u| (upper(&u), u)).collect();
    let mut new = 0;
    for achievement in remote.as_object().into_iter().flat_map(|m| m.values()) {
        let Some(id) = achievement["ID"].as_i64().map(|i| i.to_string()).or_else(|| achievement["ID"].as_str().map(str::to_string)) else { continue };
        let key = id.to_uppercase();
        let hardcore = to_millis(achievement["DateEarnedHardcore"].as_str());
        let unlock = to_millis(achievement["DateEarned"].as_str()).or(hardcore);
        match by_name.get_mut(&key) {
            None => {
                if let Some(t) = unlock {
                    by_name.insert(key.clone(), json!({ "name": id, "unlockTime": t, "hardcoreUnlockTime": hardcore }));
                    order.push(key);
                    new += 1;
                }
            }
            Some(existing) => {
                if let Some(t) = unlock {
                    existing["unlockTime"] = json!(t);
                }
                if let Some(h) = hardcore {
                    existing["hardcoreUnlockTime"] = json!(h);
                }
            }
        }
    }
    (order.into_iter().filter_map(|k| by_name.remove(&k)).collect(), new)
}

/// resolveRetroAchievementsGameId: the caller's id, else any cached shop details, else Hydra.
async fn resolve_game_id(app: &AppHandle, shop: &str, object_id: &str, given: Option<i64>) -> Option<i64> {
    if given.is_some() {
        return given;
    }
    let store = app.state::<Store>();
    let prefix = format!("{}:", game_key(shop, object_id));
    let cached = store.entries("gameShopCache").unwrap_or_default().into_iter().find_map(|(k, v)| k.starts_with(&prefix).then(|| v["retroAchievementsGameId"].as_i64()).flatten());
    if cached.is_some() {
        return cached;
    }
    let version = app.package_info().version.to_string();
    let request = json!({ "method": "get", "url": format!("/games/{shop}/{object_id}"), "options": { "needsAuth": false } });
    crate::hydra_api::call(&store, &version, &request).await.ok()?["retroAchievementsGameId"].as_i64()
}

/// `getRetroAchievementsAchievements`. Without a RetroAchievements id it answers like
/// `getUnlockedAchievements`; on other failures, from what is stored.
pub async fn get_achievements(app: &AppHandle, object_id: &str, shop: &str, given_id: Option<i64>) -> Result<Value, String> {
    let key = game_key(shop, object_id);
    let prefs = app.state::<Store>().get("", "userPreferences").unwrap_or(Value::Null);
    let (Some(api_key), Some(username)) = (prefs["retroAchievementsWebApiKey"].as_str().filter(|k| !k.is_empty()), prefs["retroAchievementsUsername"].as_str().filter(|u| !u.is_empty())) else {
        return Ok(cache_view(&app.state::<Store>(), &key));
    };
    let Some(game_id) = resolve_game_id(app, shop, object_id, given_id).await else {
        return achievements::get_unlocked_achievements(app, object_id, shop).await;
    };
    let catalogue = achievements::catalogue(app, shop, object_id, false).await;
    let response = reqwest::Client::new()
        .get(API)
        .query(&[("u", username), ("y", api_key), ("g", &game_id.to_string())])
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .and_then(|r| r.error_for_status());
    let progress: Value = match response {
        Ok(r) => r.json().await.unwrap_or(Value::Null),
        Err(err) => {
            eprintln!("[retroachievements] progress for {key} failed: {err}");
            return Ok(cache_view(&app.state::<Store>(), &key));
        }
    };
    let store = app.state::<Store>();
    let (merged, new) = merge_remote(achievements::unlocked(&store, &key), &progress["Achievements"]);
    store.put(UNLOCKED, &key, &Value::Array(merged.clone()))?;
    achievements::store_counts(&store, &key, &merged, &catalogue)?;
    eprintln!("[retroachievements] {key}: game {game_id}, {new} new unlocks");
    Ok(view(&catalogue, &merged))
}

/// `resetRetroAchievementsAchievements`: forgets every LaunchBox game's unlocks and tells
/// the open game pages. There are no pending souvenirs here, so `pendingOnly` does nothing.
pub fn reset(app: &AppHandle, pending_only: bool) -> Result<(), String> {
    if pending_only {
        return Ok(());
    }
    let store = app.state::<Store>();
    for (key, _) in store.entries(UNLOCKED)? {
        let Some(object_id) = key.strip_prefix("launchbox:") else { continue };
        store.put(UNLOCKED, &key, &json!([]))?;
        let catalogue = stored_catalogue(&store, &key);
        achievements::store_counts(&store, &key, &[], &catalogue)?;
        let locked: Vec<Value> = catalogue
            .into_iter()
            .map(|mut a| {
                a["unlocked"] = json!(false);
                a["unlockTime"] = Value::Null;
                a
            })
            .collect();
        let _ = app.emit(&format!("on-update-achievements-{object_id}-launchbox"), [locked]);
    }
    Ok(())
}

/// `openRetroAchievementsConnectionWindow`: one window over the main one, native frame like Electron on Windows.
pub fn open_window(app: &AppHandle) -> Result<(), String> {
    if let Some(existing) = app.get_webview_window(WINDOW_LABEL) {
        let _ = existing.unminimize();
        let _ = existing.set_focus();
        return Ok(());
    }
    let Some(main) = app.get_webview_window("main") else { return Ok(()) };
    let linux = cfg!(target_os = "linux");
    WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::App("tauri.html#/retroachievements-connection".into()))
        .title("Chimaera")
        // AUTH_WINDOW_CONTENT_* from src/shared/window-layout.ts, plus the custom frame on Linux.
        .inner_size(if linux { 602.0 } else { 600.0 }, if linux { 674.0 } else { 640.0 })
        .resizable(false)
        .maximizable(false)
        .minimizable(linux)
        .decorations(!linux)
        .parent(&main)
        .map_err(|e| e.to_string())?
        .center()
        .background_color(tauri::webview::Color(0x0c, 0x0c, 0x0c, 0xff))
        .initialization_script(crate::process_script())
        .build()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub fn minimize_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(WINDOW_LABEL) {
        let _ = w.minimize();
    }
}

pub fn close_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(WINDOW_LABEL) {
        let _ = w.close();
    }
}

pub fn complete_window(app: &AppHandle) {
    let _ = app.emit("on-retroachievements-connected", ());
    close_window(app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_site_dates_as_utc() {
        assert_eq!(to_millis(Some("1970-01-02 00:00:01")), Some(86_401_000));
        assert_eq!(to_millis(Some("2024-01-31 18:05:00")), Some(1_706_724_300_000));
        assert_eq!(to_millis(Some("garbage")), None);
        assert_eq!(to_millis(None), None);
    }

    #[test]
    fn merges_progress_and_sorts_view() {
        let stored = vec![json!({ "name": "10", "unlockTime": 5 })];
        let remote = json!({
            "10": { "ID": 10, "DateEarned": "1970-01-01 00:00:02", "DateEarnedHardcore": "1970-01-01 00:00:03" },
            "11": { "ID": 11, "DateEarnedHardcore": "1970-01-01 00:00:04" },
            "12": { "ID": 12 }
        });
        let (merged, new) = merge_remote(stored, &remote);
        assert_eq!(new, 1);
        assert_eq!(merged[0], json!({ "name": "10", "unlockTime": 2000, "hardcoreUnlockTime": 3000 }));
        assert_eq!(merged[1]["unlockTime"], 4000);
        let catalogue = vec![json!({ "name": "12", "hidden": true }), json!({ "name": "13" }), json!({ "name": "10" }), json!({ "name": "11" })];
        let v = view(&catalogue, &merged);
        let names: Vec<&str> = v.as_array().unwrap().iter().map(|a| a["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["11", "10", "13", "12"]);
        assert_eq!(v[0]["source"], "retroachievements");
        assert_eq!(v[2]["unlocked"], false);
    }
}
