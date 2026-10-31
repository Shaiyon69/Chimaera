//! Library reads, preferences and the steam250 picker, ported from
//! src/main/events/library/get-library.ts, user-preferences/ and services/steam-250.ts.

use serde_json::{json, Value};
use std::collections::HashMap;
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

/// The Steam appid a record stands for: its own id on the steam shop, else the first
/// `/steam/apps/<id>/` in the art URLs that the launcher import picked from Steam.
pub fn steam_app_id(game: &Value) -> Option<String> {
    if game["shop"] == "steam" {
        return game["objectId"].as_str().filter(|id| !id.is_empty()).map(str::to_string);
    }
    ["iconUrl", "libraryHeroImageUrl", "logoImageUrl"].iter().find_map(|field| {
        let url = game[*field].as_str()?;
        let rest = &url[url.find("/steam/apps/")? + "/steam/apps/".len()..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        (!digits.is_empty()).then_some(digits)
    })
}

/// toComparableTitle in dedupe-library.ts.
fn comparable_title(game: &Value) -> String {
    game["title"].as_str().unwrap_or_default().to_lowercase().chars().filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit()).collect()
}

/// `steam:<appid>`, else `title:<normalized>`; None for records that never group.
// LaunchBox is skipped as in Electron: one title spans several platforms.
pub fn dedupe_key(game: &Value) -> Option<String> {
    if game["shop"] == "launchbox" || game["dedupeExcluded"].as_bool().unwrap_or(false) {
        return None;
    }
    if let Some(id) = steam_app_id(game) {
        return Some(format!("steam:{id}"));
    }
    let title = comparable_title(game);
    (!title.is_empty()).then(|| format!("title:{title}"))
}

/// The dedupe rank: steam shop, has an exe, playtime, last played.
pub fn rank_better(candidate: &Value, current: &Value) -> bool {
    let rank = |g: &Value| {
        (
            g["shop"] == "steam",
            g["executablePath"].as_str().is_some_and(|e| !e.is_empty()),
            g["playTimeInMilliseconds"].as_f64().unwrap_or(0.0),
            // ISO strings in one fixed format sort in time order.
            g["lastTimePlayed"].as_str().unwrap_or_default().to_string(),
        )
    };
    rank(candidate).partial_cmp(&rank(current)) == Some(std::cmp::Ordering::Greater)
}

/// Folds copies of one game (Steam record, launcher imports, custom adds) into its best
/// record, at the position of the group's first member. Nothing is written back: the
/// winner only carries `linkedGames`, a borrowed exe and the group's summed playtime.
pub fn dedupe_library(entries: Vec<(String, Value)>) -> Vec<(String, Value)> {
    let mut keys: Vec<Option<String>> = entries.iter().map(|(_, g)| dedupe_key(g)).collect();
    // A copy with no Steam art still joins the Steam record of the same title.
    let mut appid_by_title = HashMap::new();
    for ((_, game), key) in entries.iter().zip(&keys) {
        if let Some(key) = key.as_ref().filter(|k| k.starts_with("steam:")) {
            appid_by_title.entry(format!("title:{}", comparable_title(game))).or_insert_with(|| key.clone());
        }
    }
    for key in keys.iter_mut() {
        if let Some(steam) = key.as_ref().and_then(|k| appid_by_title.get(k)) {
            *key = Some(steam.clone());
        }
    }

    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut slot_of: HashMap<String, usize> = HashMap::new();
    for (i, key) in keys.into_iter().enumerate() {
        match key.as_ref().and_then(|k| slot_of.get(k)) {
            Some(&slot) => groups[slot].push(i),
            None => {
                if let Some(key) = key {
                    slot_of.insert(key, groups.len());
                }
                groups.push(vec![i]);
            }
        }
    }
    groups.into_iter().map(|members| merge_group(&entries, &members)).collect()
}

