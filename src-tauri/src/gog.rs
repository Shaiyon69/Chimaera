//! GOG account link and library sync. The endpoints follow Playnite's GogLibrary
//! (JosefNemec/PlayniteExtensions) and SuccessStory (Lacro59/playnite-successstory-plugin),
//! both MIT licensed.
//!
//! The gog.com login runs in a webview with its own data folder, like the Steam link. Each sync
//! opens that profile in a hidden webview (which lets gog.com refresh its session), reads the
//! cookies, asks menu.gog.com for the account and a web access token, then calls the web
//! endpoints with them. No GOG Galaxy client credentials are used.
//!
//! Records share the launcher import's key (`custom:gog-<productId>`), so an owned game and its
//! local install are one library entry.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Map, Value};
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::achievement_files::Unlock;
use crate::achievements::{self, CATALOGUE, UNLOCKED};
use crate::games::{game_key, update, with_executable_path};
use crate::launcher_import::launcher_object_id;
use crate::library::{iso_now, now_ms};
use crate::steam::{later_iso, parse_iso_ms};
use crate::store::Store;

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const HOME_URL: &str = "https://www.gog.com/en/";
const LOGIN_URL: &str = "https://www.gog.com/en/account";
const ACCOUNT_URL: &str = "https://menu.gog.com/v1/account/basic";
const ACCOUNT_KEY: &str = "gogAccount";
const LOGIN_LABEL: &str = "gog-login";
const SESSION_LABEL: &str = "gog-session";
const WEBVIEW_DIR: &str = "gog-webview";
const ACHIEVEMENT_CONCURRENCY: usize = 4;
const STARTUP_MIN_AGE_MS: u64 = 6 * 60 * 60 * 1000;

const SESSION_REQUIRED: &str = "gog-session-required";
const ACCOUNT_MISMATCH: &str = "gog-account-mismatch";
const RATE_LIMITED: &str = "gog-rate-limited";
const ABORTED: &str = "gog-sync-aborted";

// ---- Errors and retries ----

#[derive(Debug)]
enum Error {
    Http { status: u16, retry_after: Option<u64> },
    Transport(String),
    Message(&'static str),
}

impl Error {
    fn status(&self) -> Option<u16> {
        match self {
            Error::Http { status, .. } => Some(*status),
            _ => None,
        }
    }

    fn message(&self) -> String {
        match self {
            Error::Message(m) => m.to_string(),
            Error::Http { status: 429, .. } => RATE_LIMITED.into(),
            Error::Http { status: 401 | 403, .. } => SESSION_REQUIRED.into(),
            Error::Http { status, .. } => format!("gog-http-{status}"),
            Error::Transport(e) => e.clone(),
        }
    }

    fn is_abort(&self) -> bool {
        matches!(self, Error::Message(ABORTED))
    }
}

fn retry_delay_ms(error: &Error, failed: u32) -> Option<u64> {
    let exponent = failed.saturating_sub(1);
    match error {
        Error::Http { status: 429, retry_after, .. } if failed < 5 => Some(retry_after.map_or_else(|| (1000u64 << exponent).min(30_000), |s| s * 1000)),
        Error::Http { status: 500..=504, .. } if failed < 3 => Some(500u64 << exponent),
        _ => None,
    }
}

async fn sleep(ms: u64, cancel: &AtomicBool) -> Result<(), Error> {
    let until = std::time::Instant::now() + Duration::from_millis(ms);
    while std::time::Instant::now() < until {
        check(cancel)?;
        tokio::time::sleep(Duration::from_millis(100).min(until - std::time::Instant::now())).await;
    }
    check(cancel)
}

fn check(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Message(ABORTED));
    }
    Ok(())
}

async fn with_retry<T, F, Fut>(label: &str, cancel: &AtomicBool, mut call: F) -> Result<T, Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, Error>>,
{
    let mut failed = 0;
    loop {
        check(cancel)?;
        match call().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                failed += 1;
                let Some(delay) = retry_delay_ms(&error, failed) else { return Err(error) };
                eprintln!("[gog] {label} retry {failed} after HTTP {:?}, waiting {delay}ms", error.status());
                sleep(delay, cancel).await?;
            }
        }
    }
}

// ---- HTTP ----

#[derive(Clone, Default)]
struct Session {
    cookies: String,
    user_id: String,
    username: String,
    token: String,
}

fn client() -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| Error::Transport(e.to_string()))
}

async fn send(request: reqwest::RequestBuilder) -> Result<reqwest::Response, Error> {
    let response = request.send().await.map_err(|e| Error::Transport(e.to_string()))?;
    let status = response.status().as_u16();
    if !response.status().is_success() {
        let retry_after = response.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok());
        return Err(Error::Http { status, retry_after });
    }
    Ok(response)
}

async fn get_json(request: reqwest::RequestBuilder) -> Result<Value, Error> {
    send(request).await?.json().await.map_err(|e| Error::Transport(e.to_string()))
}

/// A gog.com page or API call made as the signed-in user.
async fn cookie_get(cookies: &str, url: &str) -> Result<Value, Error> {
    get_json(client()?.get(url).header("Cookie", cookies).header("Accept", "application/json")).await
}

fn cookie_header(cookies: &[tauri::webview::Cookie<'static>]) -> String {
    cookies.iter().filter(|c| !c.value().is_empty()).map(|c| format!("{}={}", c.name(), c.value())).collect::<Vec<_>>().join("; ")
}

fn id_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

// ---- Session ----

fn webview_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join(WEBVIEW_DIR))
}

fn is_gog_url(url: &url::Url) -> bool {
    url.host_str().is_some_and(|h| h == "gog.com" || h.ends_with(".gog.com"))
}

