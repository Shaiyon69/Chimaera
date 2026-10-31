//! Game page data: `getGameShopDetails` and `getGameStats`, ported from
//! src/main/events/catalogue/ and services/steam.ts (getSteamAppDetails).

use serde_json::{json, Map, Value};
use tauri::{AppHandle, Manager};

use crate::library::{compose_assets, now_ms};
use crate::store::Store;
use crate::{bundled_games, hydra_api};

const STATS_CACHE_MS: u64 = 30 * 60 * 1000;

/// getSteamLanguage: the Steam store's name for an i18next language code.
pub fn steam_language(language: &str) -> &'static str {
    const BY_LOCALE: &[(&str, &str)] = &[
        ("ar", "arabic"), ("bg", "bulgarian"), ("cs", "czech"), ("da", "danish"), ("de", "german"),
        ("en", "english"), ("es", "spanish"), ("fi", "finnish"), ("fr", "french"), ("hu", "hungarian"),
        ("id", "indonesian"), ("it", "italian"), ("ja", "japanese"), ("ko", "koreana"), ("nb", "norwegian"),
        ("nl", "dutch"), ("pl", "polish"), ("pt-BR", "brazilian"), ("pt-PT", "portuguese"), ("ro", "romanian"),
        ("ru", "russian"), ("sv", "swedish"), ("tr", "turkish"), ("uk", "ukrainian"), ("zh-CN", "schinese"),
        ("zh-TW", "tchinese"), ("zh", "schinese"),
    ];
    let find = |code: &str| BY_LOCALE.iter().find(|(c, _)| *c == code).map(|(_, l)| *l);
    find(language).or_else(|| find(language.split('-').next().unwrap_or_default())).unwrap_or("english")
}

/// parseSteamAppDetailsResponse: the successful entry whose app id matches, plus `objectId`.
pub fn parse_steam_app_details(response: &Value, object_id: &str) -> Option<Value> {
    let entry = response.as_object()?.values().find(|item| {
        item["success"].as_bool().unwrap_or(false) && item["data"]["steam_appid"].as_u64().map(|id| id.to_string()).as_deref() == Some(object_id)
    })?;
    let Value::Object(mut data) = entry["data"].clone() else { return None };
    data.insert("objectId".into(), json!(object_id));
    Some(Value::Object(data))
}

async fn fetch_steam_app_details(object_id: &str, language: &str) -> Option<Value> {
    let response = reqwest::Client::new()
        .get("https://store.steampowered.com/api/appdetails")
        .query(&[("appids", object_id), ("l", steam_language(language)), ("cc", "us")])
        .send()
        .await
        .ok()?;
    parse_steam_app_details(&response.json().await.ok()?, object_id)
}

/// Fetches fresh Steam details, renamed to the user's title, and caches them.
async fn refresh_steam(store: &Store, object_id: &str, language: &str, cache_key: &str, assets: &Value) -> Option<Value> {
    let mut details = fetch_steam_app_details(object_id, language).await?;
    if let Some(title) = assets["title"].as_str() {
        details["name"] = json!(title);
    }
    if let Err(error) = store.put("gameShopCache", cache_key, &details) {
        eprintln!("[shop-details] could not cache {cache_key}: {error}");
    }
    Some(details)
}

fn with_assets(details: Value, assets: Value) -> Value {
    let Value::Object(mut details) = details else { return details };
    details.insert("assets".into(), assets);
    Value::Object(details)
}

fn selected_assets(store: &Store, key: &str, assets: Value) -> Result<Value, String> {
    if assets.is_null() {
        return Ok(assets);
    }
    Ok(compose_assets(assets, &store.get("artworkSelection", key)?))
}

async fn steam_details(app: &AppHandle, object_id: &str, language: &str) -> Result<Value, String> {
    let store = app.state::<Store>();
    let key = format!("steam:{object_id}");
    let cache_key = format!("{key}:{}", steam_language(language));
    let (cached, assets) = (store.get("gameShopCache", &cache_key)?, store.get("gameShopAssets", &key)?);

    let details = if cached.is_null() {
        refresh_steam(&store, object_id, language, &cache_key, &assets).await
    } else {
        // Like Electron: answer from the cache now, refresh it for next time.
        let (app, object_id, language, cache_key, assets) = (app.clone(), object_id.to_string(), language.to_string(), cache_key.clone(), assets.clone());
        tauri::async_runtime::spawn(async move {
            refresh_steam(&app.state::<Store>(), &object_id, &language, &cache_key, &assets).await;
        });
        Some(cached)
    };
    let Some(details) = details else { return Ok(Value::Null) };
    Ok(with_assets(details, selected_assets(&store, &key, assets)?))
}