fn merge_group(entries: &[(String, Value)], members: &[usize]) -> (String, Value) {
    let winner = members.iter().copied().fold(members[0], |best, i| if rank_better(&entries[i].1, &entries[best].1) { i } else { best });
    let (key, game) = &entries[winner];
    let mut game = game.clone();
    let losers: Vec<&(String, Value)> = members.iter().filter(|&&i| i != winner).map(|&i| &entries[i]).collect();
    if losers.is_empty() {
        return (key.clone(), game);
    }
    let linked: Vec<Value> = losers
        .iter()
        .map(|(id, g)| {
            let launcher = if g["shop"] == "steam" { json!("steam") } else { g.get("launcher").cloned().unwrap_or(Value::Null) };
            json!({ "id": id, "shop": g["shop"], "objectId": g["objectId"], "launcher": launcher, "executablePath": g.get("executablePath").cloned().unwrap_or(Value::Null) })
        })
        .collect();
    if !game["executablePath"].as_str().is_some_and(|e| !e.is_empty()) {
        if let Some(exe) = losers.iter().find_map(|(_, g)| g["executablePath"].as_str().filter(|e| !e.is_empty())) {
            game["executablePath"] = json!(exe);
        }
    }
    let playtime: f64 = members.iter().map(|&i| entries[i].1["playTimeInMilliseconds"].as_f64().unwrap_or(0.0)).sum();
    game["playTimeInMilliseconds"] = json!(playtime);
    game["linkedGames"] = Value::Array(linked);
    (key.clone(), game)
}

/// The grouped record that `getGameByObjectId` returns for a group's winner, so the
/// game page sees the linked copies and can launch them. None for any other key.
pub fn linked_view(store: &Store, key: &str) -> Result<Option<Value>, String> {
    let games = store.entries("games")?.into_iter().filter(|(_, g)| in_library(g, true)).collect();
    Ok(dedupe_library(games).into_iter().find(|(k, g)| k == key && g.get("linkedGames").is_some()).map(|(_, g)| g))
}

