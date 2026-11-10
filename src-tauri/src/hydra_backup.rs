//! importHydraBackup (services/hydra-backup/hydra-backup-import.ts): puts a verified
//! Hydra backup folder (made by scripts/hydra-backup-export.mjs) into the local stores:
//! library, unlocks, profile, collections, and Hydra Cloud saves as local save backups.
//! Safe to run again: playtime keeps the larger value and every unlock is kept.

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};

use crate::achievement_files::Unlock;
use crate::games::game_key;
use crate::library::now_ms;
use crate::save_backups;
use crate::store::Store;
use crate::data_location::DataDir;

const VERSION: u64 = 1;
const STEAM_ASSETS: &str = "https://shared.steamstatic.com/store_item_assets/steam/apps";

/// A path from backup.json, kept inside the backup folder.
fn inside(dir: &Path, rel: &str) -> Option<PathBuf> {
    let rel = Path::new(rel);
    rel.components().all(|c| matches!(c, Component::Normal(_))).then(|| dir.join(rel))
}

fn sha256_file(path: &Path) -> Option<String> {
    let mut hasher = Sha256::new();
    std::io::copy(&mut fs::File::open(path).ok()?, &mut hasher).ok()?;
    Some(format!("{:x}", hasher.finalize()))
}

/// readHydraBackup + validateBackup: the backup, or the error the renderer shows.
fn read(dir: &Path) -> Result<Value, String> {
    let text = fs::read_to_string(dir.join("backup.json")).map_err(|_| "backup/invalid")?;
    let backup: Value = serde_json::from_str(&text).map_err(|_| "backup/invalid")?;
    let valid = backup["version"].as_u64() == Some(VERSION) && backup["user"]["id"].is_string() && backup["data"].is_object() && backup["files"].is_array();
    if !valid {
        return Err("backup/invalid".into());
    }
    let bad = backup["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|file| {
            let Some(path) = file["path"].as_str().and_then(|rel| inside(dir, rel)) else { return true };
            let size_ok = fs::metadata(&path).ok().map(|m| m.len()) == file["sizeBytes"].as_u64();
            !size_ok || sha256_file(&path).as_deref() != file["sha256"].as_str()
        })
        .count();
    if bad > 0 {
        return Err(format!("backup/{bad}-bad-files"));
    }
    if backup["sections"]["library"]["ok"] != Value::Bool(true) {
        return Err("backup/library-missing".into());
    }
    Ok(backup)
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite())
}

/// getRemotePlayTimeParts: (hydra ms, steam ms).
fn remote_playtime(remote: &Value) -> (f64, f64) {
    let ms = |seconds: Option<f64>| seconds.filter(|s| *s >= 0.0).map_or(0.0, |s| s * 1000.0);
    let by_platform = &remote["runtimeByPlatform"];
    let (hydra, steam) = (num(&by_platform["hydra"]), num(&by_platform["steam"]));
    if hydra.is_some() || steam.is_some() {
        return (ms(hydra), ms(steam));
    }
    let seconds = num(&remote["runtime"]).or(num(&remote["playTimeInSeconds"])).or(num(&remote["playTimeInMilliseconds"]).map(|m| m / 1000.0));
    (ms(seconds), 0.0)
}

/// resolveLibrarySource
fn source(local: &Value, remote: &Value) -> &'static str {
    let is = |s: &str| local == s || remote == s;
    if is("steam") && !is("hydra") {
        "steam"
    } else {
        "hydra"
    }
}

fn collection_ids(game: &Value) -> Option<Value> {
    if game["collectionIds"].is_array() {
        return Some(game["collectionIds"].clone());
    }
    let id = game.get("collectionId")?;
    Some(if id.as_str().is_some_and(|s| !s.is_empty()) { json!([id]) } else { json!([]) })
}

