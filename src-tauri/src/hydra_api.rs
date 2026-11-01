//! `hydraApiCall`: the port of HydraApi + LocalApi (src/main/services/hydra-api.ts,
//! local-api/). Chimaera has no Hydra sign-in, so requests are classified by path:
//! public catalogue data goes to the API without a token, the user's own data is
//! answered from the local store, and Hydra account data is never requested.

use serde_json::{json, Map, Value};

use crate::store::Store;

pub const LOCAL_USER_ID: &str = "local";
const LOCAL_PROFILE_KEY: &str = "localProfile";
const LOCAL_COLLECTIONS_KEY: &str = "localCollections";
const PRIVATE_PREFIXES: &[&str] = &["/profile", "/users", "/auth", "/cloud-gifts", "/friends", "/notifications", "/achievements"];

#[derive(Debug, PartialEq)]
pub enum Kind {
    Local(&'static str),
    Public,
    Noop,
    Blocked,
}

fn is_under(path: &str, prefix: &str) -> bool {
    path == prefix || path.starts_with(&format!("{prefix}/"))
}

/// Port of classifyRequest (local-api-core.ts). Returns the kind plus the query
/// pairs, with `id` added for collection routes.
pub fn classify(method: &str, url: &str) -> (Kind, Vec<(String, String)>) {
    let (path_part, query_part) = url.split_once('?').unwrap_or((url, ""));
    let trimmed = path_part.trim_end_matches('/');
    let path = if trimmed.is_empty() { "/" } else { trimmed };
    let mut query: Vec<(String, String)> = url::form_urlencoded::parse(query_part.as_bytes()).into_owned().collect();
    let m = method.to_lowercase();

    if path == "/profile/me" && m == "get" {
        return (Kind::Local("me"), query);
    }
    if path == "/profile" && m == "patch" {
        return (Kind::Local("patchProfile"), query);
    }
    if let Some(rest) = path.strip_prefix("/users/") {
        let (user, sub) = rest.split_once('/').map_or((rest, ""), |(u, s)| (u, s));
        let decoded = url::form_urlencoded::parse(format!("u={user}").as_bytes()).next().map(|(_, v)| v.into_owned());
        if m == "get" && decoded.as_deref() == Some(LOCAL_USER_ID) {
            let route = match sub {
                "" => Some("profile"),
                "library" => Some("library"),
                "stats" => Some("stats"),
                "friends" => Some("friends"),
                "souvenirs" => Some("souvenirs"),
                "games/achievements" => Some("gameAchievements"),
                _ => None,
            };
            if let Some(route) = route {
                return (Kind::Local(route), query);
            }
        }
    }
    if path == "/profile/games/collections" {
        if m == "get" {
            return (Kind::Local("listCollections"), query);
        }
        if m == "post" {
            return (Kind::Local("createCollection"), query);
        }
    }
    if let Some(id) = path.strip_prefix("/profile/games/collections/").filter(|id| !id.contains('/')) {
        query.push(("id".into(), id.to_string()));
        if m == "put" || m == "patch" {
            return (Kind::Local("renameCollection"), query);
        }
        if m == "delete" {
            return (Kind::Local("deleteCollection"), query);
        }
    }
    // Callers push to Hydra first, then write locally: let the local write happen.
    if m != "get" && (is_under(path, "/profile/games") || is_under(path, "/profile/download-sources")) {
        return (Kind::Noop, query);
    }
    if PRIVATE_PREFIXES.iter().any(|p| is_under(path, p)) {
        return (Kind::Blocked, query);
    }
    // Reviews and votes are posted as the user.
    let segments: Vec<&str> = path.split('/').collect();
    if m != "get" && segments.len() >= 5 && segments[1] == "games" && segments[4].starts_with("reviews") {
        return (Kind::Blocked, query);
    }
    (Kind::Public, query)
}

fn query_get<'a>(query: &'a [(String, String)], key: &str) -> Option<&'a str> {
    query.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn time_ms(v: &Value) -> i64 {
    match v {
        Value::String(s) => iso_digits(s),
        Value::Number(n) => n.as_i64().unwrap_or(0),
        _ => 0,
    }
}

