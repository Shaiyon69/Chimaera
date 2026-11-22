//! Epic Games account link and library sync, local only (nothing goes to Hydra's servers).
//! Endpoints follow Legendary (legendary-gl/legendary, GPL-3.0, read for reference only) and
//! Hydra 4.4.1's Epic service; the launcher client id and secret are Epic's public launcher pair
//! that Legendary, Heroic and Playnite also use.
//!
//! Login runs epicgames.com in a webview with its own data folder. When Epic lands on its
//! `id/api/redirect` page, the webview's cookies fetch that page's authorization code, which is
//! exchanged for tokens. The refresh token lives in the OS keyring, never in the store.
//!
//! Records share the launcher import's key (`custom:epic-<appName>`), so an owned game and its
//! local install are one library entry.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Map, Value};
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::achievement_files::Unlock;
use crate::achievements::{self, CATALOGUE, UNLOCKED};
use crate::data_location::DataDir;
use crate::games::{game_key, update, with_executable_path};
use crate::launcher_import::launcher_object_id;
use crate::library::{iso_now, now_ms};
use crate::steam::parse_iso_ms;
use crate::store::Store;

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
// The API calls identify as the Epic launcher, as Legendary does.
const API_USER_AGENT: &str = "UELauncher/11.0.1-14907503+++Portal+Release-Live Windows/10.0.19041.1.256.64bit";
const CLIENT_ID: &str = "34a02cf8f4414e29b15921876da36f9a";
const CLIENT_SECRET: &str = "daafbccc737745039dffe53d94fc76cf";
const OAUTH_URL: &str = "https://account-public-service-prod03.ol.epicgames.com/account/api/oauth/token";
const LIBRARY_URL: &str = "https://library-service.live.use1a.on.epicgames.com/library/api/public";
const CATALOG_URL: &str = "https://catalog-public-service-prod06.ol.epicgames.com/catalog/api/shared/namespace";
const GRAPHQL_URL: &str = "https://launcher.store.epicgames.com/graphql";
const EPIC_HOME: &str = "https://www.epicgames.com";

const ACCOUNT_KEY: &str = "epicAccount";
const LOGIN_LABEL: &str = "epic-login";
const WEBVIEW_DIR: &str = "epic-webview";
const KEYRING_SERVICE: &str = "chimaera";
const KEYRING_ENTRY: &str = "epic-refresh-token";
const ACHIEVEMENT_CONCURRENCY: usize = 4;
const STARTUP_MIN_AGE_MS: u64 = 6 * 60 * 60 * 1000;

const SESSION_REQUIRED: &str = "epic-session-required";
const ACCOUNT_MISMATCH: &str = "epic-account-mismatch";
const RATE_LIMITED: &str = "epic-rate-limited";
const ABORTED: &str = "epic-sync-aborted";

fn redirect_url() -> String {
    format!("{EPIC_HOME}/id/api/redirect?clientId={CLIENT_ID}&responseType=code")
}

fn login_url() -> String {
    let redirect: String = url::form_urlencoded::byte_serialize(redirect_url().as_bytes()).collect();
    format!("{EPIC_HOME}/id/login?redirectUrl={redirect}")
}

// ---- Errors and retries (same rules as gog.rs, Epic error codes) ----

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
            Error::Http { status: 400 | 401 | 403, .. } => SESSION_REQUIRED.into(),
            Error::Http { status, .. } => format!("epic-http-{status}"),
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

fn check(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Message(ABORTED));
    }
    Ok(())
}

async fn sleep(ms: u64, cancel: &AtomicBool) -> Result<(), Error> {
    let until = std::time::Instant::now() + Duration::from_millis(ms);
    while std::time::Instant::now() < until {
        check(cancel)?;
        tokio::time::sleep(Duration::from_millis(100).min(until - std::time::Instant::now())).await;
    }
    check(cancel)
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
                eprintln!("[epic] {label} retry {failed} after HTTP {:?}, waiting {delay}ms", error.status());
                sleep(delay, cancel).await?;
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---- HTTP ----

fn client() -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        .user_agent(API_USER_AGENT)
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

async fn authed_get(session: &Session, url: &str) -> Result<Value, Error> {
    get_json(client()?.get(url).bearer_auth(&session.access_token)).await
}

// ---- Tokens ----

#[derive(Clone, Debug, Default, PartialEq)]
struct Session {
    account_id: String,
    display_name: String,
    access_token: String,
    refresh_token: String,
}

/// The OAuth token response: access token, refresh token and the account it belongs to.
fn parse_token(payload: &Value) -> Option<Session> {
    let text = |field: &str| payload[field].as_str().filter(|v| !v.is_empty()).map(str::to_string);
    Some(Session {
        account_id: text("account_id")?,
        display_name: text("displayName").unwrap_or_default(),
        access_token: text("access_token")?,
        refresh_token: text("refresh_token")?,
    })
}

async fn token(grant: &[(&str, &str)]) -> Result<Session, Error> {
    let mut form = vec![("token_type", "eg1")];
    form.extend_from_slice(grant);
    let payload = get_json(client()?.post(OAUTH_URL).basic_auth(CLIENT_ID, Some(CLIENT_SECRET)).form(&form)).await?;
    parse_token(&payload).ok_or(Error::Message(SESSION_REQUIRED))
}

fn keyring_entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_ENTRY).map_err(|e| e.to_string())
}