/// buildMergedGame without artwork reconciliation (that needs a Hydra Cloud subscription).
fn merge_game(local: Option<Map<String, Value>>, remote: &Value) -> Value {
    let (hydra_ms, steam_ms) = remote_playtime(remote);
    let steam_import = remote["hasActiveSteamImport"] == Value::Bool(true);
    let added_at = remote["createdAt"].as_str().map_or(Value::Null, |s| json!(s));
    let max = |a: &Value, b: &Value| json!(a.as_f64().unwrap_or(0.0).max(b.as_f64().unwrap_or(0.0)));
    let Some(mut game) = local else {
        let field = |name: &str| remote.get(name).cloned().unwrap_or(Value::Null);
        let flag = |name: &str| json!(remote[name].as_bool().unwrap_or(false));
        return json!({
            "objectId": remote["objectId"],
            "title": remote["title"],
            "remoteId": field("id"),
            "shop": remote["shop"],
            "iconUrl": field("iconUrl"),
            "libraryHeroImageUrl": field("libraryHeroImageUrl"),
            "logoImageUrl": field("logoImageUrl"),
            "addedToLibraryAt": added_at,
            "lastTimePlayed": field("lastTimePlayed"),
            "playTimeInMilliseconds": hydra_ms,
            "steamPlayTimeInMilliseconds": steam_ms,
            "hasManuallyUpdatedPlaytime": flag("hasManuallyUpdatedPlaytime"),
            "isDeleted": false,
            "favorite": flag("isFavorite"),
            "isHiddenFromOthers": flag("isHiddenFromOthers"),
            "isConcealed": flag("isConcealed"),
            "isPinned": flag("isPinned"),
            "collectionIds": collection_ids(remote).unwrap_or(json!([])),
            "achievementCount": max(&remote["achievementCount"], &Value::Null),
            "unlockedAchievementCount": max(&remote["unlockedAchievementCount"], &Value::Null),
            "platform": field("platform"),
            "source": source(&Value::Null, &remote["source"]),
            "hasActiveSteamImport": steam_import,
            "customIconUrl": field("customIconUrl"),
            "customLogoImageUrl": field("customLogoImageUrl"),
            "customHeroImageUrl": field("customLibraryHeroImageUrl"),
            "customCoverImageUrl": field("customLibraryImageUrl"),
        });
    };
    let get = |g: &Map<String, Value>, k: &str| g.get(k).cloned().unwrap_or(Value::Null);
    let local_last = get(&game, "lastTimePlayed");
    let remote_last = &remote["lastTimePlayed"];
    // ISO-8601 UTC strings compare in time order.
    let later = match (local_last.as_str(), remote_last.as_str()) {
        (None, _) if local_last.is_null() => remote_last.clone(),
        (Some(l), Some(r)) if r > l => remote_last.clone(),
        _ => local_last,
    };
    let local_hydra = num(&get(&game, "playTimeInMilliseconds")).unwrap_or(0.0);
    let updates = [
        ("remoteId", if remote["id"].is_string() { remote["id"].clone() } else { get(&game, "remoteId") }),
        ("addedToLibraryAt", if get(&game, "addedToLibraryAt").is_null() { added_at } else { get(&game, "addedToLibraryAt") }),
        ("lastTimePlayed", later),
        ("playTimeInMilliseconds", json!(local_hydra.max(hydra_ms))),
        ("steamPlayTimeInMilliseconds", json!(steam_ms)),
        ("favorite", remote.get("isFavorite").filter(|v| v.is_boolean()).cloned().unwrap_or(get(&game, "favorite"))),
        ("isHiddenFromOthers", remote.get("isHiddenFromOthers").filter(|v| v.is_boolean()).cloned().unwrap_or(get(&game, "isHiddenFromOthers"))),
        ("isConcealed", json!(remote["isConcealed"].as_bool().or(game.get("isConcealed").and_then(Value::as_bool)).unwrap_or(false))),
        ("isPinned", remote.get("isPinned").filter(|v| v.is_boolean()).cloned().unwrap_or(get(&game, "isPinned"))),
        ("collectionIds", collection_ids(remote).unwrap_or_else(|| collection_ids(&Value::Object(game.clone())).unwrap_or(json!([])))),
        ("achievementCount", max(&get(&game, "achievementCount"), &remote["achievementCount"])),
        ("unlockedAchievementCount", max(&get(&game, "unlockedAchievementCount"), &remote["unlockedAchievementCount"])),
        ("platform", if remote["platform"].is_string() { remote["platform"].clone() } else { get(&game, "platform") }),
        ("source", json!(source(&get(&game, "source"), &remote["source"]))),
        ("hasActiveSteamImport", json!(steam_import)),
        // resolveLibraryIsDeleted
        ("isDeleted", if remote["source"] == "steam" || steam_import { json!(false) } else { json!(get(&game, "isDeleted").as_bool().unwrap_or(false)) }),
    ];
    for (k, v) in updates {
        game.insert(k.into(), v);
    }
    Value::Object(game)
}