/// ISO-8601 UTC timestamps compare correctly by their digits alone
/// (YYYYMMDDhhmmssSSS), which is all "most recent first" needs.
fn iso_digits(s: &str) -> i64 {
    s.chars().filter(char::is_ascii_digit).take(17).collect::<String>().parse().unwrap_or(0)
}

fn to_user_game(g: &Value) -> Value {
    let ms = crate::games::total_play_ms(g);
    json!({
        "objectId": g["objectId"], "shop": g["shop"], "title": g["title"],
        "playTimeInSeconds": (ms / 1000.0).floor() as i64,
        "lastTimePlayed": g.get("lastTimePlayed").cloned().unwrap_or(Value::Null),
        "unlockedAchievementCount": g["unlockedAchievementCount"].as_i64().unwrap_or(0),
        "achievementCount": g["achievementCount"].as_i64().unwrap_or(0),
        "achievementsPointsEarnedSum": 0, "hasManuallyUpdatedPlaytime": false,
        "isFavorite": g["favorite"].as_bool().unwrap_or(false),
        "isHiddenFromOthers": g["isHiddenFromOthers"].as_bool().unwrap_or(false),
        "isPinned": g["isPinned"].as_bool().unwrap_or(false),
        "pinnedDate": g.get("pinnedDate").cloned().unwrap_or(Value::Null),
        "customLibraryImageUrl": g.get("customCoverImageUrl").cloned().unwrap_or(Value::Null),
        "customLibraryHeroImageUrl": g.get("customHeroImageUrl").cloned().unwrap_or(Value::Null),
        "customLogoImageUrl": g.get("customLogoImageUrl").cloned().unwrap_or(Value::Null),
        "customIconUrl": g.get("customIconUrl").cloned().unwrap_or(Value::Null),
        "iconUrl": g.get("iconUrl").cloned().unwrap_or(Value::Null),
        "libraryHeroImageUrl": g.get("libraryHeroImageUrl").cloned().unwrap_or(Value::Null),
        "logoImageUrl": g.get("logoImageUrl").cloned().unwrap_or(Value::Null),
        "libraryImageUrl": g.get("libraryImageUrl").filter(|v| !v.is_null()).or_else(|| g.get("coverImageUrl")).cloned().unwrap_or(Value::Null),
        "coverImageUrl": g.get("coverImageUrl").cloned().unwrap_or(Value::Null),
        "logoPosition": null, "downloadSources": [],
    })
}

fn visible<'a>(games: &'a [Value], shops: &[&str]) -> Vec<&'a Value> {
    games
        .iter()
        .filter(|g| !g["isDeleted"].as_bool().unwrap_or(false) && (shops.is_empty() || shops.contains(&g["shop"].as_str().unwrap_or_default())))
        .collect()
}

fn shops(query: &[(String, String)]) -> Vec<&str> {
    query.iter().filter(|(k, _)| k == "shop").map(|(_, v)| v.as_str()).collect()
}