/// `getLibrary`: each game with its id (store key), download record and artwork.
// shortcut: no stale installer/install-folder checks yet; they come with library milestone F2.
pub fn get_library(store: &Store, include_concealed: bool) -> Result<Value, String> {
    let mut out = Vec::new();
    let games = store.entries("games")?.into_iter().filter(|(_, g)| in_library(g, include_concealed)).collect();
    for (key, game) in dedupe_library(games) {
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

pub fn now_ms() -> u64 {
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

/// (year, month, day) for days since 1970-01-01, without a date crate (Howard Hinnant's civil_from_days).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (day, month) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

fn year_from_unix(secs: u64) -> i64 {
    civil_from_days((secs / 86_400) as i64).0
}

/// `Date.prototype.toISOString` for Unix milliseconds: how Electron stored dates in LevelDB.
pub fn iso_from_ms(ms: u64) -> String {
    let (secs, millis) = (ms / 1000, ms % 1000);
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z", rem / 3600, rem / 60 % 60, rem % 60)
}

pub fn iso_now() -> String {
    iso_from_ms(now_ms())
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
        assert_eq!(iso_from_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_from_ms(1_793_448_245_007), "2026-10-31T12:04:05.007Z");
        assert_eq!(iso_from_ms(951_782_400_000), "2000-02-29T00:00:00.000Z");
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

    const LN2_ART: &str = "https://shared.steamstatic.com/store_item_assets/steam/apps/860510/library_600x900_2x.jpg";

    fn ln2_pair() -> Vec<(String, Value)> {
        vec![
            ("steam:860510".into(), json!({ "shop": "steam", "objectId": "860510", "title": "Little Nightmares II", "playTimeInMilliseconds": 1000 })),
            (
                "custom:gog-1871006055".into(),
                json!({ "shop": "custom", "objectId": "gog-1871006055", "launcher": "gog", "iconUrl": LN2_ART, "executablePath": "C:\\G\\LN2.exe", "playTimeInMilliseconds": 500, "title": "Little Nightmares 2" }),
            ),
        ]
    }

    fn keys(entries: &[(String, Value)]) -> Vec<&str> {
        entries.iter().map(|(k, _)| k.as_str()).collect()
    }

    #[test]
    fn groups_steam_and_gog_by_appid() {
        let out = dedupe_library(ln2_pair());
        assert_eq!(keys(&out), ["steam:860510"]);
        let winner = &out[0].1;
        assert_eq!(winner["executablePath"], "C:\\G\\LN2.exe");
        assert_eq!(winner["playTimeInMilliseconds"], 1500.0);
        assert_eq!(
            winner["linkedGames"],
            json!([{ "id": "custom:gog-1871006055", "shop": "custom", "objectId": "gog-1871006055", "launcher": "gog", "executablePath": "C:\\G\\LN2.exe" }])
        );
    }

    #[test]
    fn keeps_steam_copy_over_custom() {
        let out = dedupe_library(vec![
            ("custom:a".into(), json!({ "shop": "custom", "title": "Hades", "executablePath": "C:/h.exe" })),
            ("steam:1".into(), json!({ "shop": "steam", "objectId": "1", "title": "HADES" })),
        ]);
        assert_eq!(keys(&out), ["steam:1"]);
    }

    #[test]
    fn prefers_more_playtime_when_shops_match() {
        let out = dedupe_library(vec![
            ("custom:a".into(), json!({ "shop": "custom", "title": "Game", "playTimeInMilliseconds": 5 })),
            ("custom:b".into(), json!({ "shop": "custom", "title": "Game", "playTimeInMilliseconds": 50 })),
        ]);
        assert_eq!(keys(&out), ["custom:b"]);
    }

    #[test]
    fn never_merges_launchbox() {
        let out = dedupe_library(vec![
            ("launchbox:a".into(), json!({ "shop": "launchbox", "title": "Game" })),
            ("launchbox:b".into(), json!({ "shop": "launchbox", "title": "Game" })),
        ]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn different_appids_never_merge_on_title() {
        let out = dedupe_library(vec![
            ("steam:1".into(), json!({ "shop": "steam", "objectId": "1", "title": "Doom" })),
            ("steam:2".into(), json!({ "shop": "steam", "objectId": "2", "title": "DOOM" })),
        ]);
        assert_eq!(keys(&out), ["steam:1", "steam:2"]);
    }

    #[test]
    fn excluded_records_stand_alone() {
        let mut pair = ln2_pair();
        pair[1].1["dedupeExcluded"] = json!(true);
        let out = dedupe_library(pair);
        assert_eq!(keys(&out), ["steam:860510", "custom:gog-1871006055"]);
        assert!(out[0].1.get("linkedGames").is_none());
    }

    #[test]
    fn non_steam_art_falls_back_to_title() {
        assert_eq!(steam_app_id(&json!({ "shop": "custom", "iconUrl": "https://cdn.example.com/x.png" })), None);
        assert_eq!(steam_app_id(&json!({ "shop": "custom", "iconUrl": null })), None);
        assert_eq!(steam_app_id(&json!({ "shop": "custom", "logoImageUrl": LN2_ART })), Some("860510".into()));
        let out = dedupe_library(vec![
            ("custom:a".into(), json!({ "shop": "custom", "title": "Hades", "iconUrl": "https://cdn.example.com/x.png" })),
            ("custom:b".into(), json!({ "shop": "custom", "title": "HADES!", "iconUrl": null })),
        ]);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn output_keeps_store_order() {
        let out = dedupe_library(vec![
            ("custom:a".into(), json!({ "shop": "custom", "title": "Alpha" })),
            ("custom:b".into(), json!({ "shop": "custom", "title": "Hades" })),
            ("steam:1".into(), json!({ "shop": "steam", "objectId": "1", "title": "Hades" })),
        ]);
        assert_eq!(keys(&out), ["custom:a", "steam:1"]);
    }

    #[test]
    fn linked_view_only_for_group_winner() {
        let store = Store::memory().unwrap();
        for (key, game) in ln2_pair() {
            store.put("games", &key, &game).unwrap();
        }
        let view = linked_view(&store, "steam:860510").unwrap().expect("winner view");
        assert_eq!(view["executablePath"], "C:\\G\\LN2.exe");
        assert!(linked_view(&store, "custom:gog-1871006055").unwrap().is_none());
    }

    #[test]
    fn merges_preferences_shallowly() {
        let merged = merge_preferences(json!({ "language": "en", "a": 1 }), &json!({ "language": "pt-BR" }));
        assert_eq!(merged, json!({ "language": "pt-BR", "a": 1 }));
        assert_eq!(merge_preferences(Value::Null, &json!({ "a": 2 })), json!({ "a": 2 }));
    }
}
