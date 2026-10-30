//! Library reads, preferences and the steam250 picker, ported from
//! src/main/events/library/get-library.ts, user-preferences/ and services/steam-250.ts.

use serde_json::{json, Value};
use std::sync::{Mutex, OnceLock};

use crate::store::Store;

/// belongsToLibraryCollection: deleted games never show; concealed ones only on request.
fn in_library(game: &Value, include_concealed: bool) -> bool {
    !game["isDeleted"].as_bool().unwrap_or(false) && (include_concealed || !game["isConcealed"].as_bool().unwrap_or(false))
}

/// composeAssetsWithArtwork (src/shared/artwork-resolver.ts): the user's artwork
/// picks override the catalogue assets.
pub fn compose_assets(assets: Value, selection: &Value) -> Value {
    let Value::Object(mut assets) = assets else { return assets };
    let selected = &selection["selected"];
    if selected.is_object() {
        for (field, kind) in [("iconUrl", "icon"), ("logoImageUrl", "logo"), ("libraryHeroImageUrl", "hero"), ("coverImageUrl", "grid")] {
            if let Some(url) = selected[kind]["url"].as_str() {
                assets.insert(field.into(), json!(url));
            }
        }
        let kinds: Vec<Value> = selected.as_object().unwrap().keys().map(|k| json!(k)).collect();
        assets.insert("selectedArtworkTypes".into(), Value::Array(kinds));
    }
    Value::Object(assets)
}

/// A game's cached catalogue assets with its artwork selection applied, or Null.
pub fn composed_assets_for(store: &Store, key: &str) -> Result<Value, String> {
    let assets = store.get("gameShopAssets", key)?;
    if assets.is_null() {
        return Ok(Value::Null);
    }
    Ok(compose_assets(assets, &store.get("artworkSelection", key)?))
}

/// One getLibrary entry, built in the same field order as get-library.ts so later
/// spreads win the same way: game, download, catalogue art, then the game's own custom art.
pub fn library_entry(key: &str, game: Value, download: Value, composed: Value) -> Value {
    let mut entry = serde_json::Map::new();
    entry.insert("id".into(), json!(key));
    if let Value::Object(fields) = &game {
        entry.extend(fields.clone());
    }
    entry.insert("download".into(), download);
    if let Value::Object(fields) = &composed {
        entry.extend(fields.clone());
    }
    let title = composed["title"].as_str().filter(|t| !t.is_empty()).map(|t| json!(t)).unwrap_or_else(|| game["title"].clone());
    entry.insert("title".into(), title);
    entry.insert("platform".into(), game.get("platform").cloned().unwrap_or(Value::Null));
    for field in ["customIconUrl", "customLogoImageUrl", "customHeroImageUrl", "customCoverImageUrl"] {
        entry.insert(field.into(), game.get(field).cloned().unwrap_or(Value::Null));
    }
    Value::Object(entry)
}

/// `getLibrary`: each game with its id (store key), download record and artwork.
// shortcut: no duplicate-entry merge (dedupeLibrary) or stale installer/install-folder
// checks yet; they come with library milestone F2.
pub fn get_library(store: &Store, include_concealed: bool) -> Result<Value, String> {
    let mut out = Vec::new();
    for (key, game) in store.entries("games")?.into_iter().filter(|(_, g)| in_library(g, include_concealed)) {
        let download = store.get("downloads", &key)?;
        let composed = composed_assets_for(store, &key)?;
        out.push(library_entry(&key, game, download, composed));
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

const ASSETS_CACHE_MS: u64 = 1000 * 60 * 60 * 8;

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// `getGameAssets`: catalogue assets cached for 8 hours, artwork selection applied.
// shortcut: bundled custom games (Bloodborne, Horizon) get their art from bundledGameService
// in Electron; here a "custom" game returns its cached assets until milestone F8 ports it.
pub async fn get_game_assets(store: &Store, version: &str, object_id: &str, shop: &str, force_fresh: bool) -> Result<Value, String> {
    let key = format!("{shop}:{object_id}");
    let cached = store.get("gameShopAssets", &key)?;
    let fresh = cached["updatedAt"].as_u64().is_some_and(|at| at + ASSETS_CACHE_MS > now_ms());
    if shop == "custom" || (!force_fresh && fresh) {
        return composed_assets_for(store, &key);
    }
    let payload = json!({ "method": "get", "url": format!("/games/{shop}/{object_id}/assets"), "options": { "needsAuth": false } });
    let assets = crate::hydra_api::call(store, version, &payload).await?;
    let Value::Object(mut assets) = assets else { return Ok(Value::Null) };
    // A title the user renamed locally survives a routine refresh.
    let keep_title = cached["title"].as_str().filter(|t| !force_fresh && Some(*t) != assets.get("title").and_then(Value::as_str));
    if let Some(title) = keep_title {
        assets.insert("title".into(), json!(title));
    }
    assets.insert("updatedAt".into(), json!(now_ms()));
    store.put("gameShopAssets", &key, &Value::Object(assets))?;
    composed_assets_for(store, &key)
}

/// `getDownloadLayoutState`: queue and paused order of downloads.
// shortcut: returns the stored order as-is; normalizing it against live download
// states (normalizeDownloadLayoutState) moves here with downloads (F4).
pub fn download_layout_state(store: &Store) -> Result<Value, String> {
    let empty = json!({ "version": 1, "queueOrder": [], "pausedOrder": [] });
    if store.entries("downloads")?.is_empty() {
        return Ok(empty);
    }
    let stored = store.get("downloadLayoutState", "layout")?;
    Ok(if stored.is_null() { empty } else { stored })
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
    fn library_entry_prefers_catalogue_art_and_keeps_custom_art() {
        let game = json!({ "title": "Old", "iconUrl": "game-icon", "customCoverImageUrl": "mine" });
        let assets = compose_assets(
            json!({ "title": "Catalogue", "coverImageUrl": "shop-cover", "iconUrl": "shop-icon" }),
            &json!({ "selected": { "grid": { "url": "picked-grid" } } }),
        );
        let entry = library_entry("steam:1", game, Value::Null, assets);
        assert_eq!(entry["title"], "Catalogue");
        assert_eq!(entry["coverImageUrl"], "picked-grid");
        assert_eq!(entry["iconUrl"], "shop-icon");
        assert_eq!(entry["customCoverImageUrl"], "mine");
        assert_eq!(entry["selectedArtworkTypes"], json!(["grid"]));
        assert_eq!(library_entry("k", json!({ "title": "T" }), Value::Null, Value::Null)["title"], "T");
    }

    #[test]
    fn merges_preferences_shallowly() {
        let merged = merge_preferences(json!({ "language": "en", "a": 1 }), &json!({ "language": "pt-BR" }));
        assert_eq!(merged, json!({ "language": "pt-BR", "a": 1 }));
        assert_eq!(merge_preferences(Value::Null, &json!({ "a": 2 })), json!({ "a": 2 }));
    }
}