fn save_refresh_token(token: &str) -> Result<(), String> {
    keyring_entry()?.set_password(token).map_err(|e| format!("keyring: {e}"))
}

fn load_refresh_token() -> Option<String> {
    keyring_entry().ok()?.get_password().ok()
}

fn delete_refresh_token() {
    if let Ok(entry) = keyring_entry() {
        let _ = entry.delete_credential();
    }
}

/// A fresh access token from the stored refresh token; Epic rotates the refresh token each time.
async fn open_session() -> Result<Session, Error> {
    let refresh = load_refresh_token().ok_or(Error::Message(SESSION_REQUIRED))?;
    let session = token(&[("grant_type", "refresh_token"), ("refresh_token", &refresh)]).await.map_err(|e| match e {
        Error::Http { status: 400 | 401, .. } => Error::Message(SESSION_REQUIRED),
        other => other,
    })?;
    save_refresh_token(&session.refresh_token).map_err(Error::Transport)?;
    Ok(session)
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

/// getEpicConnection.
pub fn integration_status(store: &Store) -> Value {
    status_of(account(store))
}

fn save_account(app: &AppHandle, session: &Session) -> Result<(), String> {
    save_refresh_token(&session.refresh_token)?;
    let store = app.state::<Store>();
    let existing = account(&store);
    store.put("", ACCOUNT_KEY, &json!({
        "accountId": session.account_id,
        "displayName": session.display_name,
        "connectedAt": existing["connectedAt"].as_str().map_or_else(iso_now, str::to_string),
        "lastSyncedAt": if existing["accountId"] == json!(session.account_id) { existing["lastSyncedAt"].clone() } else { Value::Null },
    }))?;
    app.state::<EpicSync>().clear_reconnect_required(app);
    Ok(())
}

fn webview_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(app.path().chimaera_data_dir().map_err(|e| e.to_string())?.join(WEBVIEW_DIR))
}

fn is_redirect_page(url: &url::Url) -> bool {
    url.host_str() == Some("www.epicgames.com") && url.path() == "/id/api/redirect"
}

fn cookie_header(cookies: &[tauri::webview::Cookie<'static>]) -> String {
    cookies.iter().filter(|c| !c.value().is_empty()).map(|c| format!("{}={}", c.name(), c.value())).collect::<Vec<_>>().join("; ")
}

/// The authorization code on Epic's redirect page, fetched again with the webview's cookies.
async fn authorization_code(cookies: &str) -> Result<String, Error> {
    let request = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| Error::Transport(e.to_string()))?
        .get(redirect_url())
        .header("Cookie", cookies)
        .header("Accept", "application/json");
    let payload = get_json(request).await?;
    payload["authorizationCode"].as_str().filter(|c| !c.is_empty()).map(str::to_string).ok_or(Error::Message(SESSION_REQUIRED))
}

fn show_integrations(app: &AppHandle) {
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.unminimize();
        let _ = main.show();
        let _ = main.set_focus();
        let _ = main.emit("on-navigate", "/settings?tab=integrations");
    }
}

