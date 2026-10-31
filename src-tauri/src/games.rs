//! Library mutations on `games/<shop>:<objectId>`, ported from src/main/events/library/.
//! Chimaera runs in local mode, so the Hydra profile calls those handlers make are dropped.

use serde_json::{json, Map, Value};

use crate::library::iso_now;
use crate::store::Store;

pub fn game_key(shop: &str, object_id: &str) -> String {
    format!("{shop}:{object_id}")
}

/// updateGameExecutablePath (src/main/helpers/update-executable-path.ts): the timestamp moves only on change.
pub fn with_executable_path(mut game: Value, path: Option<&str>) -> Value {
    if game["executablePath"].as_str() == path {
        return game;
    }
    game["executablePath"] = json!(path);
    game["executablePathUpdatedAt"] = if path.is_some() { json!(iso_now()) } else { Value::Null };
    game
}

/// Applies `patch` to a stored game. Ok(None) when the game does not exist, like Electron's early return.
pub fn update(store: &Store, key: &str, patch: impl FnOnce(&mut Map<String, Value>)) -> Result<Option<Value>, String> {
    let Value::Object(mut game) = store.get("games", key)? else { return Ok(None) };
    patch(&mut game);
    let game = Value::Object(game);
    store.put("games", key, &game)?;
    Ok(Some(game))
}

fn set(field: &'static str, value: Value) -> impl FnOnce(&mut Map<String, Value>) {
    move |game| {
        game.insert(field.into(), value);
    }
}

/// `getGameByObjectId`.
// shortcut: achievement counts come from the record as stored; the achievement memory store arrives with achievements (F6).
pub fn get_by_object_id(store: &Store, shop: &str, object_id: &str) -> Result<Value, String> {
    let key = game_key(shop, object_id);
    let Value::Object(mut game) = store.get("games", &key)? else { return Ok(Value::Null) };
    if game.get("isDeleted").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(Value::Null);
    }
    game.insert("id".into(), json!(key));
    game.insert("download".into(), store.get("downloads", &key)?);
    Ok(Value::Object(game))
}

/// `addGameToLibrary`: revives a removed game or creates one from its cached shop assets.
// shortcut: finished-download cleanup and the launchbox platform lookup arrive with downloads (F4) and emulators.
pub fn add_to_library(store: &Store, shop: &str, object_id: &str, title: &str, platform: Option<&str>) -> Result<(), String> {
    let key = game_key(shop, object_id);
    let revived = update(store, &key, |game| {
        game.insert("isDeleted".into(), json!(false));
        if game.get("addedToLibraryAt").is_none_or(Value::is_null) {
            game.insert("addedToLibraryAt".into(), json!(iso_now()));
        }
        game.insert("source".into(), json!("hydra"));
        if let Some(platform) = platform.filter(|_| game.get("platform").is_none_or(Value::is_null)) {
            game.insert("platform".into(), json!(platform));
        }
    })?;
    if revived.is_some() {
        return Ok(());
    }
    let assets = store.get("gameShopAssets", &key)?;
    let game = json!({
        "title": title,
        "iconUrl": assets["iconUrl"],
        "libraryHeroImageUrl": assets["libraryHeroImageUrl"],
        "logoImageUrl": assets["logoImageUrl"],
        "objectId": object_id,
        "shop": shop,
        "remoteId": null,
        "isDeleted": false,
        "playTimeInMilliseconds": 0,
        "lastTimePlayed": null,
        "addedToLibraryAt": iso_now(),
        "platform": platform,
        "source": "hydra",
    });
    store.put("games", &key, &game)
}