/// The signed-in account from menu.gog.com/v1/account/basic, or None when logged out.
fn parse_account(payload: &Value) -> Option<Session> {
    if payload["isLoggedIn"] != json!(true) {
        return None;
    }
    Some(Session {
        cookies: String::new(),
        user_id: id_string(&payload["userId"])?,
        username: payload["username"].as_str().filter(|u| !u.is_empty())?.to_string(),
        token: payload["accessToken"].as_str().unwrap_or_default().to_string(),
    })
}

async fn session_from_cookies(cookies: String) -> Result<Session, Error> {
    let payload = cookie_get(&cookies, ACCOUNT_URL).await?;
    let mut session = parse_account(&payload).ok_or(Error::Message(SESSION_REQUIRED))?;
    session.cookies = cookies;
    Ok(session)
}

/// Opens the GOG profile in a hidden webview, reads its cookies, and resolves the account.
async fn open_session(app: &AppHandle, cancel: &AtomicBool) -> Result<Session, Error> {
    if let Some(old) = app.get_webview_window(SESSION_LABEL) {
        let _ = old.destroy();
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<url::Url>();
    let window = WebviewWindowBuilder::new(app, SESSION_LABEL, WebviewUrl::External(HOME_URL.parse().expect("static url")))
        .visible(false)
        .user_agent(USER_AGENT)
        .data_directory(webview_dir(app).map_err(Error::Transport)?)
        .on_page_load(move |_, payload| {
            if payload.event() == PageLoadEvent::Finished {
                let _ = tx.send(payload.url().clone());
            }
        })
        .build()
        .map_err(|e| Error::Transport(format!("gog webview: {e}")))?;
    let cookies = async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            check(cancel)?;
            match tokio::time::timeout_at(deadline.min(tokio::time::Instant::now() + Duration::from_millis(250)), rx.recv()).await {
                Ok(Some(url)) if is_gog_url(&url) => break,
                Ok(Some(_)) | Err(_) if tokio::time::Instant::now() < deadline => {}
                _ => break,
            }
        }
        window.cookies_for_url(HOME_URL.parse().expect("static url")).map_err(|e| Error::Transport(format!("gog cookies: {e}")))
    }
    .await;
    let _ = window.destroy();
    session_from_cookies(cookie_header(&cookies?)).await
}

// ---- Account ----

fn account(store: &Store) -> Value {
    store.get("", ACCOUNT_KEY).unwrap_or(Value::Null)
}

fn status_of(account: Value) -> Value {
    match account {
        Value::Object(mut fields) => {
            fields.insert("connected".into(), json!(true));
            Value::Object(fields)
        }
        _ => json!({ "connected": false }),
    }
}

/// getGogIntegrationStatus.
pub fn integration_status(store: &Store) -> Value {
    status_of(account(store))
}

fn save_account(app: &AppHandle, session: &Session, avatar: Option<String>) -> Result<(), String> {
    let store = app.state::<Store>();
    let existing = account(&store);
    store.put("", ACCOUNT_KEY, &json!({
        "userId": session.user_id,
        "username": session.username,
        "avatarUrl": avatar,
        "connectedAt": existing["connectedAt"].as_str().map_or_else(iso_now, str::to_string),
        "lastSyncedAt": if existing["userId"] == json!(session.user_id) { existing["lastSyncedAt"].clone() } else { Value::Null },
    }))?;
    app.state::<GogSync>().clear_reconnect_required(app);
    Ok(())
}

fn show_integrations(app: &AppHandle) {
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.unminimize();
        let _ = main.show();
        let _ = main.set_focus();
        let _ = main.emit("on-navigate", "/settings?tab=integrations");
    }
}

/// startGogOAuth: the gog.com login; the account is saved once menu.gog.com reports it signed in.
pub fn start_login(app: &AppHandle) -> Result<(), String> {
    if let Some(old) = app.get_webview_window(LOGIN_LABEL) {
        let _ = old.destroy();
    }
    let done = Arc::new(AtomicBool::new(false));
    let handle = app.clone();
    let mut builder = WebviewWindowBuilder::new(app, LOGIN_LABEL, WebviewUrl::External(LOGIN_URL.parse().expect("static url")))
        .title("GOG")
        .inner_size(600.0, 800.0)
        .user_agent(USER_AGENT)
        .data_directory(webview_dir(app)?)
        .on_page_load(move |webview, payload| {
            if payload.event() != PageLoadEvent::Finished || !is_gog_url(payload.url()) || done.load(Ordering::Relaxed) {
                return;
            }
            let (done, app) = (done.clone(), handle.clone());
            // Reading cookies deadlocks on the event thread on Windows, so it runs on the async runtime.
            tauri::async_runtime::spawn(async move {
                let Some(window) = app.get_webview_window(webview.label()) else { return };
                let Ok(cookies) = window.cookies_for_url(HOME_URL.parse().expect("static url")) else { return };
                let cookies = cookie_header(&cookies);
                let Ok(payload) = cookie_get(&cookies, ACCOUNT_URL).await else { return };
                let Some(mut session) = parse_account(&payload) else { return };
                if done.swap(true, Ordering::Relaxed) {
                    return;
                }
                session.cookies = cookies;
                eprintln!("[gog] login completed");
                let avatar = payload["avatars"]["menu_user_av_big2"].as_str().or(payload["avatars"]["menu_user_av_big"].as_str()).map(str::to_string);
                let _ = window.destroy();
                match save_account(&app, &session, avatar) {
                    Ok(()) => {
                        let _ = app.emit("on-gog-connected", ());
                    }
                    Err(e) => {
                        eprintln!("[gog] failed to save the GOG account: {e}");
                        let _ = app.emit("on-gog-connect-error", "generic");
                    }
                }
                show_integrations(&app);
            });
        });
    if let Some(main) = app.get_webview_window("main") {
        builder = builder.parent(&main).map_err(|e| e.to_string())?;
    }
    builder.build().map_err(|e| e.to_string())?;
    Ok(())
}

