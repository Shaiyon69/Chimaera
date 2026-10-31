//! Custom games and artwork picks, ported from src/main/events/library/ (add-custom-game-to-library,
//! copy-custom-game-asset, update-custom-game, update-game-custom-assets, set-game-artwork-selection).
//! Chimaera runs in local mode, so the SteamGridDB/Hydra artwork sync those handlers start is dropped.

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::games::{delete_asset_files, game_key, local_asset_file};
use crate::library::{iso_now, now_ms};
use crate::store::Store;

/// (artwork type, custom URL field, custom original-path field), as MANUAL_ASSET_FIELDS in Electron.
const ASSET_FIELDS: [(&str, &str, &str); 4] = [
    ("icon", "customIconUrl", "customOriginalIconPath"),
    ("logo", "customLogoImageUrl", "customOriginalLogoPath"),
    ("hero", "customHeroImageUrl", "customOriginalHeroPath"),
    ("grid", "customCoverImageUrl", "customOriginalCoverPath"),
];

fn fields_for(asset_type: &str) -> Result<(&'static str, &'static str), String> {
    ASSET_FIELDS.iter().find(|(t, ..)| *t == asset_type).map(|(_, url, original)| (*url, *original)).ok_or_else(|| format!("unknown artwork type: {asset_type}"))
}

/// JS `value || null`: empty strings count as missing.
fn or_null(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(s)) if !s.is_empty() => json!(s),
        _ => Value::Null,
    }
}

fn or_empty(value: Option<&Value>) -> Value {
    match or_null(value) {
        Value::Null => json!(""),
        v => v,
    }
}

fn is_nullish(value: Option<&Value>) -> bool {
    value.is_none_or(Value::is_null)
}

/// createCustomGame (src/main/helpers/create-custom-game.ts).
pub fn create_custom_game(store: &Store, object_id: &str, title: &str, executable_path: Option<&str>, icon: Option<&Value>, logo: Option<&Value>, hero: Option<&Value>) -> Result<Value, String> {
    let key = game_key("custom", object_id);
    store.put("gameShopAssets", &key, &json!({
        "updatedAt": now_ms(),
        "objectId": object_id,
        "shop": "custom",
        "title": title,
        "iconUrl": or_null(icon),
        "libraryHeroImageUrl": or_empty(hero),
        "libraryImageUrl": or_empty(icon),
        "logoImageUrl": or_empty(logo),
        "logoPosition": null,
        "coverImageUrl": or_empty(icon),
        "downloadSources": [],
    }))?;
    let game = json!({
        "title": title,
        "iconUrl": or_null(icon),
        "logoImageUrl": or_null(logo),
        "libraryHeroImageUrl": or_null(hero),
        "objectId": object_id,
        "shop": "custom",
        "remoteId": null,
        "isDeleted": false,
        "playTimeInMilliseconds": 0,
        "lastTimePlayed": null,
        "addedToLibraryAt": iso_now(),
        "executablePath": executable_path,
        "executablePathUpdatedAt": iso_now(),
        "launchOptions": null,
        "favorite": false,
        "automaticCloudSync": false,
        "hasManuallyUpdatedPlaytime": false,
    });
    store.put("games", &key, &game)?;
    Ok(game)
}

/// `addCustomGameToLibrary`: one live game per executable.
pub fn add_custom_game(store: &Store, title: &str, executable_path: &str, icon: Option<&Value>, logo: Option<&Value>, hero: Option<&Value>) -> Result<Value, String> {
    let taken = store.entries("games")?.into_iter().any(|(_, g)| g["executablePath"].as_str() == Some(executable_path) && !g["isDeleted"].as_bool().unwrap_or(false));
    if taken {
        return Err("A game with this executable path already exists in your library".into());
    }
    create_custom_game(store, &uuid::Uuid::new_v4().to_string(), title, Some(executable_path), icon, logo, hero)
}

