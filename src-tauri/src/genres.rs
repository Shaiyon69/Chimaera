//! `getGameGenres`: store genres for the library and profile genre filters, fetched in
//! batches from Hydra's `/games/shop-details` and cached per game and language.

use serde_json::{json, Map, Value};

use crate::library::now_ms;
use crate::store::Store;

const SUBLEVEL: &str = "gameGenres";
const BATCH: usize = 50;
// Genres rarely change; an empty answer is cached too, so unknown games are not refetched on every open.
const TTL_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// Genre names from one shop-details entry: Steam nests them as `data.data.genres[].description`,
/// LaunchBox sends `data.genres` as plain strings.
pub fn parse_genres(entry: &Value) -> Vec<String> {
    let data = &entry["data"];
    let list = data["data"]["genres"].as_array().or_else(|| data["genres"].as_array());
    let mut names: Vec<String> = Vec::new();
    for genre in list.into_iter().flatten() {
        let name = genre.as_str().or_else(|| genre["description"].as_str()).or_else(|| genre["name"].as_str()).map(str::trim);
        if let Some(name) = name.filter(|n| !n.is_empty()) {
            if !names.iter().any(|n| n == name) {
                names.push(name.to_string());
            }
        }
    }
    names
}

fn has_genres(shop: &str) -> bool {
    matches!(shop, "steam" | "launchbox")
}

/// `games` is `[{ shop, objectId }]`; returns `{ "shop:objectId": [genre, ...] }`.
pub async fn get(store: &Store, version: &str, games: &Value, language: &str) -> Result<Value, String> {
    let mut out = Map::new();
    let mut missing: Vec<(String, String)> = Vec::new();
    for game in games.as_array().into_iter().flatten() {
        let (Some(shop), Some(object_id)) = (game["shop"].as_str(), game["objectId"].as_str()) else { continue };
        let key = format!("{shop}:{object_id}");
        if out.contains_key(&key) || !has_genres(shop) {
            continue;
        }
        let cached = store.get(SUBLEVEL, &format!("{key}:{language}"))?;
        let fresh = cached["updatedAt"].as_u64().is_some_and(|at| at + TTL_MS > now_ms());
        if fresh {
            out.insert(key, cached["genres"].clone());
        } else {
            out.insert(key.clone(), if cached["genres"].is_array() { cached["genres"].clone() } else { json!([]) });
            missing.push((shop.to_string(), object_id.to_string()));
        }
    }

    for shop in ["steam", "launchbox"] {
        let ids: Vec<&str> = missing.iter().filter(|(s, _)| s == shop).map(|(_, id)| id.as_str()).collect();
        for chunk in ids.chunks(BATCH) {
            let request = json!({
                "method": "post", "url": "/games/shop-details",
                "data": { "shop": shop, "objectIds": chunk, "language": language },
                "options": { "needsAuth": false },
            });
            // A failed batch keeps the stale or empty answer and retries next time (nothing is cached).
            let entries = match crate::hydra_api::call(store, version, &request).await {
                Ok(Value::Array(entries)) => entries,
                Ok(_) => continue,
                Err(error) => {
                    eprintln!("[genres] {shop} batch failed: {error}");
                    continue;
                }
            };
            for object_id in chunk {
                let entry = entries.iter().find(|e| e["objectId"].as_str() == Some(object_id)).unwrap_or(&Value::Null);
                let genres = json!(parse_genres(entry));
                let key = format!("{shop}:{object_id}");
                store.put(SUBLEVEL, &format!("{key}:{language}"), &json!({ "genres": genres, "updatedAt": now_ms() }))?;
                out.insert(key, genres);
            }
        }
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_steam_and_launchbox_shapes() {
        let steam = json!({ "data": { "data": { "genres": [{ "id": "1", "description": "Action" }, { "id": "25", "description": "Adventure" }] } } });
        assert_eq!(parse_genres(&steam), ["Action", "Adventure"]);
        let launchbox = json!({ "data": { "genres": ["Platform", " ", "Platform", "Puzzle"] } });
        assert_eq!(parse_genres(&launchbox), ["Platform", "Puzzle"]);
        assert!(parse_genres(&Value::Null).is_empty());
    }
}
