//! Global trackers, ported from src/main/helpers/global-trackers.ts and
//! events/torrenting/save-global-trackers.ts. The URL list is cached in the root
//! key `globalTrackersUrlCache` and refreshed in the background once an hour.

use std::collections::HashSet;
use std::sync::Mutex;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::library::now_ms;
use crate::store::Store;

const CACHE_KEY: &str = "globalTrackersUrlCache";
const MAX_TRACKER_LIST_SIZE: usize = 200_000;
const CACHE_TTL_MS: u64 = 60 * 60 * 1000;

/// URLs being fetched now, so a burst of downloads starts one fetch per URL.
static PENDING: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn valid_with_host(url: &str, schemes: &[&str]) -> bool {
    !url.contains(":///") && url::Url::parse(url).is_ok_and(|u| schemes.contains(&u.scheme()) && u.host_str().is_some_and(|h| !h.is_empty()))
}

pub fn is_valid_tracker_url(url: &str) -> bool {
    valid_with_host(url, &["http", "https", "udp", "ws", "wss"])
}

fn is_valid_list_url(url: &str) -> bool {
    valid_with_host(url, &["http", "https"])
}

/// One tracker per line; blank lines and `#` comments skipped; duplicates dropped in order.
fn parse_tracker_list(data: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    data.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#') && seen.insert(*l)).map(str::to_string).collect()
}

fn dedupe(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    items.into_iter().filter(|t| seen.insert(t.clone())).collect()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect()
}

async fn fetch_list(url: &str) -> Result<Vec<String>, String> {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build().map_err(|e| e.to_string())?;
    let mut response = client.get(url).send().await.and_then(reqwest::Response::error_for_status).map_err(|e| e.to_string())?;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_TRACKER_LIST_SIZE {
            return Err("Tracker list is too large".into());
        }
    }
    Ok(parse_tracker_list(&String::from_utf8_lossy(&body)).into_iter().filter(|t| is_valid_tracker_url(t)).collect())
}

fn refresh_in_background(app: &AppHandle, url: String) {
    if !PENDING.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new).insert(url.clone()) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match fetch_list(&url).await {
            Ok(trackers) => {
                let _ = app.state::<Store>().put("", CACHE_KEY, &json!({ "url": url, "trackers": trackers, "updatedAt": now_ms() }));
            }
            Err(e) => eprintln!("Global tracker URL cache fetch/refresh failed: {e}"),
        }
        if let Some(set) = PENDING.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            set.remove(&url);
        }
    });
}

/// Trackers added to every torrent: the manual list and the URL list, each if enabled.
/// The URL list comes from the cache; a stale cache is refreshed for next time.
pub fn global_trackers(app: &AppHandle) -> Result<Vec<String>, String> {
    let store = app.state::<Store>();
    let prefs = store.get("", "userPreferences")?;
    let url = prefs["globalTrackersUrl"].as_str().unwrap_or_default().to_string();
    let append_manual = prefs["appendGlobalTrackers"].as_bool().unwrap_or(false);
    let append_url = prefs["appendGlobalTrackersUrl"].as_bool().unwrap_or(false);
    let mut from_url = vec![];
    if append_url && is_valid_list_url(&url) {
        let cache = store.get("", CACHE_KEY)?;
        let same = cache["url"] == url.as_str();
        if same {
            from_url = strings(&cache["trackers"]);
        }
        if !same || cache["updatedAt"].as_u64().is_none_or(|at| now_ms().saturating_sub(at) >= CACHE_TTL_MS) {
            refresh_in_background(app, url);
        }
    }
    let manual = if append_manual { strings(&prefs["globalTrackers"]) } else { vec![] };
    Ok(dedupe(manual.into_iter().chain(from_url)))
}

/// `saveGlobalTrackers(manual, url, appendManual, appendUrl)`.
pub fn save(store: &Store, manual: &Value, url: Option<&str>, append_manual: bool, append_url: bool) -> Result<(), String> {
    let mut prefs = store.get("", "userPreferences")?;
    let existed = prefs.is_object();
    let previous_url = prefs["globalTrackersUrl"].as_str().map(str::to_string);
    let url = url.unwrap_or_default().trim().to_string();
    if !existed {
        prefs = json!({});
    }
    prefs["globalTrackers"] = json!(dedupe(strings(manual).into_iter().filter(|t| is_valid_tracker_url(t))));
    prefs["appendGlobalTrackers"] = json!(append_manual);
    prefs["globalTrackersUrl"] = json!(url);
    prefs["appendGlobalTrackersUrl"] = json!(append_url);
    store.put("", "userPreferences", &prefs)?;
    if existed && (!append_url || url.is_empty() || previous_url.as_deref() != Some(url.as_str())) {
        store.del("", CACHE_KEY)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_like_tracker_list_ts() {
        assert!(is_valid_tracker_url("udp://tracker.opentrackr.org:1337/announce"));
        assert!(is_valid_tracker_url("wss://t.example/a"));
        assert!(!is_valid_tracker_url("ftp://t.example"));
        assert!(!is_valid_tracker_url("http:///nohost"));
        assert!(!is_valid_list_url("udp://t.example"));
        assert_eq!(parse_tracker_list("# c\r\n a \n\nb\na\n"), vec!["a", "b"]);
    }

    #[test]
    fn save_filters_and_drops_stale_url_cache() {
        let dir = std::env::temp_dir().join(format!("chimaera-trackers-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("t.db")).unwrap();
        store.put("", "userPreferences", &json!({ "language": "en", "globalTrackersUrl": "https://a/list" })).unwrap();
        store.put("", CACHE_KEY, &json!({ "url": "https://a/list", "trackers": [] })).unwrap();
        save(&store, &json!(["udp://x:1/a", "bad", "udp://x:1/a"]), Some(" https://b/list "), true, true).unwrap();
        let prefs = store.get("", "userPreferences").unwrap();
        assert_eq!(prefs["language"], "en");
        assert_eq!(prefs["globalTrackers"], json!(["udp://x:1/a"]));
        assert_eq!(prefs["globalTrackersUrl"], "https://b/list");
        assert_eq!(store.get("", CACHE_KEY).unwrap(), Value::Null);
    }
}