/// `copyCustomGameAsset`: copies a picked image into `<assets>/custom-games` and returns its `local:` URL.
pub fn copy_custom_game_asset(assets_dir: &Path, source: &str, asset_type: &str) -> Result<String, String> {
    // The type becomes part of the file name, so only the four known values pass.
    fields_for(asset_type)?;
    let source = Path::new(source);
    if source.as_os_str().is_empty() || !source.is_file() {
        return Err("Source file does not exist".into());
    }
    let dir = assets_dir.join("custom-games");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let ext = source.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let destination = dir.join(format!("{asset_type}-{}{ext}", uuid::Uuid::new_v4()));
    std::fs::copy(source, &destination).map_err(|e| e.to_string())?;
    Ok(format!("local:{}", destination.to_string_lossy()))
}

/// `updateCustomGame`: a custom game's own title and art.
pub fn update_custom_game(store: &Store, params: &Value) -> Result<Value, String> {
    let key = game_key(params["shop"].as_str().unwrap_or_default(), params["objectId"].as_str().unwrap_or_default());
    let Value::Object(existing) = store.get("games", &key)? else { return Err("Game not found".into()) };
    let p = |field: &str| params.get(field);
    let cover = p("customCoverImageUrl");

    let mut pairs = vec![("iconUrl", p("iconUrl")), ("logoImageUrl", p("logoImageUrl")), ("libraryHeroImageUrl", p("libraryHeroImageUrl"))];
    if cover.is_some() {
        pairs.push(("customCoverImageUrl", cover));
    }
    let old_files: Vec<String> = pairs
        .iter()
        .filter_map(|(field, next)| {
            let file = local_asset_file(existing.get(*field)?)?;
            let changed = next.and_then(Value::as_str).is_none_or(|n| n.is_empty() || Some(n) != existing[*field].as_str());
            changed.then_some(file)
        })
        .collect();

    let next_cover = if cover.is_some() { or_null(cover) } else { or_null(existing.get("customCoverImageUrl")) };
    let mut game = existing.clone();
    game.insert("title".into(), params["title"].clone());
    for field in ["iconUrl", "logoImageUrl", "libraryHeroImageUrl"] {
        game.insert(field.into(), or_null(p(field)));
    }
    game.insert("customCoverImageUrl".into(), next_cover.clone());
    for field in ["originalIconPath", "originalLogoPath", "originalHeroPath", "customOriginalCoverPath"] {
        let kept = match or_null(p(field)) {
            Value::Null => or_null(existing.get(field)),
            v => v,
        };
        game.insert(field.into(), kept);
    }
    let game = Value::Object(game);
    store.put("games", &key, &game)?;

    if let Value::Object(mut assets) = store.get("gameShopAssets", &key)? {
        assets.insert("title".into(), params["title"].clone());
        assets.insert("iconUrl".into(), or_null(p("iconUrl")));
        assets.insert("libraryHeroImageUrl".into(), or_empty(p("libraryHeroImageUrl")));
        assets.insert("libraryImageUrl".into(), or_empty(p("iconUrl")));
        assets.insert("logoImageUrl".into(), or_empty(p("logoImageUrl")));
        let cover_url = if next_cover.is_null() { or_empty(p("iconUrl")) } else { next_cover };
        assets.insert("coverImageUrl".into(), cover_url);
        store.put("gameShopAssets", &key, &Value::Object(assets))?;
    }
    delete_asset_files(&old_files);
    Ok(game)
}

/// One artwork type in reconcileArtworkSelection; `next` is None when the renderer left the field out.
pub struct SelectionUpdate<'a> {
    pub asset_type: &'a str,
    pub previous: Option<&'a Value>,
    pub next: Option<&'a Value>,
    pub artwork_id: Option<i64>,
    pub clear: bool,
}

/// reconcileArtworkSelection (reconcile-artwork-selection.ts): keeps a SteamGridDB pick only while its art is in use.
pub fn reconcile_selection(current: &Map<String, Value>, updates: &[SelectionUpdate]) -> (Map<String, Value>, bool) {
    let mut selected = current.clone();
    let mut changed = false;
    for u in updates {
        if u.clear {
            changed |= selected.remove(u.asset_type).is_some();
            continue;
        }
        let Some(next) = u.next else { continue };
        if Some(next) == u.previous || (next.is_null() && is_nullish(u.previous)) {
            continue;
        }
        if let (Some(url), Some(id)) = (next.as_str().filter(|n| n.starts_with("local:")), u.artwork_id) {
            selected.insert(u.asset_type.into(), json!({ "url": url, "artworkId": id }));
            changed = true;
            continue;
        }
        changed |= selected.remove(u.asset_type).is_some();
    }
    (selected, changed)
}