/// startEpicAuth: Epic's login page; the account is saved once its redirect page hands out a code.
pub fn start_login(app: &AppHandle) -> Result<(), String> {
    if let Some(old) = app.get_webview_window(LOGIN_LABEL) {
        let _ = old.destroy();
    }
    let done = Arc::new(AtomicBool::new(false));
    let handle = app.clone();
    let mut builder = WebviewWindowBuilder::new(app, LOGIN_LABEL, WebviewUrl::External(login_url().parse().map_err(|e: url::ParseError| e.to_string())?))
        .title("Epic Games")
        .inner_size(720.0, 860.0)
        .user_agent(USER_AGENT)
        .data_directory(webview_dir(app)?)
        .on_page_load(move |webview, payload| {
            if payload.event() != PageLoadEvent::Finished || !is_redirect_page(payload.url()) || done.swap(true, Ordering::Relaxed) {
                return;
            }
            let (app, window) = (handle.clone(), webview.clone());
            // Reading cookies deadlocks on the event thread on Windows, so it runs on the async runtime.
            // The task keeps this window, not its label, so it never closes a newer login window.
            tauri::async_runtime::spawn(async move {
                let result = async {
                    let cookies = window.cookies_for_url(EPIC_HOME.parse().expect("static url")).map_err(|e| Error::Transport(format!("epic cookies: {e}")))?;
                    let code = authorization_code(&cookie_header(&cookies)).await?;
                    token(&[("grant_type", "authorization_code"), ("code", &code)]).await
                }
                .await;
                let _ = window.destroy();
                match result.map_err(|e| e.message()).and_then(|session| save_account(&app, &session)) {
                    Ok(()) => {
                        eprintln!("[epic] login completed");
                        let _ = app.emit("on-epic-connected", ());
                    }
                    Err(e) => {
                        eprintln!("[epic] login failed: {e}");
                        let _ = app.emit("on-epic-connect-error", "generic");
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
    app_name: String,
    namespace: String,
    catalog_item_id: String,
    title: String,
    /// None when Epic's playtime list could not be read; the stored playtime then stays.
    play_ms: Option<u64>,
    cover: Option<String>,
    hero: Option<String>,
    logo: Option<String>,
}

/// One page of library items: (namespace, catalogItemId, appName) per record, and the next cursor.
fn parse_items(payload: &Value) -> (Vec<(String, String, String)>, Option<String>) {
    let items = payload["records"]
        .as_array()
        .into_iter()
        .flatten()
        // Unreal Engine plugins and marketplace assets live in the "ue" namespace.
        .filter(|r| r["namespace"].as_str() != Some("ue") && r["sandboxType"].as_str() != Some("PRIVATE"))
        .filter_map(|r| {
            let text = |f: &str| r[f].as_str().filter(|v| !v.is_empty()).map(str::to_string);
            Some((text("namespace")?, text("catalogItemId")?, text("appName")?))
        })
        .collect();
    let next = payload["responseMetadata"]["nextCursor"].as_str().filter(|c| !c.is_empty()).map(str::to_string);
    (items, next)
}

fn key_image(item: &Value, kinds: &[&str]) -> Option<String> {
    let images = item["keyImages"].as_array()?;
    kinds.iter().find_map(|kind| images.iter().find(|i| i["type"].as_str() == Some(kind)).and_then(|i| i["url"].as_str()).map(str::to_string))
}

/// A catalog item as a game, or None for DLC, add-ons and anything without a title.
/// (title, cover, hero, logo).
type CatalogGame = (String, Option<String>, Option<String>, Option<String>);

fn parse_catalog_item(item: &Value) -> Option<CatalogGame> {
    let categories: Vec<&str> = item["categories"].as_array().into_iter().flatten().filter_map(|c| c["path"].as_str()).collect();
    // Same rule as the launcher import: add-ons are skipped unless Epic marks them launchable.
    let is_dlc = categories.contains(&"addons") && !categories.contains(&"addons/launchable");
    if is_dlc || (item["mainGameItem"].is_object() && !categories.contains(&"addons/launchable")) {
        return None;
    }
    let title = item["title"].as_str().filter(|t| !t.trim().is_empty())?.trim().to_string();
    Some((
        title,
        key_image(item, &["DieselGameBoxTall", "OfferImageTall", "Thumbnail"]),
        key_image(item, &["DieselGameBox", "OfferImageWide", "DieselStoreFrontWide"]),
        key_image(item, &["DieselGameBoxLogo"]),
    ))
}

/// The playtime list: appName (artifactId) to milliseconds.
fn parse_playtime(payload: &Value) -> HashMap<String, u64> {
    payload
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some((p["artifactId"].as_str()?.to_string(), p["totalTime"].as_u64()? * 1000)))
        .collect()
}

/// The owned games, plus every appName the library listed (games whose catalog lookup failed included).
async fn fetch_library(session: &Session, cancel: &AtomicBool) -> Result<(Vec<LibraryGame>, HashSet<String>), Error> {
    let mut items = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut url = format!("{LIBRARY_URL}/items?includeMetadata=true");
        if let Some(c) = &cursor {
            url.push_str(&format!("&cursor={}", url::form_urlencoded::byte_serialize(c.as_bytes()).collect::<String>()));
        }
        let payload = with_retry("library", cancel, || authed_get(session, &url)).await?;
        let (found, next) = parse_items(&payload);
        items.extend(found);
        match next {
            Some(next) if Some(&next) != cursor.as_ref() => cursor = Some(next),
            _ => break,
        }
    }

    let listed: HashSet<String> = items.iter().map(|(_, _, app_name)| app_name.clone()).collect();
    let mut by_namespace: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut seen = HashSet::new();
    for (namespace, item, app_name) in items {
        if seen.insert(app_name.clone()) {
            by_namespace.entry(namespace).or_default().push((item, app_name));
        }
    }

    let mut games = Vec::new();
    for (namespace, entries) in by_namespace {
        check(cancel)?;
        let ids: Vec<&str> = entries.iter().map(|(id, _)| id.as_str()).collect();
        let url = format!("{CATALOG_URL}/{namespace}/bulk/items?id={}&includeDLCDetails=true&includeMainGameDetails=true&country=US&locale=en-US", ids.join(","));
        let catalog = match with_retry("catalog", cancel, || authed_get(session, &url)).await {
            Ok(c) => c,
            Err(e) if e.is_abort() => return Err(e),
            // One namespace failing leaves its games out of this sync, not the whole library.
            Err(e) => {
                eprintln!("[epic] no catalog for namespace {namespace}: {e:?}");
                continue;
            }
        };
        for (item_id, app_name) in entries {
            let Some((title, cover, hero, logo)) = parse_catalog_item(&catalog[&item_id]) else { continue };
            games.push(LibraryGame { app_name, namespace: namespace.clone(), catalog_item_id: item_id, title, cover, hero, logo, ..Default::default() });
        }
    }

    let playtime_url = format!("{LIBRARY_URL}/playtime/account/{}/all", session.account_id);
    let playtime = match with_retry("playtime", cancel, || authed_get(session, &playtime_url)).await {
        Ok(p) => Some(parse_playtime(&p)),
        Err(e) if e.is_abort() => return Err(e),
        // Playtime is extra; the library still imports without it.
        Err(e) => {
            eprintln!("[epic] playtime unavailable: {e:?}");
            None
        }
    };
    for game in &mut games {
        game.play_ms = playtime.as_ref().map(|p| p.get(&game.app_name).copied().unwrap_or(0));
    }
    games.sort_by_key(|g| g.title.to_lowercase());
    Ok((games, listed))
}

// ---- Merge ----

pub(crate) fn object_id(app_name: &str) -> String {
    launcher_object_id("epic", app_name)
}

fn key_of(app_name: &str) -> String {
    game_key("custom", &object_id(app_name))
}

fn launcher_launch(game: &LibraryGame) -> Value {
    json!({ "kind": "uri", "uri": format!("com.epicgames.launcher://apps/{}%3A{}%3A{}?action=launch&silent=true", game.namespace, game.catalog_item_id, game.app_name) })
}

/// Fields every synced record gets; local edits and the local install stay.
fn apply_sync_fields(record: &mut Map<String, Value>, game: &LibraryGame) {
    if let Some(play_ms) = game.play_ms {
        record.insert("epicPlayTimeInMilliseconds".into(), json!(play_ms));
    }
    record.insert("launcher".into(), json!("epic"));
    record.insert("launcherGameId".into(), json!(game.app_name));
    record.insert("hasActiveEpicImport".into(), json!(true));
    record.insert("isDeleted".into(), json!(false));
    if record.get("externalLaunch").is_none_or(Value::is_null) {
        record.insert("externalLaunch".into(), launcher_launch(game));
    }
}

fn merge_library(store: &Store, games: &[LibraryGame]) -> Result<usize, String> {
    let mut added = 0;
    for game in games {
        let key = key_of(&game.app_name);
        let existing = store.get("games", &key)?;
        if existing.is_null() {
            let image = |url: &Option<String>| url.clone().map(Value::String);
            crate::artwork::create_custom_game(store, &object_id(&game.app_name), &game.title, None, image(&game.cover).as_ref(), image(&game.logo).as_ref(), image(&game.hero).as_ref())?;
            update(store, &key, |g| {
                g.insert("epicImported".into(), json!(true));
            })?;
            added += 1;
        } else if existing["isDeleted"] == json!(true) {
            added += 1;
        }
        update(store, &key, |g| apply_sync_fields(g, game))?;
    }
    Ok(added)
}

fn has_local_install(game: &Value) -> bool {
    game["executablePath"].as_str().is_some_and(|p| !p.is_empty())
}

/// Games Epic no longer lists lose the import flag; ones the sync brought in with nothing local
/// (no install, no Chimaera playtime, no download) are hidden.
fn hide_removed(store: &Store, keep: &HashSet<String>) -> Result<(), String> {
    for (key, game) in store.entries("games")? {
        if game["hasActiveEpicImport"] == json!(true) && !keep.contains(&key) {
            let hide = game["epicImported"] == json!(true)
                && !has_local_install(&game)
                && game["playTimeInMilliseconds"].as_f64().unwrap_or(0.0) <= 0.0
                && store.get("downloads", &key)?.is_null();
            update(store, &key, |g| {
                g.insert("hasActiveEpicImport".into(), json!(false));
                if hide {
                    g.insert("isDeleted".into(), json!(true));
                }
            })?;
        }
    }
    Ok(())
}

/// Installed Epic games found on this machine bind their exe and launch task to synced records that have none.
fn link_installs(store: &Store, installs: &[crate::launcher_import::Detected]) -> Result<usize, String> {
    let mut linked = 0;
    for install in installs.iter().filter(|d| d.launcher == "epic") {
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

// ---- Achievements (launcher GraphQL) ----

// shortcut: query shapes from Hydra 4.4.1 (player) and the store's Achievement schema (definitions), not yet
// checked against a live account; a game whose query fails is skipped, not the sync.
const DEFINITIONS_QUERY: &str = "query Achievement($sandboxId: String!, $locale: String!) { Achievement { productAchievementsRecordBySandbox(sandboxId: $sandboxId, locale: $locale) { achievements { achievement { name hidden unlockedDisplayName lockedDisplayName unlockedDescription lockedDescription unlockedIconLink lockedIconLink } } } } }";
const PLAYER_QUERY: &str = "query PlayerAchievement($epicAccountId: String!, $sandboxId: String!) { PlayerAchievement { playerAchievementGameRecordsBySandbox(epicAccountId: $epicAccountId, sandboxId: $sandboxId) { records { playerAchievements { playerAchievement { achievementName unlocked unlockDate } } } } } }";

async fn graphql(session: &Session, query: &str, variables: Value) -> Result<Value, Error> {
    let body = json!({ "query": query, "variables": variables });
    get_json(client()?.post(GRAPHQL_URL).bearer_auth(&session.access_token).json(&body)).await
}

/// The catalogue in the Hydra shape the game page reads.
fn parse_definitions(payload: &Value) -> Vec<Value> {
    payload["data"]["Achievement"]["productAchievementsRecordBySandbox"]["achievements"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let a = &a["achievement"];
            let name = a["name"].as_str().filter(|n| !n.is_empty())?;
            Some(json!({
                "name": name,
                "displayName": a["unlockedDisplayName"].as_str().or(a["lockedDisplayName"].as_str()).unwrap_or(name),
                "description": a["unlockedDescription"].as_str().or(a["lockedDescription"].as_str()).unwrap_or_default(),
                "icon": a["unlockedIconLink"],
                "icongray": a["lockedIconLink"],
                "hidden": a["hidden"].as_bool().unwrap_or(false),
            }))
        })
        .collect()
}

fn parse_unlocks(payload: &Value) -> Vec<Unlock> {
    payload["data"]["PlayerAchievement"]["playerAchievementGameRecordsBySandbox"]["records"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|r| r["playerAchievements"].as_array().into_iter().flatten())
        .filter_map(|p| {
            let p = &p["playerAchievement"];
            if p["unlocked"] != json!(true) {
                return None;
            }
            Some(Unlock { name: p["achievementName"].as_str()?.to_string(), unlock_time: p["unlockDate"].as_str().and_then(parse_iso_ms)? as i64 })
        })
        .collect()
}

async fn game_achievements(session: &Session, namespace: &str) -> Result<(Vec<Value>, Vec<Unlock>), Error> {
    let definitions = graphql(session, DEFINITIONS_QUERY, json!({ "sandboxId": namespace, "locale": "en-US" })).await?;
    let catalogue = parse_definitions(&definitions);
    if catalogue.is_empty() {
        return Ok((catalogue, Vec::new()));
    }
    let player = graphql(session, PLAYER_QUERY, json!({ "epicAccountId": session.account_id, "sandboxId": namespace })).await?;
    Ok((catalogue, parse_unlocks(&player)))
}

async fn store_achievements(app: &AppHandle, app_name: &str, catalogue: Vec<Value>, unlocks: &[Unlock]) -> Result<(), String> {
    let key = key_of(app_name);
    let baseline = {
        let store = app.state::<Store>();
        let baseline = !store.get(UNLOCKED, &key)?.is_null();
        store.put(CATALOGUE, &key, &json!({ "language": "en", "achievements": catalogue, "updatedAt": now_ms() }))?;
        baseline
    };
    // No pop-ups or page updates on the first sync of a game.
    achievements::merge(app, "custom", &object_id(app_name), unlocks, baseline, baseline).await?;
    let store = app.state::<Store>();
    achievements::store_counts(&store, &key, &achievements::unlocked(&store, &key), &catalogue)
}

async fn sync_achievements(app: &AppHandle, session: &Session, games: &[LibraryGame], cancel: &Arc<AtomicBool>, on_progress: impl Fn(usize) + Send + Sync + 'static) -> Result<(), Error> {
    // Only played games can have unlocks; the rest keep the sync short.
    let queue: Arc<Mutex<VecDeque<LibraryGame>>> = Arc::new(Mutex::new(games.iter().filter(|g| g.play_ms.is_none_or(|ms| ms > 0)).cloned().collect()));
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
                match with_retry(&format!("achievements {}", game.app_name), &cancel, || game_achievements(&session, &game.namespace)).await {
                    Ok((catalogue, unlocks)) if !catalogue.is_empty() => {
                        if let Err(e) = store_achievements(&app, &game.app_name, catalogue, &unlocks).await {
                            eprintln!("[epic] failed to store achievements for {}: {e}", game.app_name);
                        }
                    }
                    Ok(_) => {}
                    Err(e) if e.is_abort() || e.status() == Some(401) => return Err(if e.is_abort() { e } else { Error::Message(SESSION_REQUIRED) }),
                    // One game's achievements failing does not stop the sync.
                    Err(e) => eprintln!("[epic] skipping achievements for {} {}: {e:?}", game.app_name, game.title),
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
pub struct EpicSync {
    state: Mutex<Value>,
    cancel: Mutex<Option<Arc<AtomicBool>>>,
    running: tokio::sync::Mutex<()>,
}

impl EpicSync {
    pub fn state(&self) -> Value {
        let state = lock(&self.state).clone();
        if state.is_null() { json!({ "status": "idle" }) } else { state }
    }

    fn set_state(&self, app: &AppHandle, state: Value) {
        *lock(&self.state) = state.clone();
        let _ = app.emit("on-epic-sync-progress", state);
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

    /// cancelEpicSync: the run notices at its next step.
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

/// Frees the cancel slot before the state goes idle, and only if it is still this run's flag,
/// so a sync started right after keeps its own.
fn release_cancel(sync: &EpicSync, cancel: &Arc<AtomicBool>) {
    let mut slot = lock(&sync.cancel);
    if slot.as_ref().is_some_and(|flag| Arc::ptr_eq(flag, cancel)) {
        *slot = None;
    }
}

/// startEpicSync: runs in the background; a second start while one runs returns the current state.
pub fn start(app: &AppHandle, origin: &'static str) -> Value {
    let sync = app.state::<EpicSync>();
    let state = sync.state();
    if state["status"] == "running" || state["status"] == "cancelling" {
        return state;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    *lock(&sync.cancel) = Some(cancel.clone());
    sync.set_state(app, json!({ "status": "running", "phase": "starting", "gamesFound": 0, "gamesProcessed": 0 }));
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let sync = app.state::<EpicSync>();
        let _running = sync.running.lock().await;
        eprintln!("[epic] sync started");
        let finished = match run(&app, &cancel).await {
            Ok(status) => {
                eprintln!("[epic] sync succeeded");
                release_cancel(&sync, &cancel);
                sync.set_state(&app, json!({ "status": "idle" }));
                json!({ "ok": true, "status": status, "origin": origin })
            }
            Err(error) => {
                let message = if error.is_abort() { ABORTED.to_string() } else { error.message() };
                eprintln!("[epic] sync failed: {message}");
                let reconnect = message == SESSION_REQUIRED || message == ACCOUNT_MISMATCH;
                release_cancel(&sync, &cancel);
                sync.set_state(&app, if reconnect { json!({ "status": "idle", "requiresReconnect": true, "reason": message }) } else { json!({ "status": "idle" }) });
                json!({ "ok": false, "message": message, "origin": origin })
            }
        };
        let _ = app.emit("on-epic-sync-finished", finished);
    });
    sync.state()
}

async fn run(app: &AppHandle, cancel: &Arc<AtomicBool>) -> Result<Value, Error> {
    let sync = app.state::<EpicSync>();
    sync.progress(app, "library", 0, 0);
    let linked_id = account(&app.state::<Store>())["accountId"].as_str().map(str::to_string).ok_or(Error::Message(SESSION_REQUIRED))?;
    let session = open_session().await?;
    if session.account_id != linked_id {
        return Err(Error::Message(ACCOUNT_MISMATCH));
    }
    let (games, listed) = fetch_library(&session, cancel).await?;
    check(cancel)?;

    // Merged before achievements so their counts land on records that exist.
    sync.progress(app, "merging", games.len(), 0);
    let installs = tauri::async_runtime::spawn_blocking(crate::launcher_import::detect).await.unwrap_or_default();
    {
        let store = app.state::<Store>();
        let added = merge_library(&store, &games).map_err(Error::Transport)?;
        // An empty library is far likelier a bad response than a cleared account.
        if !listed.is_empty() {
            hide_removed(&store, &listed.iter().map(|a| key_of(a)).collect()).map_err(Error::Transport)?;
        }
        let linked = link_installs(&store, &installs).map_err(Error::Transport)?;
        eprintln!("[epic] {} games, {added} new, {linked} installs linked", games.len());
    }
    sync.progress(app, "merging", games.len(), games.len());
    let _ = app.emit("on-library-batch-complete", ());

    sync.progress(app, "achievements", games.len(), 0);
    let (progress_app, total, cancel_flag) = (app.clone(), games.len(), cancel.clone());
    sync_achievements(app, &session, &games, cancel, move |done| {
        if !cancel_flag.load(Ordering::Relaxed) {
            progress_app.state::<EpicSync>().progress(&progress_app, "achievements", total, done);
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
        let synced = game["hasActiveEpicImport"] == json!(true) || game["epicImported"] == json!(true) || game["epicPlayTimeInMilliseconds"].as_u64().unwrap_or(0) > 0;
        if !synced {
            continue;
        }
        if !delete_imported {
            update(store, &key, |g| {
                g.insert("hasActiveEpicImport".into(), json!(false));
            })?;
            continue;
        }
        store.del(UNLOCKED, &key)?;
        store.del(CATALOGUE, &key)?;
        let remove = game["epicImported"] == json!(true) && !has_local_install(&game) && game["playTimeInMilliseconds"].as_f64().unwrap_or(0.0) <= 0.0 && store.get("downloads", &key)?.is_null();
        update(store, &key, |g| {
            g.insert("hasActiveEpicImport".into(), json!(false));
            g.insert("epicPlayTimeInMilliseconds".into(), json!(0));
            g.insert("achievementCount".into(), json!(0));
            g.insert("unlockedAchievementCount".into(), json!(0));
            g.remove("epicImported");
            if remove {
                g.insert("isDeleted".into(), json!(true));
                g.insert("lastTimePlayed".into(), Value::Null);
            }
        })?;
    }
    Ok(())
}

/// disconnectEpic: unlinks the account, forgets the refresh token, optionally removes what the sync brought in.
pub async fn disconnect(app: &AppHandle, delete_imported: bool) -> Result<(), String> {
    {
        let store = app.state::<Store>();
        disconnect_games(&store, delete_imported)?;
        store.del("", ACCOUNT_KEY)?;
    }
    delete_refresh_token();
    // A reconnect warning from an earlier failed sync means nothing once unlinked.
    app.state::<EpicSync>().set_state(app, json!({ "status": "idle" }));
    clear_webview_data(app).await;
    let _ = app.emit("on-library-batch-complete", ());
    eprintln!("[epic] disconnected (delete imported data: {delete_imported})");
    Ok(())
}

async fn clear_webview_data(app: &AppHandle) {
    let Ok(dir) = webview_dir(app) else { return };
    if !dir.exists() {
        return;
    }
    const CLEAR_LABEL: &str = "epic-clear";
    if let Some(old) = app.get_webview_window(CLEAR_LABEL) {
        let _ = old.destroy();
    }
    match WebviewWindowBuilder::new(app, CLEAR_LABEL, WebviewUrl::External("about:blank".parse().expect("static url"))).visible(false).data_directory(dir).build() {
        Ok(window) => {
            if let Err(e) = window.clear_all_browsing_data() {
                eprintln!("[epic] failed to clear the Epic webview data: {e}");
            }
            // The clear is asynchronous; give it a moment before the webview goes.
            tokio::time::sleep(Duration::from_secs(1)).await;
            let _ = window.destroy();
        }
        Err(e) => eprintln!("[epic] failed to open the Epic webview to clear it: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_response_needs_account_and_both_tokens() {
        let session = parse_token(&json!({ "access_token": "a", "refresh_token": "r", "account_id": "id1", "displayName": "Shai" })).unwrap();
        assert_eq!(session, Session { account_id: "id1".into(), display_name: "Shai".into(), access_token: "a".into(), refresh_token: "r".into() });
        assert!(parse_token(&json!({ "access_token": "a", "account_id": "id1" })).is_none());
    }

    #[test]
    fn library_items_skip_engine_plugins_and_private_sandboxes() {
        let (items, next) = parse_items(&json!({
            "records": [
                { "namespace": "fn", "catalogItemId": "c1", "appName": "Fortnite" },
                { "namespace": "ue", "catalogItemId": "c2", "appName": "UnrealPlugin" },
                { "namespace": "x", "catalogItemId": "c3", "appName": "Dev", "sandboxType": "PRIVATE" },
                { "namespace": "y", "catalogItemId": "c4" },
            ],
            "responseMetadata": { "nextCursor": "abc" }
        }));
        assert_eq!(items, vec![("fn".into(), "c1".into(), "Fortnite".into())]);
        assert_eq!(next.as_deref(), Some("abc"));
        assert_eq!(parse_items(&json!({ "records": [], "responseMetadata": {} })).1, None);
    }

    #[test]
    fn catalog_items_drop_dlc_and_pick_art() {
        let game = json!({
            "title": " Celeste ",
            "categories": [{ "path": "games" }, { "path": "applications" }],
            "keyImages": [
                { "type": "DieselGameBox", "url": "wide" },
                { "type": "DieselGameBoxTall", "url": "tall" },
                { "type": "DieselGameBoxLogo", "url": "logo" }
            ]
        });
        assert_eq!(parse_catalog_item(&game), Some(("Celeste".into(), Some("tall".into()), Some("wide".into()), Some("logo".into()))));
        assert_eq!(parse_catalog_item(&json!({ "title": "Skin pack", "categories": [{ "path": "addons" }] })), None);
        let standalone = json!({ "title": "Standalone", "mainGameItem": { "id": "m" }, "categories": [{ "path": "addons" }, { "path": "addons/launchable" }] });
        assert_eq!(parse_catalog_item(&standalone).map(|g| g.0), Some("Standalone".into()));
        assert_eq!(parse_catalog_item(&json!({ "title": "Expansion", "mainGameItem": { "id": "m" } })), None);
        assert_eq!(parse_catalog_item(&json!({ "title": "  " })), None);
    }

    #[test]
    fn playtime_is_seconds_by_app_name() {
        let map = parse_playtime(&json!([{ "artifactId": "Fortnite", "totalTime": 90 }, { "artifactId": "Bad" }]));
        assert_eq!(map.get("Fortnite"), Some(&90_000));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn achievements_map_to_the_catalogue_and_unlocks() {
        let catalogue = parse_definitions(&json!({ "data": { "Achievement": { "productAchievementsRecordBySandbox": { "achievements": [
            { "achievement": { "name": "a1", "hidden": true, "unlockedDisplayName": "First", "unlockedDescription": "Do it", "unlockedIconLink": "on", "lockedIconLink": "off" } },
            { "achievement": { "name": "" } }
        ] } } } }));
        assert_eq!(catalogue, vec![json!({ "name": "a1", "displayName": "First", "description": "Do it", "icon": "on", "icongray": "off", "hidden": true })]);

        let unlocks = parse_unlocks(&json!({ "data": { "PlayerAchievement": { "playerAchievementGameRecordsBySandbox": { "records": [
            { "playerAchievements": [
                { "playerAchievement": { "achievementName": "a1", "unlocked": true, "unlockDate": "2026-01-02T03:04:05.000Z" } },
                { "playerAchievement": { "achievementName": "a2", "unlocked": false, "unlockDate": "2026-01-02T03:04:05.000Z" } }
            ] }
        ] } } } }));
        assert_eq!(unlocks.len(), 1);
        assert_eq!(unlocks[0].name, "a1");
    }

    #[test]
    fn records_share_the_launcher_import_key() {
        assert_eq!(object_id("Fortnite"), launcher_object_id("epic", "Fortnite"));
        assert_eq!(key_of("Fortnite"), game_key("custom", "epic-Fortnite"));
    }

    #[test]
    fn login_url_wraps_the_redirect() {
        assert!(login_url().starts_with("https://www.epicgames.com/id/login?redirectUrl=https%3A%2F%2Fwww.epicgames.com%2Fid%2Fapi%2Fredirect"));
        assert!(is_redirect_page(&"https://www.epicgames.com/id/api/redirect?clientId=x".parse().unwrap()));
        assert!(!is_redirect_page(&"https://www.epicgames.com/id/login".parse().unwrap()));
    }
}