/// Gives every launcher import with a known Steam appid a `steam:<appid>` record, so the
/// grouped tile gets the Steam achievement catalogue. A key that exists at all, even
/// removed by the user, is left alone. Returns how many records it created.
pub fn backfill_steam_records(store: &Store) -> Result<usize, String> {
    let mut created = 0;
    for (_, game) in store.entries("games")? {
        let is_import = game["shop"] == "custom" && game["launcher"].is_string() && !game["isDeleted"].as_bool().unwrap_or(false);
        let Some(app_id) = is_import.then(|| crate::library::steam_app_id(&game)).flatten() else { continue };
        let key = game_key("steam", &app_id);
        if !store.get("games", &key)?.is_null() {
            continue;
        }
        add_to_library(store, "steam", &app_id, game["title"].as_str().unwrap_or_default(), None)?;
        // No shop assets are cached for a new appid yet; the import's art is already Steam's.
        update(store, &key, |steam| {
            for field in ["iconUrl", "libraryHeroImageUrl", "logoImageUrl"] {
                if steam.get(field).is_none_or(Value::is_null) {
                    steam.insert(field.into(), game[field].clone());
                }
            }
            // A hidden import must not surface again through its new Steam twin.
            if game["isConcealed"].as_bool().unwrap_or(false) {
                steam.insert("isConcealed".into(), Value::Bool(true));
            }
        })?;
        created += 1;
    }
    Ok(created)
}

/// `removeGameFromLibrary`: marks the game deleted, drops its custom art and artwork picks.
pub fn remove_from_library(store: &Store, shop: &str, object_id: &str) -> Result<(), String> {
    let key = game_key(shop, object_id);
    let game = store.get("games", &key)?;
    if game.is_null() {
        return Ok(());
    }
    // Custom games own their main art; shop games only their overrides.
    let art_fields: &[&str] = if shop == "custom" {
        &["iconUrl", "logoImageUrl", "libraryHeroImageUrl", "customCoverImageUrl"]
    } else {
        &["customIconUrl", "customLogoImageUrl", "customHeroImageUrl", "customCoverImageUrl"]
    };
    let files: Vec<String> = art_fields.iter().filter_map(|f| local_asset_file(&game[*f])).collect();
    let mut game = with_executable_path(game, None);
    game["isDeleted"] = json!(true);
    if shop != "custom" {
        for field in art_fields {
            game[*field] = Value::Null;
        }
    }
    store.put("games", &key, &game)?;
    store.del("artworkSelection", &key)?;
    delete_asset_files(&files);
    Ok(())
}

/// The file behind a `local:` asset URL.
pub fn local_asset_file(url: &Value) -> Option<String> {
    url.as_str()?.strip_prefix("local:").map(String::from)
}

/// Best effort, as in Electron: a leftover asset file only wastes disk.
pub fn delete_asset_files(files: &[String]) {
    for file in files {
        if let Err(error) = std::fs::remove_file(file) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("[games] failed to delete asset {file}: {error}");
            }
        }
    }
}

/// Plain field writes (gamemode, mangohud, playtime tracking); a missing game is a no-op.
pub fn set_field(store: &Store, shop: &str, object_id: &str, field: &'static str, value: Value) -> Result<(), String> {
    update(store, &game_key(shop, object_id), set(field, value)).map(|_| ())
}

/// `clearNewDownloadOptions`: Electron writes `undefined`, which drops the field.
pub fn clear_new_download_options(store: &Store, shop: &str, object_id: &str) -> Result<(), String> {
    update(store, &game_key(shop, object_id), |game| {
        game.remove("newDownloadOptionsCount");
    })
    .map(|_| ())
}

/// `assignGameToCollection`: local write only; the Hydra collection sync is not used in local mode.
pub fn assign_to_collection(store: &Store, shop: &str, object_id: &str, collection_ids: Value) -> Result<(), String> {
    update(store, &game_key(shop, object_id), set("collectionIds", collection_ids))?.ok_or("game/not-found-local")?;
    Ok(())
}