// ---- Library ----

#[derive(Clone, Debug, Default, PartialEq)]
struct LibraryGame {
    id: String,
    title: String,
    play_ms: u64,
    last_played: Option<u64>,
    has_achievements: bool,
}

/// One page of account/getFilteredProducts: (games, total pages). Movies and DLC are skipped.
fn parse_products(payload: &Value) -> (Vec<LibraryGame>, u64) {
    let games = payload["products"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["isGame"].as_bool().unwrap_or(true) && !p["isMovie"].as_bool().unwrap_or(false))
        .filter_map(|p| Some(LibraryGame { id: id_string(&p["id"])?, title: p["title"].as_str().filter(|t| !t.is_empty())?.to_string(), ..Default::default() }))
        .collect();
    (games, payload["totalPages"].as_u64().unwrap_or(1))
}

/// GOG writes offsets as +00:00 or +0000; the ISO date without it goes through `parse_iso_ms`.
fn parse_gog_time(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.len() < 19 {
        return None;
    }
    let local = parse_iso_ms(&format!("{}Z", &text[..19]))? as i64;
    let rest = text[19..].trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset_minutes = match rest.chars().next() {
        Some(sign @ ('+' | '-')) => {
            let digits: String = rest[1..].chars().filter(char::is_ascii_digit).collect();
            let (h, m) = (digits.get(..2)?.parse::<i64>().ok()?, digits.get(2..4).unwrap_or("0").parse::<i64>().ok()?);
            if sign == '+' { h * 60 + m } else { -(h * 60 + m) }
        }
        _ => 0,
    };
    u64::try_from(local - offset_minutes * 60_000).ok()
}

/// A page of /u/<user>/games/stats: product id to (playtime ms, last session, achievement support), and total pages.
fn parse_stats(payload: &Value) -> (HashMap<String, (u64, Option<u64>, bool)>, u64) {
    let mut map = HashMap::new();
    for item in payload["_embedded"]["items"].as_array().into_iter().flatten() {
        let Some(id) = id_string(&item["game"]["id"]) else { continue };
        // Keyed by the user id; only one entry for the signed-in user's own stats.
        let stats = item["stats"].as_object().and_then(|s| s.values().next()).cloned().unwrap_or(Value::Null);
        let minutes = stats["playtime"].as_u64().unwrap_or(0);
        let last = stats["lastSession"].as_str().and_then(parse_gog_time);
        map.insert(id, (minutes * 60_000, last, item["game"]["achievementSupport"].as_bool().unwrap_or(false)));
    }
    (map, payload["pages"].as_u64().unwrap_or(1))
}

async fn fetch_library(session: &Session, cancel: &AtomicBool) -> Result<Vec<LibraryGame>, Error> {
    let mut games = Vec::new();
    let mut page = 1;
    loop {
        let url = format!("https://www.gog.com/account/getFilteredProducts?hiddenFlag=0&mediaType=1&sortBy=title&page={page}");
        let payload = with_retry("library", cancel, || cookie_get(&session.cookies, &url)).await?;
        let (found, pages) = parse_products(&payload);
        games.extend(found);
        if page >= pages {
            break;
        }
        page += 1;
    }
    let mut stats = HashMap::new();
    let mut page = 1;
    loop {
        let url = format!("https://www.gog.com/u/{}/games/stats?sort=recent_playtime&order=desc&page={page}", urlencoding(&session.username));
        let payload = match with_retry("stats", cancel, || cookie_get(&session.cookies, &url)).await {
            Ok(p) => p,
            // Playtime is extra; the library still imports without it.
            Err(e) if !e.is_abort() => {
                eprintln!("[gog] playtime stats unavailable: {e:?}");
                break;
            }
            Err(e) => return Err(e),
        };
        let (found, pages) = parse_stats(&payload);
        stats.extend(found);
        if page >= pages {
            break;
        }
        page += 1;
    }
    Ok(apply_stats(games, &stats))
}

fn apply_stats(games: Vec<LibraryGame>, stats: &HashMap<String, (u64, Option<u64>, bool)>) -> Vec<LibraryGame> {
    let mut seen = HashSet::new();
    games
        .into_iter()
        .filter(|g| seen.insert(g.id.clone()))
        .map(|mut g| {
            if let Some(&(play_ms, last, achievements)) = stats.get(&g.id) {
                (g.play_ms, g.last_played, g.has_achievements) = (play_ms, last, achievements);
            }
            g
        })
        .collect()
}