/// buildRemoteGameShopAssets: fields the backup lacks keep what the cache has.
fn merge_assets(local: Value, local_title: Option<&str>, remote: &Value) -> Value {
    let mut assets = match local {
        Value::Object(a) => a,
        _ => Map::new(),
    };
    assets.insert("updatedAt".into(), json!(now_ms()));
    for k in ["shop", "objectId"] {
        assets.insert(k.into(), remote[k].clone());
    }
    assets.insert("title".into(), json!(local_title.filter(|t| !t.is_empty()).or(remote["title"].as_str()).unwrap_or_default()));
    let cover = match remote["coverImageUrl"].as_str().filter(|s| !s.is_empty()) {
        Some(url) => json!(url),
        None if remote["shop"] == "steam" => json!(format!("{STEAM_ASSETS}/{}/library_600x900_2x.jpg", remote["objectId"].as_str().unwrap_or_default())),
        None => Value::Null,
    };
    assets.insert("coverImageUrl".into(), cover);
    for k in ["libraryHeroImageUrl", "libraryImageUrl", "logoImageUrl", "iconUrl", "logoPosition", "downloadSources"] {
        if let Some(v) = remote.get(k) {
            assets.insert(k.into(), v.clone());
        }
    }
    Value::Object(assets)
}

/// unlocksFromBackup: `shop:objectId` -> unlocked achievements.
fn unlocks(backup: &Value) -> Vec<(String, String, Vec<Unlock>)> {
    let Some(lists) = backup["data"]["achievements"].as_object() else { return vec![] };
    lists
        .iter()
        .filter_map(|(key, list)| {
            let (shop, object_id) = key.split_once(':')?;
            let found: Vec<Unlock> = list
                .as_array()?
                .iter()
                .filter_map(|a| Some(Unlock { name: a["name"].as_str()?.to_string(), unlock_time: a["unlockTime"].as_f64()? as i64 }))
                .collect();
            (!found.is_empty()).then(|| (shop.to_string(), object_id.to_string(), found))
        })
        .collect()
}