fn str_list(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

/// mapLaunchboxToShopDetails: Hydra's LaunchBox entry in the Steam details shape the page reads.
pub fn map_launchbox(object_id: &str, basic: &Value, entry: &Value, cached: &Value, language: &str) -> Value {
    let data = &entry["data"];
    let description = data["description"].as_str().unwrap_or_default();
    let or_null = |v: &Value| if v.is_null() { None } else { Some(v.clone()) };
    let genres: Vec<Value> = str_list(&data["genres"]).into_iter().enumerate().map(|(i, name)| json!({ "id": i.to_string(), "name": name })).collect();
    let screenshots: Vec<Value> = str_list(&data["screenshots"])
        .into_iter()
        .enumerate()
        .map(|(i, url)| json!({ "id": i, "path_thumbnail": url, "path_full": url }))
        .collect();
    let requirements = json!({ "minimum": "", "recommended": "" });
    let mut out = json!({
        "objectId": object_id,
        "name": or_null(&data["title"]).or_else(|| or_null(&basic["title"])).unwrap_or(json!("")),
        "retroAchievementsGameId": or_null(&basic["retroAchievementsGameId"]).or_else(|| or_null(&cached["retroAchievementsGameId"])).unwrap_or(Value::Null),
        "steam_appid": 0,
        "detailed_description": description,
        "about_the_game": description,
        "short_description": "",
        "developers": str_list(&data["developers"]),
        "publishers": str_list(&data["publishers"]),
        "genres": genres,
        "supported_languages": "",
        "screenshots": screenshots,
        "pc_requirements": requirements,
        "mac_requirements": requirements,
        "linux_requirements": requirements,
        "release_date": { "coming_soon": false, "date": or_null(&data["releaseDate"]).or_else(|| or_null(&basic["releaseDate"])).unwrap_or(json!("")) },
        "content_descriptors": { "ids": [] },
    });
    // Electron leaves these undefined (absent) when missing.
    if !data.is_null() {
        out["descriptionLanguage"] = json!(language);
    }
    if let Some(platform) = or_null(&entry["platform"]).or_else(|| or_null(&data["platform"])) {
        out["platform"] = platform;
    }
    if let Some(skus) = or_null(&entry["skus"]) {
        out["skus"] = skus;
    }
    out
}

/// The LaunchBox assets record cached under gameShopAssets, preferring the details' art.
fn launchbox_assets(object_id: &str, shop: &str, basic: &Value, data: &Value, name: &Value) -> Value {
    let art = &data["assets"];
    let pick = |field: &str| [&art[field], &basic[field]].into_iter().find(|v| !v.is_null()).cloned().unwrap_or(Value::Null);
    let title = [&art["title"], &basic["title"], name].into_iter().find(|v| !v.is_null()).cloned().unwrap_or(Value::Null);
    json!({
        "objectId": object_id,
        "shop": shop,
        "title": title,
        "iconUrl": pick("iconUrl"),
        "libraryHeroImageUrl": pick("libraryHeroImageUrl"),
        "libraryImageUrl": pick("libraryImageUrl"),
        "logoImageUrl": pick("logoImageUrl"),
        "logoPosition": basic["logoPosition"],
        "coverImageUrl": basic["coverImageUrl"],
        "downloadSources": [],
    })
}

async fn launchbox_details(store: &Store, version: &str, object_id: &str, shop: &str, language: &str) -> Result<Value, String> {
    let key = format!("{shop}:{object_id}");
    let cache_key = format!("{key}:{language}");
    let (cached, cached_assets) = (store.get("gameShopCache", &cache_key)?, store.get("gameShopAssets", &key)?);
    let cache_is_current = (!cached["platform"].is_null() || !cached["skus"].is_null())
        && cached["retroAchievementsGameId"].is_number()
        && cached["descriptionLanguage"].as_str() == Some(language);
    if cache_is_current {
        return Ok(with_assets(cached, selected_assets(store, &key, cached_assets)?));
    }

    let basic_req = json!({ "method": "get", "url": format!("/games/{shop}/{object_id}"), "options": { "needsAuth": false } });
    let details_req = json!({
        "method": "post", "url": "/games/shop-details",
        "data": { "shop": shop, "objectIds": [object_id], "language": language },
        "options": { "needsAuth": false },
    });
    let (basic, details) = tokio::join!(hydra_api::call(store, version, &basic_req), hydra_api::call(store, version, &details_req));
    let basic = basic.unwrap_or_else(|e| {
        eprintln!("[shop-details] launchbox basic info failed: {e}");
        Value::Null
    });
    let details = details.unwrap_or_else(|e| {
        eprintln!("[shop-details] launchbox details failed: {e}");
        json!([])
    });
    let entry = str_list(&details).into_iter().find(|e| e["objectId"].as_str() == Some(object_id)).unwrap_or(Value::Null);
    let data = &entry["data"];
    if data.is_null() && basic.is_null() {
        return Ok(Value::Null);
    }

    let mapped = map_launchbox(object_id, &basic, &entry, &cached, language);
    if let Err(error) = store.put("gameShopCache", &cache_key, &mapped) {
        eprintln!("[shop-details] could not cache {cache_key}: {error}");
    }
    let assets = if data["assets"].is_null() && basic.is_null() {
        cached_assets
    } else {
        let mut fresh = launchbox_assets(object_id, shop, &basic, data, &mapped["name"]);
        let mut stored = fresh.clone();
        stored["updatedAt"] = json!(now_ms());
        if let Err(error) = store.put("gameShopAssets", &key, &stored) {
            eprintln!("[shop-details] could not cache {key} assets: {error}");
        }
        fresh = selected_assets(store, &key, fresh)?;
        fresh
    };
    Ok(with_assets(mapped, assets))
}

pub async fn get_game_shop_details(app: &AppHandle, object_id: &str, shop: &str, language: &str) -> Result<Value, String> {
    match shop {
        "custom" => Ok(bundled_games::shop_details(app, object_id)),
        "launchbox" => {
            let version = app.package_info().version.to_string();
            launchbox_details(&app.state::<Store>(), &version, object_id, shop, language).await
        }
        "steam" => steam_details(app, object_id, language).await,
        _ => Err("Not implemented".into()),
    }
}

pub async fn get_game_stats(store: &Store, version: &str, object_id: &str, shop: &str) -> Result<Value, String> {
    if shop == "custom" {
        return Ok(Value::Null);
    }
    let key = format!("{shop}:{object_id}");
    let cached = store.get("gameStatsAssets", &key)?;
    if cached["updatedAt"].as_u64().is_some_and(|at| at + STATS_CACHE_MS > now_ms()) {
        return Ok(cached);
    }
    let request = json!({ "method": "get", "url": format!("/games/{shop}/{object_id}/stats"), "options": { "needsAuth": false } });
    let data = hydra_api::call(store, version, &request).await?;
    let mut stored = match &data {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    stored.insert("updatedAt".into(), json!(now_ms()));
    store.put("gameStatsAssets", &key, &Value::Object(stored))?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_languages_like_get_steam_language() {
        assert_eq!(steam_language("pt-BR"), "brazilian");
        assert_eq!(steam_language("zh-HK"), "schinese");
        assert_eq!(steam_language("de-AT"), "german");
        assert_eq!(steam_language("xx"), "english");
    }

    #[test]
    fn takes_only_the_matching_successful_app() {
        let response = json!({
            "620": { "success": true, "data": { "steam_appid": 620, "name": "Portal 2" } },
            "400": { "success": false },
        });
        let details = parse_steam_app_details(&response, "620").unwrap();
        assert_eq!(details["name"], "Portal 2");
        assert_eq!(details["objectId"], "620");
        assert!(parse_steam_app_details(&response, "400").is_none());
        // A redirect to another app id (bundled DLC, region swap) is not this game.
        assert!(parse_steam_app_details(&json!({ "1": { "success": true, "data": { "steam_appid": 2 } } }), "1").is_none());
    }

    #[test]
    fn maps_launchbox_entries_to_the_steam_shape() {
        let entry = json!({
            "objectId": "42", "platform": "PS1",
            "data": { "title": "Crash", "description": "<p>Spin</p>", "genres": ["Platform"], "screenshots": ["a.png"], "developers": ["ND"] },
        });
        let mapped = map_launchbox("42", &json!({ "retroAchievementsGameId": 7 }), &entry, &Value::Null, "en");
        assert_eq!(mapped["name"], "Crash");
        assert_eq!(mapped["platform"], "PS1");
        assert_eq!(mapped["retroAchievementsGameId"], 7);
        assert_eq!(mapped["genres"][0], json!({ "id": "0", "name": "Platform" }));
        assert_eq!(mapped["screenshots"][0]["path_full"], "a.png");
        assert_eq!(mapped["descriptionLanguage"], "en");
        assert!(mapped.get("skus").is_none());
    }
}