fn urlencoding(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

// ---- Artwork (gamesdb.gog.com) ----

#[derive(Debug, Default, PartialEq)]
struct Art {
    cover: Option<String>,
    hero: Option<String>,
    logo: Option<String>,
}

fn art_url(payload: &Value, field: &str, ext: &str) -> Option<String> {
    let format = payload["game"][field]["url_format"].as_str()?;
    Some(format.replace("{formatter}", "").replace("{ext}", ext))
}

fn parse_art(payload: &Value) -> Art {
    Art { cover: art_url(payload, "vertical_cover", "jpg"), hero: art_url(payload, "background", "jpg"), logo: art_url(payload, "logo", "png") }
}

async fn fetch_art(id: &str) -> Art {
    let url = format!("https://gamesdb.gog.com/platforms/gog/external_releases/{id}");
    let request = match client() {
        Ok(c) => c.get(url).timeout(Duration::from_secs(15)),
        Err(_) => return Art::default(),
    };
    match get_json(request).await {
        Ok(payload) => parse_art(&payload),
        Err(e) => {
            eprintln!("[gog] no artwork for {id}: {e:?}");
            Art::default()
        }
    }
}

// ---- Merge ----

pub(crate) fn object_id(product_id: &str) -> String {
    launcher_object_id("gog", product_id)
}

fn key_of(product_id: &str) -> String {
    game_key("custom", &object_id(product_id))
}

fn galaxy_launch(product_id: &str) -> Value {
    json!({ "kind": "uri", "uri": format!("goggalaxy://openGameView/{product_id}") })
}

/// Fields every synced record gets; local edits and the local install stay.
fn apply_sync_fields(game: &mut Map<String, Value>, library: &LibraryGame) {
    let last = later_iso(game.get("lastTimePlayed").unwrap_or(&Value::Null), library.last_played);
    game.insert("lastTimePlayed".into(), last);
    game.insert("gogPlayTimeInMilliseconds".into(), json!(library.play_ms));
    game.insert("launcher".into(), json!("gog"));
    game.insert("launcherGameId".into(), json!(library.id));
    game.insert("hasActiveGogImport".into(), json!(true));
    game.insert("isDeleted".into(), json!(false));
    if game.get("externalLaunch").is_none_or(Value::is_null) {
        game.insert("externalLaunch".into(), galaxy_launch(&library.id));
    }
}

/// Records new to the library; their art is fetched before `merge_library` creates them.
fn missing(store: &Store, games: &[LibraryGame]) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    for game in games {
        if store.get("games", &key_of(&game.id))?.is_null() {
            ids.push(game.id.clone());
        }
    }
    Ok(ids)
}

fn merge_library(store: &Store, games: &[LibraryGame], art: &HashMap<String, Art>) -> Result<usize, String> {
    let mut added = 0;
    for game in games {
        let key = key_of(&game.id);
        if store.get("games", &key)?.is_null() {
            let found = art.get(&game.id);
            let value = |pick: fn(&Art) -> &Option<String>| found.and_then(|a| pick(a).clone()).map(Value::String);
            crate::artwork::create_custom_game(store, &object_id(&game.id), &game.title, None, value(|a| &a.cover).as_ref(), value(|a| &a.logo).as_ref(), value(|a| &a.hero).as_ref())?;
            update(store, &key, |g| {
                g.insert("gogImported".into(), json!(true));
            })?;
            added += 1;
        } else if store.get("games", &key)?["isDeleted"] == json!(true) {
            added += 1;
        }
        update(store, &key, |g| apply_sync_fields(g, game))?;
    }
    Ok(added)
}

fn has_local_install(game: &Value) -> bool {
    game["executablePath"].as_str().is_some_and(|p| !p.is_empty())
}

/// Games GOG no longer lists lose the import flag; ones the sync brought in without a local install are hidden.
fn hide_removed(store: &Store, keep: &HashSet<String>) -> Result<(), String> {
    for (key, game) in store.entries("games")? {
        if game["hasActiveGogImport"] == json!(true) && !keep.contains(&key) {
            let hide = game["gogImported"] == json!(true) && !has_local_install(&game);
            update(store, &key, |g| {
                g.insert("hasActiveGogImport".into(), json!(false));
                if hide {
                    g.insert("isDeleted".into(), json!(true));
                }
            })?;
        }
    }
    Ok(())
}

/// Installed GOG games found on this machine bind their exe and launch task to synced records
/// that have none (the launcher import skips records that already exist).
fn link_installs(store: &Store, installs: &[crate::launcher_import::Detected]) -> Result<usize, String> {
    let mut linked = 0;
    for install in installs.iter().filter(|d| d.launcher == "gog") {
        let key = key_of(&install.id);
        let game = store.get("games", &key)?;
        if !game.is_object() || has_local_install(&game) || install.exe_path.is_none() {
            continue;
        }
        let mut game = with_executable_path(game, install.exe_path.as_deref());
        if let Some(launch) = &install.external_launch {
            game["externalLaunch"] = launch.clone();
        }
        store.put("games", &key, &game)?;
        linked += 1;
    }
    Ok(linked)
}

// ---- Achievements (gameplay.gog.com) ----

/// The Galaxy client id of a product, from its newest generation 2 build manifest.
async fn client_id(product_id: &str) -> Result<Option<String>, Error> {
    let builds = get_json(client()?.get(format!("https://content-system.gog.com/products/{product_id}/os/windows/builds?generation=2"))).await?;
    let Some(link) = builds["items"].as_array().and_then(|items| items.first()).and_then(|b| b["link"].as_str()) else { return Ok(None) };
    let bytes = send(client()?.get(link)).await?.bytes().await.map_err(|e| Error::Transport(e.to_string()))?;
    Ok(parse_build_meta(&bytes).and_then(|meta| id_string(&meta["clientId"])))
}

/// Build manifests are zlib-compressed JSON; some older ones are plain.
fn parse_build_meta(bytes: &[u8]) -> Option<Value> {
    let mut text = String::new();
    if flate2::read::ZlibDecoder::new(bytes).read_to_string(&mut text).is_err() {
        text = String::from_utf8_lossy(bytes).into_owned();
    }
    serde_json::from_str(&text).ok()
}