/// getGameSaveFolderShellPath (open-game-save-folder-core.ts): Explorer cannot open `\\?\` paths.
pub fn save_folder_shell_path(folder: &str) -> String {
    if !cfg!(windows) {
        return folder.into();
    }
    let path = folder.replace('/', r"\");
    let lower = path.to_ascii_lowercase();
    if lower.starts_with(r"\\?\unc\") {
        return format!(r"\\{}", &path[8..]);
    }
    let drive = lower.as_bytes().get(4..7);
    if lower.starts_with(r"\\?\") && drive.is_some_and(|d| d[0].is_ascii_lowercase() && d[1] == b':' && d[2] == b'\\') {
        return path[4..].into();
    }
    folder.into()
}

pub fn set_favorite(store: &Store, shop: &str, object_id: &str, favorite: bool) -> Result<(), String> {
    update(store, &game_key(shop, object_id), set("favorite", json!(favorite))).map(|_| ())
}

pub fn set_pin(store: &Store, shop: &str, object_id: &str, pin: bool) -> Result<(), String> {
    update(store, &game_key(shop, object_id), |game| {
        game.insert("isPinned".into(), json!(pin));
        game.insert("pinnedDate".into(), if pin { json!(iso_now()) } else { Value::Null });
    })
    .map(|_| ())
}

pub fn set_launch_options(store: &Store, shop: &str, object_id: &str, options: Option<&str>) -> Result<(), String> {
    let options = options.filter(|o| !o.trim().is_empty());
    update(store, &game_key(shop, object_id), set("launchOptions", json!(options))).map(|_| ())
}

/// `setGameVisibility`: returns the saved pair, as the Hydra API response did.
pub fn set_visibility(store: &Store, shop: &str, object_id: &str, field: &str, value: bool) -> Result<Value, String> {
    let key = game_key(shop, object_id);
    let game = store.get("games", &key)?;
    let field_ok = matches!(field, "isConcealed") || (field == "isHiddenFromOthers" && shop != "custom");
    if game.is_null() || game["isDeleted"].as_bool().unwrap_or(false) || !field_ok {
        return Err("game/not-found-local".into());
    }
    let mut saved = json!({
        "isHiddenFromOthers": shop != "custom" && game["isHiddenFromOthers"].as_bool().unwrap_or(false),
        "isConcealed": game["isConcealed"].as_bool().unwrap_or(false),
    });
    saved[field] = json!(value);
    update(store, &key, |game| {
        game.insert("isHiddenFromOthers".into(), saved["isHiddenFromOthers"].clone());
        game.insert("isConcealed".into(), saved["isConcealed"].clone());
    })?;
    Ok(saved)
}

/// A grouped game shows its copies' summed playtime, so a manual set or reset is meant for
/// that total: the copies' own playtime is cleared and the winner holds the whole value.
fn clear_linked_playtime(store: &Store, key: &str) -> Result<(), String> {
    let Some(view) = crate::library::linked_view(store, key)? else { return Ok(()) };
    for id in view["linkedGames"].as_array().into_iter().flatten().filter_map(|copy| copy["id"].as_str()) {
        update(store, id, |copy| {
            copy.insert("playTimeInMilliseconds".into(), json!(0));
        })?;
    }
    Ok(())
}

pub fn change_playtime(store: &Store, shop: &str, object_id: &str, seconds: f64) -> Result<(), String> {
    clear_linked_playtime(store, &game_key(shop, object_id))?;
    update(store, &game_key(shop, object_id), |game| {
        game.insert("playTimeInMilliseconds".into(), json!(seconds * 1000.0));
        game.insert("hasManuallyUpdatedPlaytime".into(), json!(true));
    })
    .map(|_| ())
}

pub fn reset_playtime(store: &Store, shop: &str, object_id: &str) -> Result<(), String> {
    clear_linked_playtime(store, &game_key(shop, object_id))?;
    update(store, &game_key(shop, object_id), |game| {
        game.insert("playTimeInMilliseconds".into(), json!(0));
        game.insert("lastTimePlayed".into(), Value::Null);
        game.insert("hasManuallyUpdatedPlaytime".into(), json!(false));
    })
    .map(|_| ())
}

/// `updateExecutablePath`; `path` is already resolved from a .lnk.
// shortcut: no background installedSizeInBytes scan of the game folder; it arrives with downloads (F4).
pub fn set_executable_path(store: &Store, shop: &str, object_id: &str, path: Option<&str>) -> Result<(), String> {
    let key = game_key(shop, object_id);
    let game = store.get("games", &key)?;
    if game.is_null() {
        return Ok(());
    }
    let mut game = with_executable_path(game, path);
    if path.is_none() {
        game["installedSizeInBytes"] = Value::Null;
        game["automaticCloudSync"] = json!(false);
    }
    store.put("games", &key, &game)
}

pub fn set_tracking_paths(store: &Store, shop: &str, object_id: &str, paths: Vec<String>) -> Result<(), String> {
    update(store, &game_key(shop, object_id), |game| {
        let at = if paths.is_empty() { Value::Null } else { json!(iso_now()) };
        game.insert("trackingExecutablePaths".into(), json!(paths));
        game.insert("trackingExecutablePathsUpdatedAt".into(), at);
    })
    .map(|_| ())
}

/// `verifyExecutablePathInUse`: the first game (deleted ones included, as in Electron) using this exe.
pub fn game_using_executable(store: &Store, path: &str) -> Result<Value, String> {
    Ok(store.entries("games")?.into_iter().map(|(_, g)| g).find(|g| g["executablePath"].as_str() == Some(path)).unwrap_or(Value::Null))
}

/// `getPlaySessions`: newest first, at most 200.
pub fn play_sessions(store: &Store, shop: &str, object_id: &str) -> Result<Value, String> {
    let key = game_key(shop, object_id);
    let mut sessions: Vec<Value> =
        store.entries("playSessions")?.into_iter().map(|(_, s)| s).filter(|s| s["gameKey"].as_str() == Some(&key)).collect();
    sessions.sort_by_key(|s| std::cmp::Reverse(s["startedAt"].as_i64().unwrap_or(0)));
    sessions.truncate(200);
    Ok(Value::Array(sessions))
}

/// `getPlaytimeSummary`: library-wide totals and the top 5; deleted games do not count.
pub fn playtime_summary(store: &Store) -> Result<Value, String> {
    let games: Vec<Value> =
        store.entries("games")?.into_iter().map(|(_, g)| g).filter(|g| !g["isDeleted"].as_bool().unwrap_or(false)).collect();
    let ms = |g: &Value| g["playTimeInMilliseconds"].as_f64().unwrap_or(0.0).trunc() as i64;
    let mut played: Vec<&Value> = games.iter().filter(|g| ms(g) > 0).collect();
    played.sort_by_key(|g| std::cmp::Reverse(ms(g)));
    Ok(json!({
        "totalPlayTimeInMilliseconds": games.iter().map(ms).sum::<i64>(),
        "gameCount": games.len(),
        "playedGameCount": played.len(),
        "mostPlayed": played.iter().take(5).map(|g| json!({
            "shop": g["shop"], "objectId": g["objectId"], "title": g["title"], "playTimeInMilliseconds": ms(g),
        })).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_timestamp_moves_only_on_change() {
        let game = json!({ "executablePath": "C:\\a.exe", "executablePathUpdatedAt": "old" });
        assert_eq!(with_executable_path(game.clone(), Some("C:\\a.exe"))["executablePathUpdatedAt"], "old");
        let moved = with_executable_path(game.clone(), Some("C:\\b.exe"));
        assert_ne!(moved["executablePathUpdatedAt"], "old");
        assert_eq!(with_executable_path(game, None)["executablePathUpdatedAt"], Value::Null);
    }

    const LN2_ART: &str = "https://shared.steamstatic.com/store_item_assets/steam/apps/860510/library_600x900_2x.jpg";

    fn gog_import(store: &Store) {
        let game = json!({ "shop": "custom", "objectId": "gog-1", "launcher": "gog", "iconUrl": LN2_ART, "title": "Little Nightmares II", "isDeleted": false });
        store.put("games", "custom:gog-1", &game).unwrap();
    }

    fn grouped_pair(store: &Store) {
        store.put("games", "steam:860510", &json!({ "shop": "steam", "objectId": "860510", "title": "LN2", "playTimeInMilliseconds": 36_000 })).unwrap();
        let gog = json!({ "shop": "custom", "objectId": "gog-1", "launcher": "gog", "iconUrl": LN2_ART, "title": "LN2", "playTimeInMilliseconds": 18_000 });
        store.put("games", "custom:gog-1", &gog).unwrap();
    }

    #[test]
    fn manual_playtime_on_a_group_sets_the_shown_total() {
        let store = Store::memory().unwrap();
        grouped_pair(&store);
        change_playtime(&store, "steam", "860510", 72.0).unwrap();
        let view = crate::library::linked_view(&store, "steam:860510").unwrap().unwrap();
        assert_eq!(view["playTimeInMilliseconds"], 72_000.0);
    }

    #[test]
    fn reset_playtime_on_a_group_clears_every_copy() {
        let store = Store::memory().unwrap();
        grouped_pair(&store);
        reset_playtime(&store, "steam", "860510").unwrap();
        let view = crate::library::linked_view(&store, "steam:860510").unwrap().unwrap();
        assert_eq!(view["playTimeInMilliseconds"], 0.0);
    }

    #[test]
    fn creates_steam_record_for_launcher_import() {
        let store = Store::memory().unwrap();
        gog_import(&store);
        assert_eq!(backfill_steam_records(&store).unwrap(), 1);
        let steam = store.get("games", "steam:860510").unwrap();
        assert_eq!((steam["shop"].as_str(), steam["objectId"].as_str(), steam["isDeleted"].as_bool()), (Some("steam"), Some("860510"), Some(false)));
        assert_eq!(steam["title"], "Little Nightmares II");
        assert_eq!(steam["iconUrl"], LN2_ART);
    }

    #[test]
    fn a_hidden_import_gets_a_hidden_steam_record() {
        let store = Store::memory().unwrap();
        gog_import(&store);
        crate::games::update(&store, "custom:gog-1", |g| {
            g.insert("isConcealed".into(), json!(true));
        })
        .unwrap();
        assert_eq!(backfill_steam_records(&store).unwrap(), 1);
        assert_eq!(store.get("games", "steam:860510").unwrap()["isConcealed"], true);
    }

    #[test]
    fn leaves_a_removed_steam_record_alone() {
        let store = Store::memory().unwrap();
        gog_import(&store);
        store.put("games", "steam:860510", &json!({ "shop": "steam", "objectId": "860510", "isDeleted": true })).unwrap();
        assert_eq!(backfill_steam_records(&store).unwrap(), 0);
        assert_eq!(store.get("games", "steam:860510").unwrap()["isDeleted"], true);
    }

    #[test]
    fn skips_non_launcher_and_deleted_records() {
        let store = Store::memory().unwrap();
        store.put("games", "custom:mine", &json!({ "shop": "custom", "objectId": "mine", "iconUrl": LN2_ART, "title": "Mine" })).unwrap();
        store.put("games", "custom:gog-2", &json!({ "shop": "custom", "objectId": "gog-2", "launcher": "gog", "iconUrl": LN2_ART, "isDeleted": true })).unwrap();
        assert_eq!(backfill_steam_records(&store).unwrap(), 0);
    }

    #[test]
    fn second_run_creates_nothing() {
        let store = Store::memory().unwrap();
        gog_import(&store);
        backfill_steam_records(&store).unwrap();
        assert_eq!(backfill_steam_records(&store).unwrap(), 0);
    }

    #[test]
    fn add_remove_and_revive() {
        let store = Store::memory().unwrap();
        store.put("gameShopAssets", "steam:620", &json!({ "iconUrl": "icon" })).unwrap();
        add_to_library(&store, "steam", "620", "Portal 2", None).unwrap();
        let game = get_by_object_id(&store, "steam", "620").unwrap();
        assert_eq!((game["id"].as_str(), game["iconUrl"].as_str()), (Some("steam:620"), Some("icon")));

        set_executable_path(&store, "steam", "620", Some("C:\\p2.exe")).unwrap();
        store.put("artworkSelection", "steam:620", &json!({})).unwrap();
        remove_from_library(&store, "steam", "620").unwrap();
        assert!(get_by_object_id(&store, "steam", "620").unwrap().is_null());
        assert!(store.get("artworkSelection", "steam:620").unwrap().is_null());
        assert!(store.get("games", "steam:620").unwrap()["executablePath"].is_null());

        add_to_library(&store, "steam", "620", "Portal 2", None).unwrap();
        assert_eq!(get_by_object_id(&store, "steam", "620").unwrap()["source"], "hydra");
    }

    #[test]
    fn visibility_rules_match_electron() {
        let store = Store::memory().unwrap();
        store.put("games", "custom:x", &json!({ "shop": "custom" })).unwrap();
        assert!(set_visibility(&store, "custom", "x", "isHiddenFromOthers", true).is_err());
        assert_eq!(set_visibility(&store, "custom", "x", "isConcealed", true).unwrap(), json!({ "isHiddenFromOthers": false, "isConcealed": true }));
        assert!(set_visibility(&store, "steam", "missing", "isConcealed", true).is_err());
    }

    #[test]
    fn collection_and_download_option_writes() {
        let store = Store::memory().unwrap();
        store.put("games", "steam:1", &json!({ "newDownloadOptionsCount": 3 })).unwrap();
        clear_new_download_options(&store, "steam", "1").unwrap();
        assign_to_collection(&store, "steam", "1", json!(["c1"])).unwrap();
        assert_eq!(store.get("games", "steam:1").unwrap(), json!({ "collectionIds": ["c1"] }));
        assert_eq!(assign_to_collection(&store, "steam", "2", json!([])).unwrap_err(), "game/not-found-local");
    }

    #[test]
    #[cfg(windows)]
    fn save_folder_paths_drop_the_long_path_prefix() {
        assert_eq!(save_folder_shell_path(r"\\?\C:\Saves"), r"C:\Saves");
        assert_eq!(save_folder_shell_path(r"\\?\UNC\server\share"), r"\\server\share");
        assert_eq!(save_folder_shell_path("C:/Saves"), "C:/Saves");
    }

    #[test]
    fn sessions_and_summary() {
        let store = Store::memory().unwrap();
        store.put("games", "steam:1", &json!({ "shop": "steam", "objectId": "1", "title": "A", "playTimeInMilliseconds": 5000.7 })).unwrap();
        store.put("games", "steam:2", &json!({ "shop": "steam", "objectId": "2", "isDeleted": true, "playTimeInMilliseconds": 9 })).unwrap();
        store.put("playSessions", "000000000001", &json!({ "gameKey": "steam:1", "startedAt": 10 })).unwrap();
        store.put("playSessions", "1793404800000", &json!({ "gameKey": "steam:1", "startedAt": 20 })).unwrap();
        let sessions = play_sessions(&store, "steam", "1").unwrap();
        assert_eq!(sessions[0]["startedAt"], 20);
        let summary = playtime_summary(&store).unwrap();
        assert_eq!((summary["totalPlayTimeInMilliseconds"].as_i64(), summary["gameCount"].as_u64()), (Some(5000), Some(1)));
        assert_eq!(summary["mostPlayed"][0]["title"], "A");
    }
}