fn write_selection(store: &Store, key: &str, shop: &Value, object_id: &Value, selected: Map<String, Value>) -> Result<Value, String> {
    if selected.is_empty() {
        store.del("artworkSelection", key)?;
        return Ok(Value::Null);
    }
    let record = json!({ "objectId": object_id, "shop": shop, "selected": selected, "updatedAt": now_ms() });
    store.put("artworkSelection", key, &record)?;
    Ok(record)
}

/// `updateGameCustomAssets`: a shop game's art overrides. Fields the renderer leaves out stay as they are.
pub fn update_game_custom_assets(store: &Store, params: &Value) -> Result<Value, String> {
    let key = game_key(params["shop"].as_str().unwrap_or_default(), params["objectId"].as_str().unwrap_or_default());
    let Value::Object(existing) = store.get("games", &key)? else { return Err("Game not found".into()) };

    let old_files: Vec<String> = ASSET_FIELDS
        .iter()
        .filter_map(|(_, field, _)| {
            let next = params.get(*field)?;
            let file = local_asset_file(existing.get(*field)?)?;
            (Some(next) != existing.get(*field)).then_some(file)
        })
        .collect();

    let mut game = existing.clone();
    game.insert("title".into(), params["title"].clone());
    for (_, url, original) in ASSET_FIELDS {
        for field in [url, original] {
            if let Some(value) = params.get(field) {
                game.insert(field.into(), value.clone());
            }
        }
    }
    let game = Value::Object(game);
    store.put("games", &key, &game)?;

    if let Value::Object(mut assets) = store.get("gameShopAssets", &key)? {
        assets.insert("title".into(), params["title"].clone());
        store.put("gameShopAssets", &key, &Value::Object(assets))?;
    }

    let clear: Vec<&str> = params["clearArtworkTypes"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let updates: Vec<SelectionUpdate> = ASSET_FIELDS
        .iter()
        .map(|(t, field, _)| SelectionUpdate {
            asset_type: t,
            previous: existing.get(*field),
            next: params.get(*field),
            artwork_id: params["customArtworkIds"][*t].as_i64(),
            clear: clear.contains(t),
        })
        .collect();
    let selection = store.get("artworkSelection", &key)?;
    let current = selection["selected"].as_object().cloned().unwrap_or_default();
    let (selected, changed) = reconcile_selection(&current, &updates);
    if changed {
        write_selection(store, &key, &params["shop"], &params["objectId"], selected)?;
    }
    delete_asset_files(&old_files);
    Ok(game)
}

/// `setGameArtworkSelection`: picks (or clears) a SteamGridDB artwork, replacing any manual override of that type.
pub fn set_artwork_selection(store: &Store, params: &Value) -> Result<Value, String> {
    let asset_type = params["type"].as_str().unwrap_or_default();
    let (url_field, original_field) = fields_for(asset_type)?;
    let key = game_key(params["shop"].as_str().unwrap_or_default(), params["objectId"].as_str().unwrap_or_default());
    let url = params["url"].as_str().filter(|u| !u.is_empty());
    let artwork_id = params.get("artworkId").filter(|id| !id.is_null());
    let clearing = params["clear"].as_bool().unwrap_or(false) || url.is_none() || artwork_id.is_none();

    let mut selected = store.get("artworkSelection", &key)?["selected"].as_object().cloned().unwrap_or_default();
    if clearing {
        selected.remove(asset_type);
    } else {
        selected.insert(asset_type.into(), json!({ "url": url, "artworkId": artwork_id }));
    }

    let mut file_to_delete = None;
    if let Value::Object(mut game) = store.get("games", &key)? {
        if !is_nullish(game.get(url_field)) || !is_nullish(game.get(original_field)) {
            file_to_delete = game.get(url_field).and_then(local_asset_file);
            game.insert(url_field.into(), Value::Null);
            game.insert(original_field.into(), Value::Null);
            store.put("games", &key, &Value::Object(game))?;
        }
    }
    let record = write_selection(store, &key, &params["shop"], &params["objectId"], selected)?;
    delete_asset_files(&file_to_delete.into_iter().collect::<Vec<_>>());
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_games_refuse_a_taken_executable() {
        let store = Store::memory().unwrap();
        let game = add_custom_game(&store, "Mine", "C:\\m.exe", Some(&json!("")), None, None).unwrap();
        assert_eq!((game["shop"].as_str(), game["iconUrl"].is_null()), (Some("custom"), true));
        let key = game_key("custom", game["objectId"].as_str().unwrap());
        assert_eq!(store.get("gameShopAssets", &key).unwrap()["libraryImageUrl"], "");
        assert!(add_custom_game(&store, "Again", "C:\\m.exe", None, None, None).is_err());
    }

    #[test]
    fn copies_assets_and_rejects_unknown_types() {
        let dir = std::env::temp_dir().join(format!("chimaera-artwork-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("pic.png");
        std::fs::write(&source, b"png").unwrap();
        let url = copy_custom_game_asset(&dir, source.to_str().unwrap(), "hero").unwrap();
        let copied = url.strip_prefix("local:").unwrap();
        assert!(copied.ends_with(".png") && Path::new(copied).is_file());
        assert!(copy_custom_game_asset(&dir, source.to_str().unwrap(), "../x").is_err());
        assert!(copy_custom_game_asset(&dir, "", "icon").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_custom_game_keeps_originals_and_cover() {
        let store = Store::memory().unwrap();
        store.put("games", "custom:c", &json!({ "title": "Old", "customCoverImageUrl": "cover", "originalIconPath": "orig" })).unwrap();
        store.put("gameShopAssets", "custom:c", &json!({ "title": "Old" })).unwrap();
        let game = update_custom_game(&store, &json!({ "shop": "custom", "objectId": "c", "title": "New", "iconUrl": "icon" })).unwrap();
        assert_eq!((game["title"].as_str(), game["customCoverImageUrl"].as_str(), game["originalIconPath"].as_str()), (Some("New"), Some("cover"), Some("orig")));
        assert_eq!(store.get("gameShopAssets", "custom:c").unwrap()["coverImageUrl"], "cover");
        assert!(update_custom_game(&store, &json!({ "shop": "custom", "objectId": "missing", "title": "x" })).is_err());
    }

    #[test]
    fn reconcile_keeps_picks_only_for_local_art_with_an_id() {
        let current = json!({ "icon": { "url": "local:a", "artworkId": 1 }, "hero": { "url": "local:h", "artworkId": 2 } });
        let current = current.as_object().unwrap();
        let (next_local, next_remote) = (json!("local:b"), json!("https://x"));
        let (selected, changed) = reconcile_selection(current, &[
            SelectionUpdate { asset_type: "icon", previous: None, next: Some(&next_local), artwork_id: Some(5), clear: false },
            SelectionUpdate { asset_type: "hero", previous: None, next: Some(&next_remote), artwork_id: None, clear: false },
            SelectionUpdate { asset_type: "logo", previous: None, next: None, artwork_id: None, clear: true },
        ]);
        assert!(changed);
        assert_eq!(selected["icon"]["artworkId"], 5);
        assert!(!selected.contains_key("hero"));
        let (_, unchanged) = reconcile_selection(current, &[SelectionUpdate { asset_type: "icon", previous: None, next: None, artwork_id: None, clear: false }]);
        assert!(!unchanged);
    }

    #[test]
    fn selection_replaces_manual_art_and_clears_to_null() {
        let store = Store::memory().unwrap();
        store.put("games", "steam:1", &json!({ "customIconUrl": "https://manual", "customOriginalIconPath": "p" })).unwrap();
        let record = set_artwork_selection(&store, &json!({ "shop": "steam", "objectId": "1", "type": "icon", "url": "https://sgdb", "artworkId": 9 })).unwrap();
        assert_eq!(record["selected"]["icon"]["artworkId"], 9);
        assert!(store.get("games", "steam:1").unwrap()["customIconUrl"].is_null());
        let cleared = set_artwork_selection(&store, &json!({ "shop": "steam", "objectId": "1", "type": "icon", "clear": true })).unwrap();
        assert!(cleared.is_null() && store.get("artworkSelection", "steam:1").unwrap().is_null());
        assert!(set_artwork_selection(&store, &json!({ "shop": "steam", "objectId": "1", "type": "bad" })).is_err());
    }
}