pub fn build_library(games: &[Value], query: &[(String, String)]) -> Value {
    let mut list = visible(games, &shops(query));
    let num = |g: &Value, k: &str| g[k].as_f64().unwrap_or(0.0);
    match query_get(query, "sortBy").unwrap_or("recently_played") {
        "title_asc" => list.sort_by(|a, b| a["title"].as_str().cmp(&b["title"].as_str())),
        "title_desc" => list.sort_by(|a, b| b["title"].as_str().cmp(&a["title"].as_str())),
        "most_played" => list.sort_by(|a, b| crate::games::total_play_ms(b).total_cmp(&crate::games::total_play_ms(a))),
        "achievements" => list.sort_by(|a, b| num(b, "unlockedAchievementCount").total_cmp(&num(a, "unlockedAchievementCount"))),
        _ => list.sort_by_key(|g| std::cmp::Reverse(time_ms(&g["lastTimePlayed"]))),
    }
    let take = query_get(query, "take").and_then(|t| t.parse().ok()).filter(|t| *t > 0).unwrap_or(list.len());
    let skip = query_get(query, "skip").and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut pinned: Vec<&Value> = list.iter().copied().filter(|g| g["isPinned"].as_bool().unwrap_or(false)).collect();
    pinned.sort_by_key(|g| std::cmp::Reverse(time_ms(&g["pinnedDate"])));
    json!({
        "totalCount": list.len(),
        "library": list.iter().skip(skip).take(take).map(|g| to_user_game(g)).collect::<Vec<_>>(),
        "pinnedGames": pinned.iter().map(|g| to_user_game(g)).collect::<Vec<_>>(),
    })
}

fn build_stats(games: &[Value], query: &[(String, String)]) -> Value {
    let list = visible(games, &shops(query));
    let seconds: i64 = list.iter().map(|g| (g["playTimeInMilliseconds"].as_f64().unwrap_or(0.0) / 1000.0).floor() as i64).sum();
    let unlocked: i64 = list.iter().map(|g| g["unlockedAchievementCount"].as_i64().unwrap_or(0)).sum();
    json!({
        "libraryCount": list.len(), "friendsCount": 0,
        "totalPlayTimeInSeconds": { "value": seconds, "topPercentile": 0 },
        "unlockedAchievementSum": unlocked,
    })
}

fn default_profile() -> Value {
    json!({ "displayName": "Player", "bio": "", "profileImageUrl": null, "backgroundImageUrl": null, "badges": [], "hydraUserId": null })
}

pub(crate) fn load_profile(store: &Store) -> Result<Value, String> {
    let mut profile = default_profile();
    if let Value::Object(stored) = store.get("", LOCAL_PROFILE_KEY)? {
        profile.as_object_mut().unwrap().extend(stored);
    }
    Ok(profile)
}

pub(crate) fn save_profile(store: &Store, profile: &Value) -> Result<(), String> {
    store.put("", LOCAL_PROFILE_KEY, profile)
}

pub(crate) fn load_collections(store: &Store) -> Result<Value, String> {
    Ok(json!(store.get("", LOCAL_COLLECTIONS_KEY)?.as_array().cloned().unwrap_or_default()))
}

pub(crate) fn save_collections(store: &Store, collections: &Value) -> Result<(), String> {
    store.put("", LOCAL_COLLECTIONS_KEY, collections)
}

/// Port of patchLocalProfile: only the fields a local profile has.
pub fn patch_profile(mut profile: Value, body: &Value) -> Value {
    for key in ["displayName", "bio"] {
        if let Some(s) = body[key].as_str() {
            profile[key] = json!(s.chars().take(if key == "bio" { 1000 } else { 64 }).collect::<String>());
        }
    }
    for key in ["profileImageUrl", "backgroundImageUrl"] {
        match &body[key] {
            v @ (Value::Null | Value::String(_)) if body.get(key).is_some() => profile[key] = v.clone(),
            _ => {}
        }
    }
    profile
}

fn build_me(p: &Value) -> Value {
    json!({
        "id": LOCAL_USER_ID, "username": p["displayName"], "email": null,
        "displayName": p["displayName"], "profileImageUrl": p["profileImageUrl"],
        "backgroundImageUrl": p["backgroundImageUrl"], "profileVisibility": "PRIVATE",
        "allowCloudGifts": false, "souvenirsVisibility": "PRIVATE", "bio": p["bio"],
        "workwondersJwt": "", "subscription": null, "karma": 0,
        "quirks": { "backupsPerGameLimit": 0 },
    })
}