/// profileFromBackup
fn profile_fields(backup: &Value) -> Value {
    let (me, profile) = (&backup["data"]["me"], &backup["data"]["profile"]);
    let pick = |k: &str| me[k].as_str().or(profile[k].as_str()).map(str::to_string);
    let badges: Vec<&str> = profile["badges"].as_array().map(|b| b.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    json!({
        "displayName": pick("displayName").unwrap_or_else(|| "Player".into()),
        "bio": pick("bio").unwrap_or_default(),
        "profileImageUrl": pick("profileImageUrl"),
        "backgroundImageUrl": pick("backgroundImageUrl"),
        "badges": badges,
        "hydraUserId": me["id"].as_str(),
    })
}

/// mergeBackupCollections: backup collections added to the local ones; the same id stays local.
fn merge_collections(local: Value, backup: &Value) -> Value {
    let mut all = local.as_array().cloned().unwrap_or_default();
    let mut ids: HashSet<String> = all.iter().filter_map(|c| c["id"].as_str().map(str::to_string)).collect();
    for c in backup["data"]["collections"].as_array().into_iter().flatten() {
        if let (Some(id), Some(name)) = (c["id"].as_str(), c["name"].as_str()) {
            if ids.insert(id.to_string()) {
                all.push(json!({ "id": id, "name": name }));
            }
        }
    }
    Value::Array(all)
}

/// keepImage: copies a backed-up image next to the local profile, as a `local:` URL.
fn keep_image(app_data: &Path, dir: &Path, images: &Value, url: &Value, name: &str) -> Option<String> {
    let rel = images[url.as_str()?].as_str()?;
    let source = inside(dir, rel)?;
    let target_dir = app_data.join("local-profile");
    fs::create_dir_all(&target_dir).ok()?;
    let ext = source.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let target = target_dir.join(format!("{name}{ext}"));
    fs::copy(&source, &target).ok()?;
    Some(format!("local:{}", target.display()))
}

pub async fn import(app: &AppHandle, dir: &Path) -> Result<Value, String> {
    let backup = read(dir)?;
    let library = backup["data"]["library"].as_array().cloned().unwrap_or_default();
    {
        let store = app.state::<Store>();
        for remote in &library {
            let (Some(shop), Some(object_id)) = (remote["shop"].as_str(), remote["objectId"].as_str()) else { continue };
            let key = game_key(shop, object_id);
            let local = match store.get("games", &key)? {
                Value::Object(g) => Some(g),
                _ => None,
            };
            let merged = merge_game(local, remote);
            store.put("games", &key, &merged)?;
            let assets = merge_assets(store.get("gameShopAssets", &key)?, merged["title"].as_str(), remote);
            store.put("gameShopAssets", &key, &assets)?;
        }
    }

    let mut unlock_count = 0;
    for (shop, object_id, found) in unlocks(&backup) {
        // No pop-ups or page events: these are old unlocks.
        crate::achievements::merge(app, &shop, &object_id, &found, false, false).await?;
        unlock_count += crate::achievements::unlocked(&app.state::<Store>(), &game_key(&shop, &object_id)).len();
    }

    let store = app.state::<Store>();
    let app_data = app.path().chimaera_data_dir().map_err(|e| e.to_string())?;
    let images = &backup["data"]["images"];
    let fields = profile_fields(&backup);
    let mut profile = crate::hydra_api::load_profile(&store)?;
    for k in ["displayName", "bio", "badges", "hydraUserId"] {
        profile[k] = fields[k].clone();
    }
    for (k, name) in [("profileImageUrl", "avatar"), ("backgroundImageUrl", "background")] {
        if let Some(url) = keep_image(&app_data, dir, images, &fields[k], name) {
            profile[k] = json!(url);
        }
    }
    crate::hydra_api::save_profile(&store, &profile)?;
    crate::hydra_api::save_collections(&store, &merge_collections(crate::hydra_api::load_collections(&store)?, &backup))?;

    // Hydra Cloud saves become local save backups.
    let mut save_count = 0;
    for (key, list) in backup["data"]["cloudArtifacts"].as_object().into_iter().flatten() {
        let Some((shop, object_id)) = key.split_once(':') else { continue };
        let game_dir = save_backups::game_dir(app, shop, object_id)?;
        let existing: HashSet<String> = save_backups::list_in(&game_dir).iter().filter_map(|b| b["createdAt"].as_str().map(str::to_string)).collect();
        for artifact in list.as_array().into_iter().flatten() {
            let (Some(file), Some(home)) = (artifact["backupFile"].as_str(), artifact["homeDir"].as_str()) else { continue };
            let Some(source) = inside(dir, file) else { continue };
            let created_at = artifact["createdAt"].as_str().or(backup["createdAt"].as_str()).unwrap_or_default().to_string();
            if existing.contains(&created_at) {
                continue;
            }
            let label = artifact["label"].as_str().filter(|l| !l.is_empty()).unwrap_or("Hydra Cloud");
            let meta = json!({
                "label": label,
                "createdAt": created_at,
                "homeDir": home,
                "winePrefixPath": artifact["winePrefixPath"].as_str(),
                "hostname": artifact["hostname"].as_str().unwrap_or_default(),
            });
            save_backups::store_in(&game_dir, &source, meta)?;
            save_count += 1;
        }
    }
    eprintln!("[hydra-backup] imported {}", dir.display());
    Ok(json!({ "games": library.len(), "unlocks": unlock_count, "saveBackups": save_count }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chimaera-hydra-backup-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("files")).unwrap();
        dir
    }

    fn write_backup(dir: &Path, files: Value) {
        let backup = json!({ "version": 1, "user": { "id": "u1" }, "sections": { "library": { "ok": true } }, "data": { "library": [] }, "files": files });
        fs::write(dir.join("backup.json"), backup.to_string()).unwrap();
    }

    #[test]
    fn validates_listed_files() {
        let dir = temp_dir("validate");
        fs::write(dir.join("files/a.png"), b"abc").unwrap();
        let sha = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        write_backup(&dir, json!([{ "path": "files/a.png", "sizeBytes": 3, "sha256": sha }]));
        assert!(read(&dir).is_ok());
        write_backup(&dir, json!([{ "path": "files/a.png", "sizeBytes": 3, "sha256": "00" }, { "path": "files/missing", "sizeBytes": 1, "sha256": sha }]));
        assert_eq!(read(&dir).unwrap_err(), "backup/2-bad-files");
        write_backup(&dir, json!([{ "path": "../backup.json", "sizeBytes": 3, "sha256": sha }]));
        assert_eq!(read(&dir).unwrap_err(), "backup/1-bad-files");
        fs::write(dir.join("backup.json"), r#"{"version":2}"#).unwrap();
        assert_eq!(read(&dir).unwrap_err(), "backup/invalid");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_game_takes_the_backup_fields() {
        let remote = json!({ "shop": "steam", "objectId": "10", "title": "CS", "id": "r1", "createdAt": "2024-01-01T00:00:00.000Z",
            "runtimeByPlatform": { "hydra": 60, "steam": 120 }, "isFavorite": true, "collectionId": "c1", "achievementCount": 5, "customLibraryImageUrl": "x" });
        let game = merge_game(None, &remote);
        assert_eq!(game["playTimeInMilliseconds"], json!(60_000.0));
        assert_eq!(game["steamPlayTimeInMilliseconds"], json!(120_000.0));
        assert_eq!(game["favorite"], json!(true));
        assert_eq!(game["collectionIds"], json!(["c1"]));
        assert_eq!(game["customCoverImageUrl"], json!("x"));
        assert_eq!(game["addedToLibraryAt"], json!("2024-01-01T00:00:00.000Z"));
        assert_eq!(game["source"], json!("hydra"));
    }

    #[test]
    fn existing_game_keeps_the_larger_playtime_and_local_choices() {
        let local = json!({ "shop": "steam", "objectId": "10", "title": "Mine", "playTimeInMilliseconds": 900_000, "favorite": false,
            "lastTimePlayed": "2025-01-01T00:00:00.000Z", "collectionIds": ["keep"], "executablePath": "C:\\g.exe", "isDeleted": true, "source": "hydra" });
        let remote = json!({ "shop": "steam", "objectId": "10", "playTimeInSeconds": 60, "lastTimePlayed": "2024-01-01T00:00:00.000Z", "achievementCount": 3 });
        let game = merge_game(local.as_object().cloned(), &remote);
        assert_eq!(game["playTimeInMilliseconds"], json!(900_000.0));
        assert_eq!(game["lastTimePlayed"], json!("2025-01-01T00:00:00.000Z"));
        assert_eq!(game["collectionIds"], json!(["keep"]));
        assert_eq!(game["executablePath"], json!("C:\\g.exe"));
        assert_eq!(game["favorite"], json!(false));
        assert_eq!(game["isDeleted"], json!(true));
        assert_eq!(game["title"], json!("Mine"));
    }

    #[test]
    fn steam_import_revives_a_deleted_game() {
        let local = json!({ "isDeleted": true, "source": "steam" });
        let game = merge_game(local.as_object().cloned(), &json!({ "hasActiveSteamImport": true }));
        assert_eq!(game["isDeleted"], json!(false));
        assert_eq!(game["source"], json!("steam"));
    }

    #[test]
    fn assets_keep_cached_fields_and_default_the_steam_cover() {
        let assets = merge_assets(json!({ "libraryImageUrl": "cached" }), None, &json!({ "shop": "steam", "objectId": "10", "title": "CS" }));
        assert_eq!(assets["libraryImageUrl"], json!("cached"));
        assert_eq!(assets["coverImageUrl"], json!(format!("{STEAM_ASSETS}/10/library_600x900_2x.jpg")));
        assert_eq!(assets["title"], json!("CS"));
    }

    #[test]
    fn reads_unlocks_profile_and_collections() {
        let backup = json!({ "data": {
            "achievements": { "steam:10": [{ "name": "a", "unlockTime": 5 }, { "name": "locked" }], "steam:11": [] },
            "me": { "id": "u1", "displayName": "Shai" }, "profile": { "bio": "hi", "badges": ["b", 1] },
            "collections": [{ "id": "c1", "name": "Mine" }, { "id": "c2", "name": "New" }],
        }});
        let found = unlocks(&backup);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].0.as_str(), found[0].1.as_str(), found[0].2.len()), ("steam", "10", 1));
        let profile = profile_fields(&backup);
        assert_eq!(profile["displayName"], json!("Shai"));
        assert_eq!(profile["bio"], json!("hi"));
        assert_eq!(profile["badges"], json!(["b"]));
        let merged = merge_collections(json!([{ "id": "c1", "name": "Local" }]), &backup);
        assert_eq!(merged, json!([{ "id": "c1", "name": "Local" }, { "id": "c2", "name": "New" }]));
    }

    /// Read-only check of the user's real export in chimaera/hydra-backup/.
    #[test]
    #[ignore]
    fn validates_the_local_backup() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../hydra-backup");
        for entry in fs::read_dir(root).unwrap().flatten() {
            let backup = read(&entry.path()).unwrap();
            eprintln!("{}: {} games, {} unlock lists", entry.path().display(), backup["data"]["library"].as_array().map_or(0, Vec::len), unlocks(&backup).len());
        }
    }
}