/// (catalogue in the Hydra shape the game page reads, unlocks).
fn parse_achievements(payload: &Value) -> (Vec<Value>, Vec<Unlock>) {
    let mut catalogue = Vec::new();
    let mut unlocks = Vec::new();
    for item in payload["items"].as_array().into_iter().flatten() {
        let Some(name) = item["achievement_key"].as_str().filter(|k| !k.is_empty()) else { continue };
        catalogue.push(json!({
            "name": name,
            "displayName": item["name"].as_str().unwrap_or(name),
            "description": item["description"].as_str().unwrap_or_default(),
            "icon": item["image_url_unlocked"],
            "icongray": item["image_url_locked"],
            "hidden": !item["visible"].as_bool().unwrap_or(true),
        }));
        if let Some(at) = item["date_unlocked"].as_str().and_then(parse_gog_time) {
            unlocks.push(Unlock { name: name.to_string(), unlock_time: at as i64 });
        }
    }
    (catalogue, unlocks)
}

async fn game_achievements(session: &Session, product_id: &str) -> Result<Option<(Vec<Value>, Vec<Unlock>)>, Error> {
    let Some(client_id) = client_id(product_id).await? else { return Ok(None) };
    // shortcut: names and descriptions are English; follow the app language when GOG locales are mapped.
    let url = format!("https://gameplay.gog.com/clients/{client_id}/users/{}/achievements?locale=en-US", session.user_id);
    let payload = get_json(client()?.get(url).bearer_auth(&session.token)).await?;
    Ok(Some(parse_achievements(&payload)))
}

async fn store_achievements(app: &AppHandle, product_id: &str, catalogue: Vec<Value>, unlocks: &[Unlock]) -> Result<(), String> {
    let key = key_of(product_id);
    let baseline = {
        let store = app.state::<Store>();
        let baseline = !store.get(UNLOCKED, &key)?.is_null();
        store.put(CATALOGUE, &key, &json!({ "language": "en", "achievements": catalogue, "updatedAt": now_ms() }))?;
        baseline
    };
    // No pop-ups or page updates on the first sync of a game.
    achievements::merge(app, "custom", &object_id(product_id), unlocks, baseline, baseline).await?;
    let store = app.state::<Store>();
    achievements::store_counts(&store, &key, &achievements::unlocked(&store, &key), &catalogue)
}

