//! Library reads, preferences and the steam250 picker, ported from
//! src/main/events/library/get-library.ts, user-preferences/ and services/steam-250.ts.

use serde_json::{json, Value};
use std::sync::{Mutex, OnceLock};

use crate::store::Store;

/// belongsToLibraryCollection: deleted games never show; concealed ones only on request.
fn in_library(game: &Value, include_concealed: bool) -> bool {
    !game["isDeleted"].as_bool().unwrap_or(false) && (include_concealed || !game["isConcealed"].as_bool().unwrap_or(false))
}

/// `getLibrary`: each game with its id (store key) and download record.
// shortcut: no duplicate-entry merge (dedupeLibrary), catalogue artwork composition or
// stale installer/install-folder checks yet; they come with library milestone F2.
pub fn get_library(store: &Store, include_concealed: bool) -> Result<Value, String> {
    let games = store.entries("games")?;
    let mut out = Vec::new();
    for (key, game) in games.into_iter().filter(|(_, g)| in_library(g, include_concealed)) {
        let download = store.get("downloads", &key)?;
        let mut entry = json!({ "id": key });
        if let (Value::Object(target), Value::Object(fields)) = (&mut entry, game) {
            target.extend(fields);
        }
        entry["download"] = download;
        if entry.get("platform").is_none() {
            entry["platform"] = Value::Null;
        }
        out.push(entry);
    }
    Ok(Value::Array(out))
}

/// `updateUserPreferences` merge: the patch's top-level fields replace the stored ones.
// shortcut: download-directory normalization (getDownloadDirectoryPreferences) moves here with downloads (F4).
pub fn merge_preferences(stored: Value, patch: &Value) -> Value {
    let mut merged = match stored {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    if let Value::Object(fields) = patch {
        merged.extend(fields.clone());
    }
    Value::Object(merged)
}

/// Gregorian year for a Unix time, without a date crate (Howard Hinnant's civil_from_days).
fn year_from_unix(secs: u64) -> i64 {
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    yoe + era * 400 + i64::from(mp >= 10)
}

/// `<a ... data-title="Name" ... href=".../app/620">` entries, as the jsdom query read them.
pub fn parse_steam250(html: &str) -> Vec<Value> {
    let attr = |tag: &str, name: &str| -> Option<String> {
        let start = tag.find(&format!("{name}=\""))? + name.len() + 2;
        let end = tag[start..].find('"')? + start;
        Some(tag[start..end].replace("&amp;", "&").replace("&#39;", "'").replace("&quot;", "\""))
    };
    html.split("<a ")
        .skip(1)
        .filter_map(|rest| {
            let tag = &rest[..rest.find('>')?];
            let title = attr(tag, "data-title")?;
            let href = attr(tag, "href")?;
            let object_id = href.trim_end_matches('/').rsplit('/').next()?.to_string();
            (!object_id.is_empty()).then(|| json!({ "title": title, "objectId": object_id }))
        })
        .collect()
}

static STEAM250: OnceLock<Mutex<Vec<Value>>> = OnceLock::new();

/// `getRandomGame`: a random steam250 pick, list fetched once per run. "" when the site is unreachable.
pub async fn random_steam250_game() -> Result<Value, String> {
    let cache = STEAM250.get_or_init(|| Mutex::new(Vec::new()));
    if cache.lock().unwrap_or_else(|e| e.into_inner()).is_empty() {
        let year = year_from_unix(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
        let mut games: Vec<Value> = Vec::new();
        for path in ["/hidden_gems".to_string(), format!("/{year}"), "/top250".into(), "/most_played".into()] {
            let Ok(response) = reqwest::get(format!("https://steam250.com{path}")).await else { continue };
            let Ok(html) = response.text().await else { continue };
            for game in parse_steam250(&html) {
                if !games.iter().any(|g| g["objectId"] == game["objectId"]) {
                    games.push(game);
                }
            }
        }
        *cache.lock().unwrap_or_else(|e| e.into_inner()) = games;
    }
    let games = cache.lock().unwrap_or_else(|e| e.into_inner());
    // shortcut: a random pick per call can repeat, unlike Electron's shuffled cycle; fine for a "surprise me" button.
    Ok(if games.is_empty() { json!("") } else { games[fastrand::usize(..games.len())].clone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_year() {
        assert_eq!(year_from_unix(0), 1970);
        assert_eq!(year_from_unix(1_793_404_800), 2026); // 2026-10-31
        assert_eq!(year_from_unix(1_798_761_600), 2027); // 2027-01-01
    }

    #[test]
    fn parses_steam250_links() {
        let html = r#"<div><a class="x" data-title="Half-Life &amp; Co" href="https://store.steampowered.com/app/70">x</a><a href="/other">y</a></div>"#;
        assert_eq!(parse_steam250(html), vec![json!({ "title": "Half-Life & Co", "objectId": "70" })]);
    }

    #[test]
    fn library_hides_deleted_and_concealed() {
        assert!(in_library(&json!({}), false));
        assert!(!in_library(&json!({ "isDeleted": true }), true));
        assert!(!in_library(&json!({ "isConcealed": true }), false));
        assert!(in_library(&json!({ "isConcealed": true }), true));
    }

    #[test]
    fn merges_preferences_shallowly() {
        let merged = merge_preferences(json!({ "language": "en", "a": 1 }), &json!({ "language": "pt-BR" }));
        assert_eq!(merged, json!({ "language": "pt-BR", "a": 1 }));
        assert_eq!(merge_preferences(Value::Null, &json!({ "a": 2 })), json!({ "a": 2 }));
    }
}