fn build_profile(p: &Value, games: &[Value]) -> Value {
    let mut recent: Vec<&Value> = visible(games, &[]).into_iter().filter(|g| !g["lastTimePlayed"].is_null()).collect();
    recent.sort_by_key(|g| std::cmp::Reverse(time_ms(&g["lastTimePlayed"])));
    let mut out = build_me(p);
    out.as_object_mut().unwrap().extend(
        json!({
            "libraryGames": [], "recentGames": recent.iter().take(10).map(|g| to_user_game(g)).collect::<Vec<_>>(),
            "friends": [], "totalFriends": 0, "relation": null, "currentGame": null,
            // Hydra put profile achievement features behind a subscription; locally they are free.
            "hasActiveSubscription": true, "canReceiveCloudGift": false,
            "badges": p["badges"], "hasCompletedWrapped2025": false,
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    out
}

/// loadGamesWithArtwork (local-api.ts): catalogue art fills in only the fields it has.
fn games_with_artwork(store: &Store) -> Result<Vec<Value>, String> {
    store
        .entries("games")?
        .into_iter()
        .map(|(key, mut game)| {
            if let (Value::Object(target), Value::Object(art)) = (&mut game, crate::library::composed_assets_for(store, &key)?) {
                let title = art.get("title").and_then(Value::as_str).filter(|t| !t.is_empty()).map(str::to_string);
                target.extend(art.into_iter().filter(|(_, v)| !v.is_null()));
                if let Some(title) = title {
                    target.insert("title".into(), json!(title));
                }
            }
            Ok(game)
        })
        .collect()
}

fn collection_name(body: &Value) -> Result<String, String> {
    match body["name"].as_str().map(str::trim) {
        Some(name) if !name.is_empty() => Ok(name.chars().take(64).collect()),
        _ => Err("collection/name-required".into()),
    }
}

fn answer_local(store: &Store, route: &str, query: &[(String, String)], body: &Value) -> Result<Value, String> {
    let collections = || -> Result<Vec<Value>, String> { Ok(store.get("", LOCAL_COLLECTIONS_KEY)?.as_array().cloned().unwrap_or_default()) };
    let id = query_get(query, "id").map(str::to_string);
    match route {
        "me" => Ok(build_me(&load_profile(store)?)),
        "patchProfile" => {
            let next = patch_profile(load_profile(store)?, body);
            store.put("", LOCAL_PROFILE_KEY, &next)?;
            Ok(build_me(&next))
        }
        "friends" => Ok(json!({ "totalFriends": 0, "friends": [] })),
        // Souvenirs live on Hydra's servers, so the local profile has none; "true" here only showed a false 1,000-souvenir banner.
        "souvenirs" => Ok(json!({ "items": [], "total": 0, "hasReachedLimit": false, "hiddenReason": null })),
        "gameAchievements" => Ok(crate::achievements::profile_game_achievements(
            store,
            query_get(query, "shop").unwrap_or_default(),
            query_get(query, "objectId").unwrap_or_default(),
        )),
        "createCollection" => {
            let created = json!({ "id": uuid::Uuid::new_v4().to_string(), "name": collection_name(body)? });
            let mut all = collections()?;
            all.push(created.clone());
            store.put("", LOCAL_COLLECTIONS_KEY, &Value::Array(all))?;
            Ok(json!({ "id": created["id"], "name": created["name"], "gamesCount": 0 }))
        }
        "renameCollection" => {
            let name = collection_name(body)?;
            let all: Vec<Value> = collections()?
                .into_iter()
                .map(|mut c| {
                    if c["id"].as_str() == id.as_deref() {
                        c["name"] = json!(name);
                    }
                    c
                })
                .collect();
            store.put("", LOCAL_COLLECTIONS_KEY, &Value::Array(all))?;
            Ok(json!({ "id": id, "name": name }))
        }
        "deleteCollection" => {
            let all: Vec<Value> = collections()?.into_iter().filter(|c| c["id"].as_str() != id.as_deref()).collect();
            store.put("", LOCAL_COLLECTIONS_KEY, &Value::Array(all))?;
            Ok(json!({}))
        }
        _ => {
            let games = games_with_artwork(store)?;
            match route {
                "listCollections" => Ok(Value::Array(
                    collections()?
                        .into_iter()
                        .map(|mut c| {
                            let count = games
                                .iter()
                                .filter(|g| !g["isDeleted"].as_bool().unwrap_or(false) && g["collectionIds"].as_array().is_some_and(|ids| ids.contains(&c["id"])))
                                .count();
                            c["gamesCount"] = json!(count);
                            c
                        })
                        .collect(),
                )),
                "library" => Ok(build_library(&games, query)),
                "stats" => Ok(build_stats(&games, query)),
                _ => Ok(build_profile(&load_profile(store)?, &games)),
            }
        }
    }
}

fn query_pairs(params: &Value) -> Vec<(String, String)> {
    let Value::Object(map) = params else { return vec![] };
    let scalar = |v: &Value| match v {
        Value::String(s) => Some(s.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    };
    map.iter()
        .flat_map(|(k, v)| match v {
            // Axios sends arrays as `key[]=a&key[]=b`; the API ignores repeated plain keys (download options came back empty).
            Value::Array(items) => items.iter().filter_map(scalar).map(|s| (format!("{k}[]"), s)).collect::<Vec<_>>(),
            v => scalar(v).map(|s| vec![(k.clone(), s)]).unwrap_or_default(),
        })
        .collect()
}

/// Client and full URL for a request to the public Hydra API.
pub fn public_request(version: &str, url: &str) -> Result<(reqwest::Client, String), String> {
    let base = option_env!("CHIMAERA_API_URL").ok_or("MAIN_VITE_API_URL was not set in .env at build time")?;
    let client = reqwest::Client::builder()
        .user_agent(format!("Hydra Launcher v{version}"))
        .build()
        .map_err(|e| e.to_string())?;
    Ok((client, format!("{}{url}", base.trim_end_matches('/'))))
}

async fn send_public(version: &str, method: &str, url: &str, payload: &Map<String, Value>) -> Result<Value, String> {
    let (client, full) = public_request(version, url)?;
    let data = payload.get("data").cloned().unwrap_or(Value::Null);
    let request = match method {
        "get" => client.get(&full).query(&query_pairs(payload.get("params").unwrap_or(&Value::Null))),
        "post" | "postResponse" => client.post(&full).json(&data),
        "put" => client.put(&full).json(&data),
        "patch" => client.patch(&full).json(&data),
        "delete" => client.delete(&full),
        other => return Err(format!("Unsupported HTTP method: {other}")),
    };
    // shortcut: Hydra-If-Modified-Since is not sent, so the API returns full data instead of 304s; add it with the catalogue cache (F3).
    let response = request.send().await.map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    let text = response.text().await.map_err(|e| e.to_string())?;
    let body: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
    let accepted = payload["options"]["acceptedStatuses"].as_array().is_some_and(|s| s.iter().any(|v| v.as_u64() == Some(u64::from(status))));
    if !(200..300).contains(&status) && !(accepted && status != 401) {
        let message = body["message"].as_str().map(str::to_string);
        return Err(message.unwrap_or_else(|| format!("Request failed with status code {status}")));
    }
    Ok(if method == "postResponse" { json!({ "status": status, "data": body }) } else { body })
}

/// Entry point for the `hydraApiCall` channel; same payload as the preload sends.
pub async fn call(store: &Store, version: &str, payload: &Value) -> Result<Value, String> {
    let Value::Object(payload) = payload else { return Err("hydra-api-call-failed".into()) };
    let method = payload.get("method").and_then(Value::as_str).unwrap_or("get");
    let url = payload.get("url").and_then(Value::as_str).unwrap_or("/");
    let params = query_pairs(payload.get("params").unwrap_or(&Value::Null));
    let with_params = if params.is_empty() {
        url.to_string()
    } else {
        let encoded = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(&params).finish();
        format!("{url}{}{encoded}", if url.contains('?') { "&" } else { "?" })
    };
    let local_method = if method == "postResponse" { "post" } else { method };
    match classify(local_method, &with_params) {
        (Kind::Public, _) => send_public(version, method, url, payload).await,
        (Kind::Noop, _) => Ok(json!({})),
        // Matches UserNotLoggedInError's message in src/shared.
        (Kind::Blocked, _) => Err("user not logged in".into()),
        (Kind::Local(route), query) => answer_local(store, route, &query, payload.get("data").unwrap_or(&Value::Null)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_like_local_api_core() {
        assert_eq!(classify("get", "/profile/me").0, Kind::Local("me"));
        assert_eq!(classify("patch", "/profile").0, Kind::Local("patchProfile"));
        assert_eq!(classify("get", "/users/local/library?take=12").0, Kind::Local("library"));
        assert_eq!(classify("get", "/users/someone/library").0, Kind::Blocked);
        assert_eq!(classify("post", "/profile/games/collections").0, Kind::Local("createCollection"));
        let (kind, query) = classify("delete", "/profile/games/collections/abc");
        assert_eq!(kind, Kind::Local("deleteCollection"));
        assert_eq!(query_get(&query, "id"), Some("abc"));
        assert_eq!(classify("put", "/profile/games/steam/620").0, Kind::Noop);
        assert_eq!(classify("get", "/notifications").0, Kind::Blocked);
        assert_eq!(classify("post", "/games/steam/620/reviews").0, Kind::Blocked);
        assert_eq!(classify("get", "/games/steam/620/reviews").0, Kind::Public);
        assert_eq!(classify("get", "/catalogue/hot?take=12").0, Kind::Public);
    }

    #[test]
    fn arrays_use_axios_brackets() {
        let pairs = query_pairs(&json!({ "take": 100, "ids": ["a", "b"], "skip": null }));
        assert_eq!(pairs, vec![("ids[]".into(), "a".into()), ("ids[]".into(), "b".into()), ("take".into(), "100".into())]);
    }

    #[test]
    fn library_sorts_pages_and_skips_deleted() {
        let games = vec![
            json!({ "objectId": "1", "shop": "steam", "title": "B", "playTimeInMilliseconds": 5000, "lastTimePlayed": "2026-01-01T00:00:00Z", "isDeleted": false, "isPinned": true }),
            json!({ "objectId": "2", "shop": "steam", "title": "A", "playTimeInMilliseconds": 1000, "steamPlayTimeInMilliseconds": 9000, "lastTimePlayed": "2026-06-01T00:00:00Z", "isDeleted": false }),
            json!({ "objectId": "3", "shop": "steam", "title": "C", "isDeleted": true }),
        ];
        let lib = build_library(&games, &[("sortBy".into(), "title_asc".into()), ("take".into(), "1".into())]);
        assert_eq!(lib["totalCount"], 2);
        assert_eq!(lib["library"][0]["title"], "A");
        assert_eq!(lib["pinnedGames"][0]["objectId"], "1");
        let recent = build_library(&games, &[]);
        assert_eq!(recent["library"][0]["objectId"], "2");
        assert_eq!(recent["library"][1]["playTimeInSeconds"], 5);
        let played = build_library(&games, &[("sortBy".into(), "most_played".into())]);
        assert_eq!((played["library"][0]["objectId"].as_str(), played["library"][0]["playTimeInSeconds"].as_i64()), (Some("2"), Some(10)));
    }

    #[test]
    fn profile_patch_keeps_only_local_fields() {
        let next = patch_profile(default_profile(), &json!({ "displayName": "x".repeat(80), "email": "a@b.c", "profileImageUrl": null }));
        assert_eq!(next["displayName"].as_str().unwrap().len(), 64);
        assert!(next.get("email").is_none());
        assert_eq!(next["profileImageUrl"], Value::Null);
    }
}