async fn sync_achievements(app: &AppHandle, session: &Session, games: &[LibraryGame], cancel: &Arc<AtomicBool>, on_progress: impl Fn(usize) + Send + Sync + 'static) -> Result<(), Error> {
    let queue: Arc<Mutex<VecDeque<LibraryGame>>> = Arc::new(Mutex::new(games.iter().filter(|g| g.has_achievements && g.play_ms > 0).cloned().collect()));
    let skipped = games.len() - lock(&queue).len();
    let processed = Arc::new(AtomicUsize::new(skipped));
    let on_progress = Arc::new(on_progress);
    on_progress(skipped);
    let mut workers = Vec::new();
    for _ in 0..ACHIEVEMENT_CONCURRENCY {
        let (app, session, cancel, queue, processed, on_progress) = (app.clone(), session.clone(), cancel.clone(), queue.clone(), processed.clone(), on_progress.clone());
        workers.push(tauri::async_runtime::spawn(async move {
            loop {
                check(&cancel)?;
                let Some(game) = lock(&queue).pop_front() else { return Ok::<(), Error>(()) };
                match with_retry(&format!("achievements {}", game.id), &cancel, || game_achievements(&session, &game.id)).await {
                    Ok(Some((catalogue, unlocks))) if !catalogue.is_empty() => {
                        if let Err(e) = store_achievements(&app, &game.id, catalogue, &unlocks).await {
                            eprintln!("[gog] failed to store achievements for {}: {e}", game.id);
                        }
                    }
                    Ok(_) => {}
                    Err(e) if e.is_abort() || e.status() == Some(401) => return Err(if e.is_abort() { e } else { Error::Message(SESSION_REQUIRED) }),
                    // One game's achievements failing does not stop the sync.
                    Err(e) => eprintln!("[gog] skipping achievements for {} {}: {e:?}", game.id, game.title),
                }
                on_progress(processed.fetch_add(1, Ordering::Relaxed) + 1);
            }
        }));
    }
    let mut first_error = None;
    for worker in workers {
        if let Err(e) = worker.await.map_err(|e| Error::Transport(e.to_string())).and_then(|r| r) {
            cancel.store(true, Ordering::Relaxed);
            if first_error.as_ref().is_none_or(|f: &Error| f.is_abort()) {
                first_error = Some(e);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

// ---- Sync state ----

#[derive(Default)]
pub struct GogSync {
    state: Mutex<Value>,
    cancel: Mutex<Option<Arc<AtomicBool>>>,
    running: tokio::sync::Mutex<()>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl GogSync {
    pub fn state(&self) -> Value {
        let state = lock(&self.state).clone();
        if state.is_null() { json!({ "status": "idle" }) } else { state }
    }

    fn set_state(&self, app: &AppHandle, state: Value) {
        *lock(&self.state) = state.clone();
        let _ = app.emit("on-gog-sync-progress", state);
    }

    fn progress(&self, app: &AppHandle, phase: &str, found: usize, processed: usize) {
        if self.state()["status"] == "running" {
            self.set_state(app, json!({ "status": "running", "phase": phase, "gamesFound": found, "gamesProcessed": processed }));
        }
    }

    fn clear_reconnect_required(&self, app: &AppHandle) {
        let state = self.state();
        if state["status"] == "idle" && state["requiresReconnect"] == json!(true) {
            self.set_state(app, json!({ "status": "idle" }));
        }
    }

    /// cancelGogSync: the run notices at its next step.
    pub async fn cancel(&self, app: &AppHandle) {
        if self.state()["status"] != "running" {
            return;
        }
        self.set_state(app, json!({ "status": "cancelling" }));
        if let Some(flag) = lock(&self.cancel).as_ref() {
            flag.store(true, Ordering::Relaxed);
        }
        drop(self.running.lock().await);
    }
}

/// startGogSync: runs in the background; a second start while one runs returns the current state.
pub fn start(app: &AppHandle, origin: &'static str) -> Value {
    let sync = app.state::<GogSync>();
    let state = sync.state();
    if state["status"] == "running" || state["status"] == "cancelling" {
        return state;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    *lock(&sync.cancel) = Some(cancel.clone());
    sync.set_state(app, json!({ "status": "running", "phase": "starting", "gamesFound": 0, "gamesProcessed": 0 }));
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let sync = app.state::<GogSync>();
        let _running = sync.running.lock().await;
        eprintln!("[gog] sync started");
        let finished = match run(&app, &cancel).await {
            Ok(status) => {
                eprintln!("[gog] sync succeeded");
                sync.set_state(&app, json!({ "status": "idle" }));
                json!({ "ok": true, "status": status, "origin": origin })
            }
            Err(error) => {
                let message = if error.is_abort() { ABORTED.to_string() } else { error.message() };
                eprintln!("[gog] sync failed: {message}");
                let reconnect = message == SESSION_REQUIRED || message == ACCOUNT_MISMATCH;
                sync.set_state(&app, if reconnect { json!({ "status": "idle", "requiresReconnect": true }) } else { json!({ "status": "idle" }) });
                json!({ "ok": false, "message": message, "origin": origin })
            }
        };
        *lock(&sync.cancel) = None;
        let _ = app.emit("on-gog-sync-finished", finished);
    });
    sync.state()
}

async fn run(app: &AppHandle, cancel: &Arc<AtomicBool>) -> Result<Value, Error> {
    let sync = app.state::<GogSync>();
    sync.progress(app, "library", 0, 0);
    let linked_id = account(&app.state::<Store>())["userId"].as_str().map(str::to_string).ok_or(Error::Message(SESSION_REQUIRED))?;
    let session = open_session(app, cancel).await?;
    if session.user_id != linked_id {
        return Err(Error::Message(ACCOUNT_MISMATCH));
    }
    let games = fetch_library(&session, cancel).await?;
    check(cancel)?;

    sync.progress(app, "artwork", games.len(), 0);
    let new_ids = missing(&app.state::<Store>(), &games).map_err(Error::Transport)?;
    let mut art = HashMap::new();
    for (done, id) in new_ids.iter().enumerate() {
        check(cancel)?;
        art.insert(id.clone(), fetch_art(id).await);
        sync.progress(app, "artwork", new_ids.len(), done + 1);
    }

    // Merged before achievements so their counts land on records that exist.
    sync.progress(app, "merging", games.len(), 0);
    let installs = tauri::async_runtime::spawn_blocking(crate::launcher_import::detect).await.unwrap_or_default();
    {
        let store = app.state::<Store>();
        merge_library(&store, &games, &art).map_err(Error::Transport)?;
        // An empty library is far likelier a bad response than a cleared account.
        if !games.is_empty() {
            hide_removed(&store, &games.iter().map(|g| key_of(&g.id)).collect()).map_err(Error::Transport)?;
        }
        let linked = link_installs(&store, &installs).map_err(Error::Transport)?;
        eprintln!("[gog] {} games, {} new, {linked} installs linked", games.len(), new_ids.len());
    }
    sync.progress(app, "merging", games.len(), games.len());
    let _ = app.emit("on-library-batch-complete", ());

    sync.progress(app, "achievements", games.len(), 0);
    let (progress_app, total, cancel_flag) = (app.clone(), games.len(), cancel.clone());
    sync_achievements(app, &session, &games, cancel, move |done| {
        if !cancel_flag.load(Ordering::Relaxed) {
            progress_app.state::<GogSync>().progress(&progress_app, "achievements", total, done);
        }
    })
    .await?;
    check(cancel)?;
    let _ = app.emit("on-library-batch-complete", ());

    let store = app.state::<Store>();
    let mut linked_account = account(&store);
    if linked_account.is_object() {
        linked_account["lastSyncedAt"] = json!(iso_now());
        store.put("", ACCOUNT_KEY, &linked_account).map_err(Error::Transport)?;
    }
    Ok(status_of(linked_account))
}

/// A linked account last synced over six hours ago syncs on launch.
pub fn start_on_launch(app: &AppHandle) {
    let last = account(&app.state::<Store>());
    if last.is_object() && last["lastSyncedAt"].as_str().and_then(parse_iso_ms).is_none_or(|at| now_ms().saturating_sub(at) > STARTUP_MIN_AGE_MS) {
        start(app, "startup");
    }
}

// ---- Disconnect ----

fn disconnect_games(store: &Store, delete_imported: bool) -> Result<(), String> {
    for (key, game) in store.entries("games")? {
        let synced = game["hasActiveGogImport"] == json!(true) || game["gogImported"] == json!(true) || game["gogPlayTimeInMilliseconds"].as_u64().unwrap_or(0) > 0;
        if !synced {
            continue;
        }
        if !delete_imported {
            update(store, &key, |g| {
                g.insert("hasActiveGogImport".into(), json!(false));
            })?;
            continue;
        }
        store.del(UNLOCKED, &key)?;
        store.del(CATALOGUE, &key)?;
        let remove = game["gogImported"] == json!(true) && !has_local_install(&game) && game["playTimeInMilliseconds"].as_f64().unwrap_or(0.0) <= 0.0 && store.get("downloads", &key)?.is_null();
        update(store, &key, |g| {
            g.insert("hasActiveGogImport".into(), json!(false));
            g.insert("gogPlayTimeInMilliseconds".into(), json!(0));
            g.insert("achievementCount".into(), json!(0));
            g.insert("unlockedAchievementCount".into(), json!(0));
            g.remove("gogImported");
            if remove {
                g.insert("isDeleted".into(), json!(true));
                g.insert("lastTimePlayed".into(), Value::Null);
            }
        })?;
    }
    Ok(())
}

/// disconnectGog: unlinks the account, optionally removes what the sync brought in, and signs the webview out.
pub async fn disconnect(app: &AppHandle, delete_imported: bool) -> Result<(), String> {
    {
        let store = app.state::<Store>();
        disconnect_games(&store, delete_imported)?;
        store.del("", ACCOUNT_KEY)?;
    }
    clear_webview_data(app).await;
    let _ = app.emit("on-library-batch-complete", ());
    eprintln!("[gog] disconnected (delete imported data: {delete_imported})");
    Ok(())
}

async fn clear_webview_data(app: &AppHandle) {
    let Ok(dir) = webview_dir(app) else { return };
    if !dir.exists() {
        return;
    }
    if let Some(old) = app.get_webview_window(SESSION_LABEL) {
        let _ = old.destroy();
    }
    match WebviewWindowBuilder::new(app, SESSION_LABEL, WebviewUrl::External("about:blank".parse().expect("static url"))).visible(false).data_directory(dir).build() {
        Ok(window) => {
            if let Err(e) = window.clear_all_browsing_data() {
                eprintln!("[gog] failed to clear the GOG webview data: {e}");
            }
            // The clear is asynchronous; give it a moment before the webview goes.
            tokio::time::sleep(Duration::from_secs(1)).await;
            let _ = window.destroy();
        }
        Err(e) => eprintln!("[gog] failed to open the GOG webview to clear it: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(id: &str, play_ms: u64) -> LibraryGame {
        LibraryGame { id: id.into(), title: format!("Game {id}"), play_ms, last_played: Some(1_700_000_000_000), has_achievements: true }
    }

    #[test]
    fn account_needs_login_id_and_name() {
        let session = parse_account(&json!({ "isLoggedIn": true, "userId": "4800", "username": "cat", "accessToken": "tok" })).unwrap();
        assert_eq!((session.user_id.as_str(), session.username.as_str(), session.token.as_str()), ("4800", "cat", "tok"));
        assert!(parse_account(&json!({ "isLoggedIn": false, "cacheExpires": 1 })).is_none());
        assert!(parse_account(&json!({ "isLoggedIn": true, "userId": "1" })).is_none());
    }

    #[test]
    fn products_skip_movies_and_report_pages() {
        let (games, pages) = parse_products(&json!({ "totalPages": 3, "products": [
            { "id": 1207664643, "title": "The Witcher 3", "isGame": true, "isMovie": false },
            { "id": 5, "title": "A Film", "isGame": false, "isMovie": true },
            { "id": 6, "title": "" },
        ]}));
        assert_eq!(pages, 3);
        assert_eq!(games.iter().map(|g| g.id.as_str()).collect::<Vec<_>>(), ["1207664643"]);
    }

    #[test]
    fn gog_times_apply_their_offset() {
        let utc = parse_iso_ms("2024-05-01T10:00:00.000Z").unwrap();
        assert_eq!(parse_gog_time("2024-05-01T10:00:00+00:00"), Some(utc));
        assert_eq!(parse_gog_time("2024-05-01T12:00:00+0200"), Some(utc));
        assert_eq!(parse_gog_time("2024-05-01T08:30:00-01:30"), Some(utc));
        assert_eq!(parse_gog_time("2024-05-01T10:00:00"), Some(utc));
        assert_eq!(parse_gog_time("soon"), None);
    }

    #[test]
    fn stats_give_playtime_and_last_session() {
        let (stats, pages) = parse_stats(&json!({ "pages": 2, "_embedded": { "items": [
            { "game": { "id": "1207664643", "achievementSupport": true }, "stats": { "4800": { "playtime": 90, "lastSession": "2024-05-01T10:00:00+00:00" } } },
            { "game": { "id": 7 }, "stats": [] },
        ]}}));
        assert_eq!(pages, 2);
        assert_eq!(stats["1207664643"], (5_400_000, parse_iso_ms("2024-05-01T10:00:00Z"), true));
        assert_eq!(stats["7"], (0, None, false));
        let merged = apply_stats(vec![LibraryGame { id: "1207664643".into(), title: "W".into(), ..Default::default() }, LibraryGame { id: "1207664643".into(), title: "W".into(), ..Default::default() }], &stats);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].play_ms, 5_400_000);
    }

    #[test]
    fn art_fills_gamesdb_formats() {
        let art = parse_art(&json!({ "game": { "vertical_cover": { "url_format": "https://images.gog.com/abc{formatter}.{ext}?namespace=gamesdb" }, "logo": { "url_format": "https://images.gog.com/l{formatter}.{ext}" } } }));
        assert_eq!(art.cover.as_deref(), Some("https://images.gog.com/abc.jpg?namespace=gamesdb"));
        assert_eq!(art.logo.as_deref(), Some("https://images.gog.com/l.png"));
        assert_eq!(art.hero, None);
    }

    #[test]
    fn build_meta_reads_zlib_and_plain() {
        use std::io::Write;
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(br#"{"clientId":"47746531845925190"}"#).unwrap();
        assert_eq!(parse_build_meta(&encoder.finish().unwrap()).unwrap()["clientId"], "47746531845925190");
        assert_eq!(parse_build_meta(br#"{"clientId":"1"}"#).unwrap()["clientId"], "1");
        assert!(parse_build_meta(b"\x00\x01").is_none());
    }

    #[test]
    fn achievements_map_to_the_catalogue_shape() {
        let (catalogue, unlocks) = parse_achievements(&json!({ "items": [
            { "achievement_key": "KILL_GRIFFIN", "name": "Griffin", "description": "Kill it", "image_url_unlocked": "u.png", "image_url_locked": "l.png", "visible": true, "date_unlocked": "2024-05-01T10:00:00+0000" },
            { "achievement_key": "SECRET", "name": "Secret", "visible": false, "date_unlocked": null },
            { "name": "no key" },
        ]}));
        assert_eq!(catalogue.len(), 2);
        assert_eq!(catalogue[0], json!({ "name": "KILL_GRIFFIN", "displayName": "Griffin", "description": "Kill it", "icon": "u.png", "icongray": "l.png", "hidden": false }));
        assert_eq!(catalogue[1]["hidden"], true);
        assert_eq!(unlocks.len(), 1);
        assert_eq!((unlocks[0].name.as_str(), unlocks[0].unlock_time), ("KILL_GRIFFIN", parse_iso_ms("2024-05-01T10:00:00Z").unwrap() as i64));
    }

    #[test]
    fn merge_joins_the_launcher_import_record() {
        let store = Store::memory().unwrap();
        // A game the launcher import added from the local install.
        crate::artwork::create_custom_game(&store, "gog-1", "Installed", Some(r"C:\GOG\g.exe"), None, None, None).unwrap();
        update(&store, "custom:gog-1", |g| {
            g.insert("externalLaunch".into(), json!({ "kind": "exe", "path": r"C:\GOG\g.exe", "args": [] }));
        })
        .unwrap();
        let art = HashMap::from([("2".to_string(), Art { cover: Some("c.jpg".into()), ..Default::default() })]);
        assert_eq!(missing(&store, &[game("1", 0), game("2", 60_000)]).unwrap(), ["2"]);
        assert_eq!(merge_library(&store, &[game("1", 120_000), game("2", 60_000)], &art).unwrap(), 1);

        let installed = store.get("games", "custom:gog-1").unwrap();
        assert_eq!(installed["executablePath"], r"C:\GOG\g.exe");
        assert_eq!(installed["externalLaunch"]["kind"], "exe");
        assert_eq!(installed["gogPlayTimeInMilliseconds"], 120_000);
        assert_eq!(installed["hasActiveGogImport"], true);
        assert!(installed.get("gogImported").is_none());

        let owned = store.get("games", "custom:gog-2").unwrap();
        assert_eq!(owned["iconUrl"], "c.jpg");
        assert_eq!(owned["gogImported"], true);
        assert_eq!(owned["externalLaunch"], galaxy_launch("2"));
        assert_eq!(owned["launcher"], "gog");
        assert!(owned["lastTimePlayed"].is_string());
    }

    #[test]
    fn removed_games_hide_only_sync_only_records() {
        let store = Store::memory().unwrap();
        crate::artwork::create_custom_game(&store, "gog-1", "Installed", Some("g.exe"), None, None, None).unwrap();
        merge_library(&store, &[game("1", 0), game("2", 0), game("3", 0)], &HashMap::new()).unwrap();
        hide_removed(&store, &HashSet::from([key_of("3")])).unwrap();
        let get = |id: &str| store.get("games", &key_of(id)).unwrap();
        assert_eq!((get("1")["isDeleted"].clone(), get("1")["hasActiveGogImport"].clone()), (json!(false), json!(false)));
        assert_eq!(get("2")["isDeleted"], true);
        assert_eq!(get("3")["hasActiveGogImport"], true);
    }

    #[test]
    fn installs_link_to_records_without_an_exe() {
        let store = Store::memory().unwrap();
        merge_library(&store, &[game("1", 0)], &HashMap::new()).unwrap();
        let install = crate::launcher_import::Detected { launcher: "gog", id: "1".into(), title: "G".into(), install_dir: r"C:\G".into(), exe_path: Some(r"C:\G\g.exe".into()), external_launch: Some(json!({ "kind": "exe", "path": r"C:\G\g.exe", "args": [] })) };
        assert_eq!(link_installs(&store, std::slice::from_ref(&install)).unwrap(), 1);
        assert_eq!(store.get("games", &key_of("1")).unwrap()["executablePath"], r"C:\G\g.exe");
        assert_eq!(link_installs(&store, &[install]).unwrap(), 0);
    }

    #[test]
    fn disconnect_keeps_or_removes_imported_data() {
        let setup = || {
            let store = Store::memory().unwrap();
            crate::artwork::create_custom_game(&store, "gog-1", "Installed", Some("g.exe"), None, None, None).unwrap();
            merge_library(&store, &[game("1", 60_000), game("2", 60_000)], &HashMap::new()).unwrap();
            store.put(UNLOCKED, &key_of("2"), &json!([{ "name": "A", "unlockTime": 1 }])).unwrap();
            store
        };
        let store = setup();
        disconnect_games(&store, false).unwrap();
        let kept = store.get("games", &key_of("2")).unwrap();
        assert_eq!((kept["isDeleted"].clone(), kept["hasActiveGogImport"].clone(), kept["gogPlayTimeInMilliseconds"].clone()), (json!(false), json!(false), json!(60_000)));
        assert!(!store.get(UNLOCKED, &key_of("2")).unwrap().is_null());

        let store = setup();
        disconnect_games(&store, true).unwrap();
        assert_eq!(store.get("games", &key_of("2")).unwrap()["isDeleted"], true);
        assert!(store.get(UNLOCKED, &key_of("2")).unwrap().is_null());
        let installed = store.get("games", &key_of("1")).unwrap();
        assert_eq!((installed["isDeleted"].clone(), installed["gogPlayTimeInMilliseconds"].clone()), (json!(false), json!(0)));
    }
}
