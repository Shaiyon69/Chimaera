//! Steam account link and library sync, ported from src/main/services/steam-integration/
//! (steam-store-session.ts, steam-web-api.ts, steam-sync-orchestrator.ts, steam-family-library.ts,
//! steam-source-payload.ts, steam-source-retry.ts, steam-community-achievements.ts,
//! link-imported-steam-executables.ts) and events/user/*steam*.ts.
//!
//! The Steam store login runs in a webview with its own data folder, standing in for Electron's
//! `persist:steam` partition. Each sync opens that profile in a hidden webview (which also lets
//! Steam refresh its login), reads the cookies, and makes the HTTP calls with them.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::achievement_files::Unlock;
use crate::games::{game_key, update, with_executable_path};
use crate::library::{iso_from_ms, iso_now, now_ms};
use crate::store::Store;

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const LOGIN_URL: &str = "https://store.steampowered.com/login/?redir=explore%2F";
const EXPLORE_URL: &str = "https://store.steampowered.com/explore/";
const STORE_URL: &str = "https://store.steampowered.com/";
const COMMUNITY: &str = "https://steamcommunity.com";
const API: &str = "https://api.steampowered.com";
const ACCOUNT_KEY: &str = "steamAccount";
const LOGIN_LABEL: &str = "steam-login";
const SESSION_LABEL: &str = "steam-session";
const WEBVIEW_DIR: &str = "steam-webview";
const ACHIEVEMENT_CONCURRENCY: usize = 8;
/// Launching shouldn't open a Steam session every time; a recent sync is fresh enough.
const STARTUP_MIN_AGE_MS: u64 = 6 * 60 * 60 * 1000;

const SESSION_REQUIRED: &str = "steam-session-required";
const ACCOUNT_MISMATCH: &str = "steam-account-mismatch";
const PROFILE_PRIVATE: &str = "steam-profile-private";
const RATE_LIMITED: &str = "profile/steam-rate-limited";
const ABORTED: &str = "steam-sync-aborted";

// ---- Errors (steam-sync-errors.ts, steam-source-retry.ts) ----

#[derive(Debug)]
enum Error {
    Http { status: u16, retry_after: Option<u64>, body: String },
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

    /// The message the renderer maps to a toast (getSteamSyncFailureMessage).
    fn message(&self) -> String {
        match self {
            Error::Message(m) => m.to_string(),
            Error::Http { status: 429, .. } => RATE_LIMITED.into(),
            Error::Http { status, body, .. } => format!("steam-web-api-http-{status}{}", if body.is_empty() { String::new() } else { format!(", {}", body.chars().take(200).collect::<String>()) }),
            Error::Transport(e) => e.clone(),
        }
    }

    fn is_abort(&self) -> bool {
        matches!(self, Error::Message(ABORTED))
    }

    /// isSteamSourceAchievementSkippable: one game's achievements fail, the sync goes on.
    fn achievement_skippable(&self) -> bool {
        matches!(self.status(), Some(400 | 403 | 409 | 429 | 502)) || matches!(self, Error::Transport(_))
    }
}

/// shouldRetrySteamSource and getSteamSourceRetryDelayMs: the delay before attempt `failed + 1`, if any.
fn retry_delay_ms(error: &Error, failed: u32) -> Option<u64> {
    let exponent = failed.saturating_sub(1);
    match error {
        Error::Http { status: 429, retry_after, .. } if failed < 5 => Some(retry_after.map_or_else(|| (1000u64 << exponent).min(30_000), |s| s * 1000)),
        Error::Http { status: 502, .. } if failed < 3 => Some(500u64 << exponent),
        _ => None,
    }
}

/// Sleeps in short steps so a cancel lands quickly.
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

// ---- Session (steam-store-session.ts) ----

#[derive(Clone, Default)]
struct Session {
    steam_id: String,
    token: String,
    community_cookies: String,
    has_community_login: bool,
    timezone_offset: i64,
}

fn webview_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join(WEBVIEW_DIR))
}

fn client() -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| Error::Transport(e.to_string()))
}

fn cookie_header(cookies: &[tauri::webview::Cookie<'static>]) -> String {
    cookies.iter().filter(|c| !c.value().is_empty()).map(|c| format!("{}={}", c.name(), c.value())).collect::<Vec<_>>().join("; ")
}

fn has_login_cookie(cookies: &[tauri::webview::Cookie<'static>]) -> bool {
    cookies.iter().any(|c| c.name() == "steamLoginSecure" && !c.value().is_empty())
}

/// parseSteamTimezoneOffsetSeconds: Steam's `timezoneOffset` cookie is "<seconds>,0".
fn timezone_offset(value: Option<&str>) -> i64 {
    let value = value.unwrap_or_default();
    let end = value.char_indices().find(|&(i, c)| !(c.is_ascii_digit() || (i == 0 && c == '-'))).map_or(value.len(), |(i, _)| i);
    value[..end].parse().unwrap_or(0)
}

/// isSteamStoreLoggedInUrl: the login page sends a signed-in user on to `redir`.
fn is_logged_in_url(url: &url::Url) -> bool {
    url.host_str() == Some("store.steampowered.com") && !url.path().starts_with("/login")
}

/// Opens the Steam profile in a hidden webview and reads its cookies (readSteamCommunitySession
/// plus getSteamWebApiToken's window). Loading the store page lets Steam refresh an old login.
async fn read_cookies(app: &AppHandle, cancel: &AtomicBool) -> Result<(String, Vec<tauri::webview::Cookie<'static>>, Vec<tauri::webview::Cookie<'static>>), Error> {
    if let Some(old) = app.get_webview_window(SESSION_LABEL) {
        let _ = old.destroy();
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<url::Url>();
    let dir = webview_dir(app).map_err(Error::Transport)?;
    let window = WebviewWindowBuilder::new(app, SESSION_LABEL, WebviewUrl::External(EXPLORE_URL.parse().expect("static url")))
        .visible(false)
        .user_agent(USER_AGENT)
        .data_directory(dir)
        .on_page_load(move |_, payload| {
            if payload.event() == PageLoadEvent::Finished {
                let _ = tx.send(payload.url().clone());
            }
        })
        .build()
        .map_err(|e| Error::Transport(format!("steam webview: {e}")))?;
    let result = async {
        // Steam may bounce through login.steampowered.com first; wait for the store or the login page.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let mut final_url = EXPLORE_URL.to_string();
        loop {
            check(cancel)?;
            match tokio::time::timeout_at(deadline.min(tokio::time::Instant::now() + Duration::from_millis(250)), rx.recv()).await {
                Ok(Some(url)) if url.host_str() == Some("store.steampowered.com") => {
                    final_url = url.to_string();
                    break;
                }
                Ok(Some(_)) | Err(_) if tokio::time::Instant::now() < deadline => {}
                _ => break,
            }
        }
        let read = |url: &str| window.cookies_for_url(url.parse().expect("static url")).map_err(|e| Error::Transport(format!("steam cookies: {e}")));
        Ok((final_url, read(STORE_URL)?, read(COMMUNITY)?))
    }
    .await;
    let _ = window.destroy();
    result
}

/// getSteamWebApiToken + readSteamCommunitySession in one webview pass.
async fn open_session(app: &AppHandle, cancel: &AtomicBool) -> Result<Session, Error> {
    let (final_url, store_cookies, community_cookies) = read_cookies(app, cancel).await?;
    if final_url.contains("/login") || !has_login_cookie(&store_cookies) {
        return Err(Error::Message(SESSION_REQUIRED));
    }
    let response = client()?
        .get(EXPLORE_URL)
        .header("Cookie", cookie_header(&store_cookies))
        .header("Accept", "text/html,application/xhtml+xml")
        .send()
        .await
        .map_err(|e| Error::Transport(e.to_string()))?;
    let url = response.url().to_string();
    let html = response.text().await.map_err(|e| Error::Transport(e.to_string()))?;
    let (steam_id, token) = parse_store_config(&url, &html).ok_or(Error::Message(SESSION_REQUIRED))?;
    let timezone = community_cookies.iter().chain(&store_cookies).find(|c| c.name() == "timezoneOffset").map(|c| c.value().to_string());
    Ok(Session {
        steam_id,
        token,
        has_community_login: has_login_cookie(&community_cookies),
        community_cookies: cookie_header(&community_cookies),
        timezone_offset: timezone_offset(timezone.as_deref()),
    })
}

/// parseSteamStoreSessionConfig: (SteamID64, web API token) from `#application_config`.
fn parse_store_config(url: &str, html: &str) -> Option<(String, String)> {
    if url.contains("/login") {
        return None;
    }
    let at = html.find("id=\"application_config\"")?;
    let start = html[..at].rfind('<')?;
    let tag = &html[start..start + html[start..].find('>')?];
    let user: Value = serde_json::from_str(&decode_entities(&attribute(tag, "data-userinfo")?)).ok()?;
    let config: Value = serde_json::from_str(&decode_entities(&attribute(tag, "data-store_user_config")?)).ok()?;
    if user["logged_in"] != json!(true) {
        return None;
    }
    let steam_id = id_string(&user["steamid"])?;
    let token = config["webapi_token"].as_str().map(str::trim).filter(|t| !t.is_empty())?.to_string();
    (steam_id.len() == 17 && steam_id.bytes().all(|b| b.is_ascii_digit())).then_some((steam_id, token))
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let needle = format!(" {name}=\"");
    let from = tag.find(&needle)? + needle.len();
    Some(tag[from..from + tag[from..].find('"')?].to_string())
}

/// The HTML entities Steam's pages use in attributes and achievement text.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest[..rest.len().min(10)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "quot" => Some('"'),
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn id_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

// ---- Account (steam-account.ts, steam-account-core.ts) ----

fn account(store: &Store) -> Value {
    store.get("", ACCOUNT_KEY).unwrap_or(Value::Null)
}

/// toSteamIntegrationStatus.
pub fn integration_status(store: &Store) -> Value {
    status_of(account(store))
}

fn status_of(account: Value) -> Value {
    match account {
        Value::Object(mut fields) => {
            fields.insert("connected".into(), json!(true));
            fields.insert("snapshotPreserved".into(), json!(false));
            fields.insert("disconnectedAt".into(), Value::Null);
            fields.insert("latestSyncRun".into(), Value::Null);
            Value::Object(fields)
        }
        _ => json!({ "connected": false, "snapshotPreserved": false }),
    }
}

/// parseSteamProfileXml: a private profile still exposes its name and avatar.
fn profile_field(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let from = xml.find(&open)? + open.len();
    let body = &xml[from..from + xml[from..].find(&format!("</{tag}>"))?];
    let body = body.trim();
    let body = body.strip_prefix("<![CDATA[").and_then(|b| b.strip_suffix("]]>")).unwrap_or(body);
    Some(body.trim().to_string()).filter(|b| !b.is_empty())
}

async fn save_connected_account(app: &AppHandle) -> Result<(), Error> {
    let session = open_session(app, &AtomicBool::new(false)).await?;
    let (username, avatar) = match community_get(&session, &format!("{COMMUNITY}/profiles/{}/?xml=1", session.steam_id)).await {
        Ok((_, xml)) => (profile_field(&xml, "steamID"), profile_field(&xml, "avatarFull")),
        // The name and avatar are cosmetic; the link itself must still succeed.
        Err(e) => {
            eprintln!("[steam] failed to read the Steam profile: {e:?}");
            (None, None)
        }
    };
    let store = app.state::<Store>();
    let existing = account(&store);
    store
        .put("", ACCOUNT_KEY, &json!({
            "steamId64": session.steam_id,
            "username": username.unwrap_or_else(|| session.steam_id.clone()),
            "avatarUrl": avatar,
            "connectedAt": existing["connectedAt"].as_str().map_or_else(iso_now, str::to_string),
            "lastSyncedAt": if existing["steamId64"] == json!(session.steam_id) { existing["lastSyncedAt"].clone() } else { Value::Null },
        }))
        .map_err(Error::Transport)?;
    app.state::<SteamSync>().clear_reconnect_required(app);
    Ok(())
}

/// WindowManager.redirect("settings?tab=integrations").
fn show_integrations(app: &AppHandle) {
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.unminimize();
        let _ = main.show();
        let _ = main.set_focus();
        let _ = main.emit("on-navigate", "/settings?tab=integrations");
    }
}

/// startSteamOAuth: the Steam store login; the account is saved once Steam sets its login cookie.
pub fn start_login(app: &AppHandle) -> Result<(), String> {
    if let Some(old) = app.get_webview_window(LOGIN_LABEL) {
        let _ = old.destroy();
    }
    let done = Arc::new(AtomicBool::new(false));
    let handle = app.clone();
    // Linux loads the login page below, once the cookie policy is set.
    let first_url = if cfg!(target_os = "linux") { "about:blank" } else { LOGIN_URL };
    let mut builder = WebviewWindowBuilder::new(app, LOGIN_LABEL, WebviewUrl::External(first_url.parse().expect("static url")))
        .title("Steam")
        .inner_size(600.0, 800.0)
        .user_agent(USER_AGENT)
        .data_directory(webview_dir(app)?)
        .on_page_load(move |webview, payload| {
            if payload.event() != PageLoadEvent::Finished || !is_logged_in_url(payload.url()) || done.load(Ordering::Relaxed) {
                return;
            }
            let (done, app) = (done.clone(), handle.clone());
            // Reading cookies deadlocks on the event thread on Windows, so it runs on the async runtime.
            tauri::async_runtime::spawn(async move {
                let Some(window) = app.get_webview_window(webview.label()) else { return };
                if !window.cookies_for_url(STORE_URL.parse().expect("static url")).is_ok_and(|c| has_login_cookie(&c)) || done.swap(true, Ordering::Relaxed) {
                    return;
                }
                eprintln!("[steam] login completed");
                let _ = window.hide();
                let result = save_connected_account(&app).await;
                let _ = window.destroy();
                match result {
                    Ok(()) => {
                        let _ = app.emit("on-steam-connected", ());
                    }
                    Err(e) => {
                        eprintln!("[steam] failed to connect Steam: {e:?}");
                        let _ = app.emit("on-steam-connect-error", "generic");
                    }
                }
                show_integrations(&app);
            });
        });
    if let Some(main) = app.get_webview_window("main") {
        builder = builder.parent(&main).map_err(|e| e.to_string())?;
    }
    let _window = builder.build().map_err(|e| e.to_string())?;
    // The store login sets the steamcommunity.com cookie cross-site, which sync needs and WebKitGTK blocks by default.
    #[cfg(target_os = "linux")]
    {
        use webkit2gtk::{CookieAcceptPolicy, CookieManagerExt, WebViewExt, WebsiteDataManagerExt};
        _window
            .with_webview(|webview| {
                if let Some(cookies) = webview.inner().website_data_manager().and_then(|manager| manager.cookie_manager()) {
                    cookies.set_accept_policy(CookieAcceptPolicy::Always);
                }
            })
            .map_err(|e| e.to_string())?;
        _window.navigate(LOGIN_URL.parse().expect("static url")).map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ---- Web API (steam-web-api.ts) ----

async fn api_get(session: &Session, path: &str, params: Vec<(&str, String)>, include_steam_id: bool) -> Result<Value, Error> {
    let mut url = url::Url::parse(API).expect("static url").join(path).expect("static path");
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("access_token", &session.token);
        if include_steam_id {
            query.append_pair("steamid", &session.steam_id);
        }
        query.append_pair("format", "json");
        for (key, value) in &params {
            query.append_pair(key, value);
        }
    }
    let response = client()?.get(url).send().await.map_err(|e| Error::Transport(e.to_string()))?;
    let status = response.status().as_u16();
    let retry_after = response.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok());
    let body = response.text().await.map_err(|e| Error::Transport(e.to_string()))?;
    if !(200..300).contains(&status) {
        return Err(Error::Http { status, retry_after, body });
    }
    Ok(serde_json::from_str(&body).unwrap_or(Value::String(body)))
}

/// withSteamSourceRetry: 429 and 502 are retried with backoff.
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
                eprintln!("[steam] {label} retry {failed} after HTTP {:?}, waiting {delay}ms", error.status());
                sleep(delay, cancel).await?;
            }
        }
    }
}

async fn community_get(session: &Session, url: &str) -> Result<(String, String), Error> {
    let response = client()?
        .get(url)
        .header("Cookie", &session.community_cookies)
        .header("Accept", "text/html,application/xhtml+xml")
        .send()
        .await
        .map_err(|e| Error::Transport(e.to_string()))?;
    let (status, final_url) = (response.status().as_u16(), response.url().to_string());
    let retry_after = response.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok());
    let body = response.text().await.map_err(|e| Error::Transport(e.to_string()))?;
    if !(200..300).contains(&status) {
        return Err(Error::Http { status, retry_after, body });
    }
    Ok((final_url, body))
}

// ---- Library payloads (steam-source-payload.ts, steam-family-library.ts) ----

#[derive(Clone, Debug, PartialEq)]
struct LibraryGame {
    app_id: String,
    name: String,
    play_seconds: u64,
    /// Unix milliseconds.
    last_played: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Playtime {
    seconds: u64,
    last_played: Option<u64>,
}

fn is_app_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 10 && !id.starts_with('0') && id.bytes().all(|b| b.is_ascii_digit())
}

/// The list at `key`, at `response.key`, or the payload itself when it is a list.
fn named_array<'a>(payload: &'a Value, key: &str) -> &'a [Value] {
    payload.as_array().or_else(|| payload[key].as_array()).or_else(|| payload["response"][key].as_array()).map_or(&[], Vec::as_slice)
}

fn non_negative(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite() && *f >= 0.0).map(|f| f.trunc() as u64),
        Value::String(s) => s.parse::<f64>().ok().filter(|f| f.is_finite() && *f >= 0.0).map(|f| f.trunc() as u64),
        _ => None,
    }
}

fn unix_ms(value: &Value) -> Option<u64> {
    non_negative(value).filter(|s| *s > 0).map(|s| s * 1000)
}

fn item_app_id(item: &Value) -> Option<String> {
    id_string(&item["appid"]).filter(|id| is_app_id(id))
}

/// parseSteamSourceLibrary over GetOwnedGames.
fn parse_owned_games(payload: &Value) -> Vec<LibraryGame> {
    named_array(payload, "games")
        .iter()
        .filter_map(|item| {
            Some(LibraryGame {
                app_id: item_app_id(item)?,
                name: item["name"].as_str().unwrap_or_default().to_string(),
                // Only real numbers count here, unlike the family payloads.
                play_seconds: (item["playtime_forever"].as_u64().unwrap_or(0) + item["playtime_disconnected"].as_u64().unwrap_or(0)) * 60,
                last_played: item["rtime_last_played"].as_u64().filter(|s| *s > 0).map(|s| s * 1000),
            })
        })
        .collect()
}

/// isSkippedSteamLibraryTitle: demos, betas and test clients.
fn is_skipped_title(name: &str) -> bool {
    let words: Vec<String> = name.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect();
    words.iter().enumerate().any(|(i, w)| {
        let next = words.get(i + 1).map(String::as_str);
        let glued = |prefix: &str| w.strip_prefix(prefix) == Some("test");
        matches!(w.as_str(), "demo" | "beta" | "ptb")
            || (["play", "public", "closed", "open", "technical"].contains(&w.as_str()) && next == Some("test"))
            || ["play", "public", "closed", "open", "technical"].iter().any(|p| glued(p))
            || (w == "test" && matches!(next, Some("server" | "client")))
            || w == "testserver"
            || w == "testclient"
    })
}

fn family_group_id(payload: &Value) -> Option<String> {
    let response = if payload["response"].is_object() { &payload["response"] } else { payload };
    if response["is_not_member_of_any_group"] == json!(true) {
        return None;
    }
    id_string(&response["family_groupid"]).filter(|id| id != "0")
}

/// parseSteamSharedLibraryApps: shareable games only.
fn parse_shared_apps(payload: &Value) -> Vec<LibraryGame> {
    named_array(payload, "apps")
        .iter()
        .filter(|item| non_negative(&item["exclude_reason"]).is_none_or(|r| r == 0) && non_negative(&item["app_type"]).is_none_or(|t| t == 1))
        .filter_map(|item| {
            let name = item["name"].as_str().map(str::trim).filter(|n| !n.is_empty())?;
            Some(LibraryGame {
                app_id: item_app_id(item)?,
                name: name.to_string(),
                play_seconds: non_negative(&item["rt_playtime"]).unwrap_or(0) * 60,
                last_played: unix_ms(&item["rt_last_played"]),
            })
        })
        .collect()
}

/// pickPreferredPlaytime: more playtime wins; a missing last-played date is filled in.
fn prefer(current: Option<Playtime>, next: Playtime) -> Playtime {
    match current {
        None => next,
        Some(current) if next.seconds > current.seconds => Playtime { seconds: next.seconds, last_played: next.last_played.or(current.last_played) },
        Some(current) if current.last_played.is_none() && next.last_played.is_some() => Playtime { last_played: next.last_played, ..current },
        Some(current) => current,
    }
}

fn add_playtime(map: &mut HashMap<String, Playtime>, id: String, playtime: Playtime) {
    let merged = prefer(map.get(&id).copied(), playtime);
    map.insert(id, merged);
}

/// parseSteamFamilyPlaytimeByAppId: entries of other family members are skipped.
fn parse_family_playtime(payload: &Value, steam_id: &str) -> HashMap<String, Playtime> {
    let mut map = HashMap::new();
    for item in named_array(payload, "entries").iter().chain(named_array(payload, "entries_by_owner")) {
        let owner = item.get("steamid").or_else(|| item.get("steam_id")).and_then(Value::as_str).filter(|s| !s.is_empty());
        if owner.is_some_and(|o| o != steam_id) {
            continue;
        }
        let Some(id) = item_app_id(item) else { continue };
        let seconds = non_negative(&item["seconds"]).or_else(|| non_negative(&item["seconds_played"])).or_else(|| non_negative(&item["playtime_forever"]).map(|m| m * 60)).unwrap_or(0);
        add_playtime(&mut map, id, Playtime { seconds, last_played: unix_ms(&item["latest_played"]).or_else(|| unix_ms(&item["first_played"])) });
    }
    map
}

/// parseSteamLastPlayedTimes over ClientGetLastPlayedTimes.
fn parse_last_played(payload: &Value) -> HashMap<String, Playtime> {
    let mut map = HashMap::new();
    for item in named_array(payload, "games") {
        let Some(id) = item_app_id(item) else { continue };
        let seconds = (non_negative(&item["playtime_forever"]).unwrap_or(0) + non_negative(&item["playtime_disconnected"]).unwrap_or(0)) * 60;
        add_playtime(&mut map, id, Playtime { seconds, last_played: unix_ms(&item["last_playtime"]).or_else(|| unix_ms(&item["first_playtime"])) });
    }
    map
}

/// mergeSteamOwnedAndFamilyGames: owned games first, then family games not owned.
fn merge_owned_and_family(owned: Vec<LibraryGame>, family: Vec<LibraryGame>, playtime: &HashMap<String, Playtime>) -> Vec<LibraryGame> {
    let mut seen = HashSet::new();
    let mut games = Vec::new();
    for game in owned.into_iter().chain(family) {
        if !seen.insert(game.app_id.clone()) {
            continue;
        }
        let own = Playtime { seconds: game.play_seconds, last_played: game.last_played };
        let merged = playtime.get(&game.app_id).map_or(own, |extra| prefer(Some(own), *extra));
        games.push(LibraryGame { play_seconds: merged.seconds, last_played: merged.last_played, ..game });
    }
    games
}

// ---- Achievements (steam-community-achievements.ts) ----

struct SchemaItem {
    internal_name: String,
    name: String,
    description: String,
    icon_hash: Option<String>,
}

struct HtmlUnlock {
    name: String,
    description: String,
    icon_hash: Option<String>,
    unlock_ms: i64,
}

/// iconHashFromUrl: the hex file name of an achievement icon.
fn icon_hash(url: &str) -> Option<String> {
    let file = url.split(['?', '#']).next()?.rsplit('/').next()?;
    let (stem, ext) = file.rsplit_once('.')?;
    (stem.len() >= 16 && stem.bytes().all(|b| b.is_ascii_hexdigit()) && !ext.is_empty() && ext.bytes().all(|b| b.is_ascii_alphabetic())).then(|| stem.to_lowercase())
}

/// parseSteamGameAchievementSchema over IPlayerService/GetGameAchievements.
fn parse_schema(payload: &Value) -> Vec<SchemaItem> {
    let root = if payload["response"].is_object() { &payload["response"] } else { payload };
    root["achievements"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(|item| {
            let text = |key: &str| id_string(&item[key]).unwrap_or_default();
            let internal_name = text("internal_name");
            let icon = Some(text("icon")).filter(|i| !i.is_empty()).unwrap_or_else(|| text("icon_gray"));
            (!internal_name.is_empty()).then(|| SchemaItem { internal_name, name: text("localized_name"), description: text("localized_desc"), icon_hash: icon_hash(&icon) })
        })
        .collect()
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's days_from_civil).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

/// parseSteamUnlockTimeText: "Unlocked 12 Mar, 2023 @ 4:05pm" or "Unlocked Mar 12 @ 4:05pm" (no
/// year means this year, or last year when that lands in the future), shown in the profile's timezone.
fn parse_unlock_time(text: &str, timezone_offset: i64, now_ms: i64) -> Option<i64> {
    let lower = text.to_ascii_lowercase();
    let rest = &lower[lower.find("unlocked")? + "unlocked".len()..];
    let tokens: Vec<&str> = rest.split(|c: char| c.is_whitespace() || c == ',').filter(|t| !t.is_empty()).collect();
    let month_of = |t: &str| MONTHS.iter().position(|m| t.len() >= 3 && t.bytes().all(|b| b.is_ascii_alphabetic()) && t[..3] == **m);
    let (day, month, mut i) = match (tokens.first(), tokens.get(1)) {
        (Some(a), Some(b)) if a.parse::<i64>().is_ok() && month_of(b).is_some() => (a.parse().ok()?, month_of(b)?, 2),
        (Some(a), Some(b)) if month_of(a).is_some() && b.parse::<i64>().is_ok() => (b.parse().ok()?, month_of(a)?, 2),
        _ => return None,
    };
    let year = tokens.get(i).and_then(|t| (t.len() == 4).then(|| t.parse::<i64>().ok()).flatten());
    if year.is_some() {
        i += 1;
    }
    if tokens.get(i) != Some(&"@") {
        return None;
    }
    let mut time = tokens.get(i + 1)?.to_string();
    if let Some(meridiem) = tokens.get(i + 2).filter(|t| **t == "am" || **t == "pm") {
        time.push_str(meridiem);
    }
    let (clock, pm) = match (time.strip_suffix("pm"), time.strip_suffix("am")) {
        (Some(c), _) => (c, true),
        (_, Some(c)) => (c, false),
        _ => return None,
    };
    let (hour, minute) = clock.split_once(':')?;
    let (mut hour, minute): (i64, i64) = (hour.parse().ok()?, minute.parse().ok()?);
    if pm && hour < 12 {
        hour += 12;
    }
    if !pm && hour == 12 {
        hour = 0;
    }
    let at = |year: i64| (days_from_civil(year, month as i64 + 1, day) * 86_400 + hour * 3600 + minute * 60 - timezone_offset) * 1000;
    let year = year.unwrap_or_else(|| {
        let new_year = |y: i64| days_from_civil(y, 1, 1) * 86_400_000;
        let mut this_year = 1970 + now_ms.div_euclid(86_400_000 * 365);
        while new_year(this_year) > now_ms {
            this_year -= 1;
        }
        while new_year(this_year + 1) <= now_ms {
            this_year += 1;
        }
        if at(this_year) > now_ms + 86_400_000 { this_year - 1 } else { this_year }
    });
    Some(at(year))
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let lower = text.to_ascii_lowercase();
    let from = lower.find(&open.to_ascii_lowercase())? + open.len();
    let to = from + lower[from..].find(&close.to_ascii_lowercase())?;
    Some(&text[from..to])
}

/// parseSteamCommunityAchievementHtml: each unlocked `achieveRow` on the stats page.
fn parse_achievement_html(html: &str, timezone_offset: i64, now_ms: i64) -> Vec<HtmlUnlock> {
    let mut unlocks = Vec::new();
    let mut search = 0;
    while let Some(found) = html[search..].find("achieveRow") {
        let at = search + found;
        search = at + "achieveRow".len();
        let Some(start) = html[..at].rfind("<div") else { continue };
        // Only a div whose own tag carries the class starts a row.
        if html[start..at].contains('>') {
            continue;
        }
        let end = html[search..].find("achieveRow").map_or(html.len(), |n| search + n);
        let window = &html[start..end.min(start + 5000)];
        let name = decode_entities(between(window, "<h3 class=\"ellipsis\">", "</h3>").unwrap_or_default()).trim().to_string();
        let Some(unlock_text) = between(window, "<div class=\"achieveUnlockTime\">", "</div>") else { continue };
        let unlock_text = unlock_text.replace("<br>", " ").replace("<br/>", " ").replace("<br />", " ");
        let Some(unlock_ms) = parse_unlock_time(&unlock_text, timezone_offset, now_ms).filter(|_| !name.is_empty()) else { continue };
        let image = window.find("<img").and_then(|i| between(&window[i..], "src=\"", "\"")).unwrap_or_default();
        unlocks.push(HtmlUnlock {
            name,
            description: decode_entities(between(window, "<h5>", "</h5>").unwrap_or_default()).trim().to_string(),
            icon_hash: icon_hash(image),
            unlock_ms,
        });
    }
    unlocks
}

/// mapCommunityHtmlToPlayerstats: page rows matched to API names by icon, then name, then description.
fn map_html_unlocks(unlocks: &[HtmlUnlock], schema: &[SchemaItem]) -> Vec<Unlock> {
    let mut used = HashSet::new();
    unlocks
        .iter()
        .filter_map(|unlock| {
            let free = |item: &&SchemaItem| !used.contains(&item.internal_name);
            let found = unlock
                .icon_hash
                .as_ref()
                .and_then(|hash| schema.iter().filter(free).find(|i| i.icon_hash.as_ref() == Some(hash)))
                .or_else(|| schema.iter().filter(free).find(|i| i.name == unlock.name))
                .or_else(|| (!unlock.description.is_empty()).then(|| schema.iter().filter(free).find(|i| i.description == unlock.description)).flatten())?;
            used.insert(found.internal_name.clone());
            // Steam reports whole seconds.
            Some(Unlock { name: found.internal_name.clone(), unlock_time: unlock.unlock_ms / 1000 * 1000 })
        })
        .collect()
}

/// parseSteamCommunityAchievementsXml: closed (unlocked) achievements of the `?xml=1` stats page.
fn parse_achievement_xml(xml: &str) -> Vec<Unlock> {
    let lower = xml.to_ascii_lowercase();
    let starts: Vec<usize> = lower.match_indices("<achievement").filter(|(i, _)| !lower[i + 12..].starts_with('s')).map(|(i, _)| i).collect();
    starts
        .iter()
        .enumerate()
        .filter_map(|(n, &start)| {
            let block = &xml[start..starts.get(n + 1).copied().unwrap_or(xml.len())];
            let tag = &block[..block.find('>').unwrap_or(block.len())].to_ascii_lowercase().replace(' ', "");
            let closed = tag.contains("closed=\"1\"") || tag.contains("closed='1'");
            let name = decode_entities(between(block, "<apiname>", "</apiname>")?.trim().trim_start_matches("<![CDATA[").trim_end_matches("]]>")).trim().to_string();
            let unlock = between(block, "<unlockTimestamp>", "</unlockTimestamp>").and_then(|t| t.trim().parse::<i64>().ok()).unwrap_or(0);
            (closed && !name.is_empty() && unlock > 0).then(|| Unlock { name, unlock_time: unlock * 1000 })
        })
        .collect()
}

/// Steam's React achievements page embeds its query cache as escaped JSON: the schema pairs
/// `internal_key` with `api_name`, the user's data pairs it with `unlocked` and `unlock_time`.
// shortcut: reads those pairs as text and relies on Steam's field order, parse the embedded JSON if the order changes.
fn parse_achievement_ssr(html: &str) -> Vec<Unlock> {
    let text = html.replace("\\\"", "\"");
    let mut names = HashMap::new();
    let mut times = Vec::new();
    for entry in text.split("\"internal_key\":").skip(1) {
        let Some((key, rest)) = entry.split_once(',') else { continue };
        if let Some(name) = rest.strip_prefix("\"api_name\":\"").and_then(|r| r.split('"').next()) {
            names.insert(key, name);
        } else if let Some(time) = rest.strip_prefix("\"unlocked\":true,\"unlock_time\":") {
            let digits = time.find(|c: char| !c.is_ascii_digit()).map_or(time, |end| &time[..end]);
            times.extend(digits.parse::<i64>().ok().filter(|t| *t > 0).map(|t| (key, t)));
        }
    }
    times.into_iter().filter_map(|(key, time)| Some(Unlock { name: names.get(key)?.to_string(), unlock_time: time * 1000 })).collect()
}

fn is_login_page(url: &str, body: &str) -> bool {
    let url_lower = url.to_ascii_lowercase();
    if url_lower.contains("login.steampowered.com") || url_lower.split(['/', '.', '?', '#']).any(|p| p == "login") {
        return true;
    }
    let lower = body.to_ascii_lowercase();
    if lower.contains("<playerstats") || body.contains("achieveRow") {
        return false;
    }
    lower.contains("login.steampowered.com") && lower.contains("sign in")
}

async fn community_page(session: &Session, url: &str) -> Result<(String, String), Error> {
    let (final_url, body) = community_get(session, url).await?;
    if is_login_page(&final_url, &body) {
        return Err(Error::Message(SESSION_REQUIRED));
    }
    Ok((final_url, body))
}

/// The unlocks listed on a stats page, mapped to API names through the game's schema.
async fn mapped_page_unlocks(session: &Session, app_id: &str, html: &str) -> Result<Option<Vec<Unlock>>, Error> {
    let unlocks = parse_achievement_html(html, session.timezone_offset, now_ms() as i64);
    if unlocks.is_empty() {
        return Ok(None);
    }
    let schema = api_get(session, "IPlayerService/GetGameAchievements/v1/", vec![("appid", app_id.into()), ("language", "english".into())], false).await?;
    Ok(Some(map_html_unlocks(&unlocks, &parse_schema(&schema))))
}

/// fetchSteamCommunityPlayerAchievements: the profile's stats page, then the owner's own page,
/// then the XML stats.
async fn community_unlocks(session: &Session, app_id: &str) -> Result<Vec<Unlock>, Error> {
    let profile = community_page(session, &format!("{COMMUNITY}/profiles/{}/stats/{app_id}/achievements?l=english", session.steam_id)).await?.1;
    if let Some(unlocks) = mapped_page_unlocks(session, app_id, &profile).await? {
        return Ok(unlocks);
    }
    let owner = community_page(session, &format!("{COMMUNITY}/my/stats/{app_id}")).await?.1;
    if let Some(unlocks) = mapped_page_unlocks(session, app_id, &owner).await? {
        return Ok(unlocks);
    }
    if profile.contains("achieveRow") || owner.contains("achieveRow") {
        return Ok(Vec::new());
    }
    // Some games no longer serve the XML stats below, only this page.
    let embedded = parse_achievement_ssr(&profile);
    if !embedded.is_empty() {
        return Ok(embedded);
    }
    let (url, mut xml) = community_page(session, &format!("{COMMUNITY}/profiles/{}/stats/{app_id}/?xml=1", session.steam_id)).await?;
    if !xml.to_ascii_lowercase().contains("<playerstats") {
        let mut page = url::Url::parse(&url).map_err(|e| Error::Transport(e.to_string()))?;
        page.set_query(Some("xml=1"));
        page.set_fragment(None);
        if !page.path().ends_with('/') {
            let path = format!("{}/", page.path());
            page.set_path(&path);
        }
        xml = community_page(session, page.as_str()).await?.1;
    }
    let lower = xml.to_ascii_lowercase();
    let private = lower.replace(char::is_whitespace, "").contains("<privacystate>private</privacystate>") && !lower.contains("<achievement");
    Ok(if lower.contains("<playerstats") && !private { parse_achievement_xml(&xml) } else { Vec::new() })
}

// ---- Library merge (library-sync/merge-with-remote-games.ts for a Steam source) ----

const STEAM_ASSETS: &str = "https://shared.steamstatic.com/store_item_assets/steam/apps";

fn later_iso(local: &Value, remote: Option<u64>) -> Value {
    let local_ms = local.as_str().and_then(parse_iso_ms);
    match (local_ms, remote) {
        (_, None) => local.clone(),
        (None, Some(r)) if local.is_null() => json!(iso_from_ms(r)),
        (Some(l), Some(r)) if r > l => json!(iso_from_ms(r)),
        _ => local.clone(),
    }
}

/// The millisecond time of an ISO-8601 UTC date like 2024-05-01T10:20:30.000Z.
pub(crate) fn parse_iso_ms(iso: &str) -> Option<u64> {
    let (date, time) = iso.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>());
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let time = time.trim_end_matches('Z');
    let mut t = time.split(':');
    let (h, min) = (t.next()?.parse::<i64>().ok()?, t.next()?.parse::<i64>().ok()?);
    let sec: f64 = t.next().unwrap_or("0").parse().ok()?;
    let ms = (days_from_civil(y, m, day) * 86_400 + h * 3600 + min * 60) * 1000 + (sec * 1000.0) as i64;
    u64::try_from(ms).ok()
}

/// Real library art URLs from the store. Newer apps keep their art under hashed folders,
/// so the fixed `<id>/library_600x900_2x.jpg` path 404s for them.
#[derive(Debug, Default, PartialEq)]
struct StoreArt {
    cover: Option<String>,
    hero: Option<String>,
}

fn parse_store_art(payload: &Value) -> HashMap<String, StoreArt> {
    let mut out = HashMap::new();
    for item in named_array(payload, "store_items") {
        let (Some(id), Some(format)) = (item["appid"].as_u64(), item["assets"]["asset_url_format"].as_str()) else { continue };
        let url = |fields: &[&str]| {
            fields.iter().find_map(|f| item["assets"][*f].as_str()).map(|file| format!("https://shared.steamstatic.com/store_item_assets/{}", format.replace("${FILENAME}", file)))
        };
        out.insert(id.to_string(), StoreArt { cover: url(&["library_capsule_2x", "library_capsule"]), hero: url(&["library_hero_2x", "library_hero", "header_2x", "header"]) });
    }
    out
}

/// Best effort: a failed batch keeps the cached or fixed-path art. The endpoint is public,
/// so it runs without a Steam session (an empty token is accepted).
async fn fetch_store_art(cancel: &AtomicBool, app_ids: &[String]) -> Result<HashMap<String, StoreArt>, Error> {
    let session = Session::default();
    let session = &session;
    let mut art = HashMap::new();
    for chunk in app_ids.chunks(50) {
        let ids: Vec<Value> = chunk.iter().filter_map(|id| id.parse::<u64>().ok()).map(|id| json!({ "appid": id })).collect();
        let input = json!({ "ids": ids, "context": { "language": "english", "country_code": "US" }, "data_request": { "include_assets": true } }).to_string();
        // Network errors are retried too: a lost batch leaves its games on fixed paths that 404.
        for attempt in 1..=3u64 {
            match with_retry("store art", cancel, || api_get(session, "IStoreBrowseService/GetItems/v1/", vec![("input_json", input.clone())], false)).await {
                Ok(payload) => {
                    art.extend(parse_store_art(&payload));
                    break;
                }
                Err(e) if e.is_abort() => return Err(e),
                Err(e) => {
                    eprintln!("[steam] store art attempt {attempt} failed: {}", e.message());
                    if attempt < 3 {
                        sleep(attempt * 2000, cancel).await?;
                    }
                }
            }
        }
    }
    Ok(art)
}

/// mergeProfileGamesIntoLibrary for Steam's library: playtime goes to `steamPlayTimeInMilliseconds`,
/// everything the user set locally stays. Returns how many games were new or revived.
fn merge_library(store: &Store, games: &[LibraryGame], art: &HashMap<String, StoreArt>) -> Result<usize, String> {
    let mut added = 0;
    for game in games {
        let key = game_key("steam", &game.app_id);
        let local = store.get("games", &key)?;
        let steam_ms = game.play_seconds * 1000;
        let found = art.get(&game.app_id);
        let cover = found.and_then(|a| a.cover.clone());
        let hero = found.and_then(|a| a.hero.clone());
        let fixed_cover = format!("{STEAM_ASSETS}/{}/library_600x900_2x.jpg", game.app_id);
        let fixed_hero = format!("{STEAM_ASSETS}/{}/library_hero.jpg", game.app_id);
        let merged = match local {
            Value::Object(mut fields) => {
                if fields.get("isDeleted").and_then(Value::as_bool) == Some(true) {
                    added += 1;
                }
                let last = later_iso(fields.get("lastTimePlayed").unwrap_or(&Value::Null), game.last_played);
                fields.insert("lastTimePlayed".into(), last);
                let hydra_ms = fields.get("playTimeInMilliseconds").and_then(Value::as_u64).unwrap_or(0);
                fields.insert("playTimeInMilliseconds".into(), json!(hydra_ms));
                fields.insert("steamPlayTimeInMilliseconds".into(), json!(steam_ms));
                let source = if fields.get("source").and_then(Value::as_str) == Some("hydra") { "hydra" } else { "steam" };
                fields.insert("source".into(), json!(source));
                fields.insert("hasActiveSteamImport".into(), json!(true));
                fields.insert("isDeleted".into(), json!(false));
                if !fields.contains_key("addedToLibraryAt") {
                    fields.insert("addedToLibraryAt".into(), Value::Null);
                }
                Value::Object(fields)
            }
            _ => {
                added += 1;
                json!({
                    "objectId": game.app_id,
                    "title": game.name,
                    "remoteId": null,
                    "shop": "steam",
                    "iconUrl": cover.clone().unwrap_or_else(|| fixed_cover.clone()),
                    "libraryHeroImageUrl": hero.clone().unwrap_or_else(|| fixed_hero.clone()),
                    "logoImageUrl": format!("{STEAM_ASSETS}/{}/logo.png", game.app_id),
                    "addedToLibraryAt": null,
                    "lastTimePlayed": game.last_played.map(iso_from_ms),
                    "playTimeInMilliseconds": 0,
                    "steamPlayTimeInMilliseconds": steam_ms,
                    "hasManuallyUpdatedPlaytime": false,
                    "isDeleted": false,
                    "favorite": false,
                    "isHiddenFromOthers": false,
                    "isConcealed": false,
                    "isPinned": false,
                    "collectionIds": [],
                    "achievementCount": 0,
                    "unlockedAchievementCount": 0,
                    "platform": null,
                    "source": "steam",
                    "hasActiveSteamImport": true,
                    "customIconUrl": null,
                    "customLogoImageUrl": null,
                    "customHeroImageUrl": null,
                    "customCoverImageUrl": null,
                })
            }
        };
        let title = merged["title"].as_str().filter(|t| !t.is_empty()).unwrap_or(&game.name).to_string();
        store.put("games", &key, &merged)?;
        // buildRemoteGameShopAssets: fields Steam does not send keep what the cache has.
        let mut assets = match store.get("gameShopAssets", &key)? {
            Value::Object(a) => a,
            _ => Default::default(),
        };
        for (field, value) in [("updatedAt", json!(now_ms())), ("shop", json!("steam")), ("objectId", json!(game.app_id)), ("title", json!(title))] {
            assets.insert(field.into(), value);
        }
        put_art(&mut assets, &game.app_id, found);
        store.put("gameShopAssets", &key, &Value::Object(assets))?;
    }
    Ok(added)
}

/// Store art wins; without it, art the cache already has stays and the fixed path only fills gaps.
fn put_art(assets: &mut serde_json::Map<String, Value>, app_id: &str, art: Option<&StoreArt>) {
    let cover = art.and_then(|a| a.cover.clone());
    let hero = art.and_then(|a| a.hero.clone());
    for (field, real, fixed) in [
        ("coverImageUrl", &cover, format!("{STEAM_ASSETS}/{app_id}/library_600x900_2x.jpg")),
        ("iconUrl", &cover, format!("{STEAM_ASSETS}/{app_id}/library_600x900_2x.jpg")),
        ("libraryHeroImageUrl", &hero, format!("{STEAM_ASSETS}/{app_id}/library_hero.jpg")),
        ("logoImageUrl", &None, format!("{STEAM_ASSETS}/{app_id}/logo.png")),
    ] {
        match real {
            Some(url) => _ = assets.insert(field.into(), json!(url)),
            None => _ = assets.entry(field).or_insert_with(|| json!(fixed)),
        }
    }
}

/// Steam games whose cached cover is still the fixed-path guess older syncs wrote.
fn guessed_art_ids(store: &Store) -> Result<Vec<String>, String> {
    Ok(store
        .entries("gameShopAssets")?
        .into_iter()
        .filter_map(|(key, assets)| {
            let id = key.strip_prefix("steam:")?.to_string();
            let guess = format!("{STEAM_ASSETS}/{id}/library_600x900_2x.jpg");
            (assets["coverImageUrl"].as_str() == Some(guess.as_str())).then_some(id)
        })
        .collect())
}

/// Replaces fixed-path art with store art without waiting for a full sync.
async fn repair_art(app: &AppHandle) {
    let store = app.state::<Store>();
    let ids = match guessed_art_ids(&store) {
        Ok(ids) if !ids.is_empty() => ids,
        _ => return,
    };
    let Ok(art) = fetch_store_art(&AtomicBool::new(false), &ids).await else { return };
    let mut fixed = 0;
    for (id, found) in &art {
        let key = game_key("steam", id);
        let Ok(Value::Object(mut assets)) = store.get("gameShopAssets", &key) else { continue };
        put_art(&mut assets, id, Some(found));
        fixed += usize::from(store.put("gameShopAssets", &key, &Value::Object(assets)).is_ok());
    }
    eprintln!("[steam] repaired art for {fixed} of {} games", ids.len());
    if fixed > 0 {
        let _ = app.emit("on-library-batch-complete", ());
    }
}

/// hideGamesRemovedFromSteamImport: an imported game Steam no longer lists is hidden.
fn hide_removed(store: &Store, keep: &HashSet<String>) -> Result<(), String> {
    for (key, game) in store.entries("games")? {
        if game["shop"] == "steam" && game["hasActiveSteamImport"] == json!(true) && !keep.contains(&key) {
            update(store, &key, |g| {
                g.insert("hasActiveSteamImport".into(), json!(false));
                g.insert("isDeleted".into(), json!(true));
            })?;
        }
    }
    Ok(())
}

fn is_link_candidate(game: &Value) -> bool {
    game["shop"] == "steam" && game["isDeleted"] != json!(true) && game["hasActiveSteamImport"] == json!(true) && game["executablePath"].as_str().is_none_or(str::is_empty)
}

/// linkImportedSteamGameExecutables: installed imported games get their executable from the catalogue.
/// shortcut: Steam's appinfo.vdf launch entries are not read (as in the scan); catalogue only.
async fn link_executables(app: &AppHandle, only: Option<&str>, on_progress: impl Fn(usize, usize)) -> usize {
    let store = app.state::<Store>();
    let libraries = crate::steam_shortcuts::steam_path().map(|s| crate::scan::library_folders(&s)).unwrap_or_default();
    let Ok(entries) = store.entries("games") else { return 0 };
    let mut imported: Vec<(String, Value)> = entries
        .into_iter()
        .filter(|(_, g)| g["shop"] == "steam" && g["isDeleted"] != json!(true) && g["hasActiveSteamImport"] == json!(true))
        .filter(|(_, g)| only.is_none_or(|id| g["objectId"] == json!(id)))
        .collect();
    if let Err(e) = crate::scan::clear_missing_executables(&store, &mut imported, &libraries) {
        eprintln!("[steam] failed to clear missing executables: {e}");
    }
    let installed: HashMap<String, std::path::PathBuf> = crate::scan::installed_apps(&libraries).into_iter().collect();
    let candidates: Vec<(String, std::path::PathBuf)> = imported
        .iter()
        .filter(|(_, g)| is_link_candidate(g))
        .filter_map(|(key, g)| Some((key.clone(), installed.get(g["objectId"].as_str()?)?.clone())))
        .collect();
    on_progress(0, candidates.len());
    if candidates.is_empty() {
        return 0;
    }
    let version = app.package_info().version.to_string();
    let Some(catalogue) = crate::scan::catalogue(&store, &version).await else { return 0 };
    let mut linked = 0;
    for (n, (key, folder)) in candidates.iter().enumerate() {
        let id = key.trim_start_matches("steam:");
        let pick = crate::scan::catalogue_executable(&catalogue, id, folder, &AtomicBool::new(false));
        if let Some(path) = pick {
            let result = update(&store, key, |g| {
                if is_link_candidate(&Value::Object(g.clone())) {
                    g.insert("executablePath".into(), json!(path));
                    g.insert("executablePathUpdatedAt".into(), json!(iso_now()));
                }
            });
            if result.is_ok_and(|g| g.is_some_and(|g| g["executablePath"] == json!(path))) {
                eprintln!("[steam] linked catalogue executable for {key}: {path}");
                linked += 1;
            }
        }
        on_progress(n + 1, candidates.len());
    }
    linked
}

// ---- Sync (steam-sync-orchestrator.ts) ----

#[derive(Default)]
pub struct SteamSync {
    state: Mutex<Value>,
    cancel: Mutex<Option<Arc<AtomicBool>>>,
    /// Held for the length of a run; a game-page sync waits on it.
    running: tokio::sync::Mutex<()>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl SteamSync {
    pub fn state(&self) -> Value {
        let state = lock(&self.state).clone();
        if state.is_null() { json!({ "status": "idle" }) } else { state }
    }

    fn set_state(&self, app: &AppHandle, state: Value) {
        *lock(&self.state) = state.clone();
        let _ = app.emit("on-steam-sync-progress", state);
    }

    fn progress(&self, app: &AppHandle, run_id: &str, phase: &str, found: usize, processed: usize) {
        if self.state()["status"] == "running" {
            self.set_state(app, json!({ "status": "running", "syncRunId": run_id, "phase": phase, "gamesFound": found, "gamesProcessed": processed }));
        }
    }

    fn clear_reconnect_required(&self, app: &AppHandle) {
        let state = self.state();
        if state["status"] == "idle" && state["requiresReconnect"] == json!(true) {
            self.set_state(app, json!({ "status": "idle" }));
        }
    }

    /// cancel: the run notices at its next step.
    pub async fn cancel(&self, app: &AppHandle) {
        let state = self.state();
        if state["status"] != "running" {
            return;
        }
        self.set_state(app, json!({ "status": "cancelling", "syncRunId": state["syncRunId"] }));
        if let Some(flag) = lock(&self.cancel).as_ref() {
            flag.store(true, Ordering::Relaxed);
        }
        drop(self.running.lock().await);
    }
}

/// start: runs in the background; a second start while one runs returns the current state.
pub fn start(app: &AppHandle, origin: &'static str) -> Value {
    let sync = app.state::<SteamSync>();
    let state = sync.state();
    if state["status"] == "running" || state["status"] == "cancelling" {
        return state;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    *lock(&sync.cancel) = Some(cancel.clone());
    sync.set_state(app, json!({ "status": "running", "syncRunId": "", "phase": "starting", "gamesFound": 0, "gamesProcessed": 0 }));
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let sync = app.state::<SteamSync>();
        let _running = sync.running.lock().await;
        let run_id = uuid::Uuid::new_v4().to_string();
        eprintln!("[steam] sync started");
        let result = run(&app, &run_id, &cancel).await;
        let finished = match result {
            Ok(status) => {
                eprintln!("[steam] sync succeeded");
                // Picks up any art batch the sync could not fetch.
                repair_art(&app).await;
                sync.set_state(&app, json!({ "status": "idle" }));
                json!({ "ok": true, "status": status, "origin": origin })
            }
            Err(error) => {
                let message = if error.is_abort() { ABORTED.to_string() } else { error.message() };
                eprintln!("[steam] sync failed: {message}");
                let reconnect = message == SESSION_REQUIRED || message == ACCOUNT_MISMATCH;
                sync.set_state(&app, if reconnect { json!({ "status": "idle", "requiresReconnect": true }) } else { json!({ "status": "idle" }) });
                json!({ "ok": false, "message": message, "origin": origin })
            }
        };
        *lock(&sync.cancel) = None;
        let _ = app.emit("on-steam-sync-finished", finished);
    });
    sync.state()
}

/// resolveSteamSession: the store login must belong to the linked account.
async fn linked_session(app: &AppHandle, cancel: &AtomicBool) -> Result<Session, Error> {
    let linked = account(&app.state::<Store>());
    let Some(linked_id) = linked["steamId64"].as_str().map(str::to_string) else { return Err(Error::Message(SESSION_REQUIRED)) };
    let session = open_session(app, cancel).await?;
    if session.steam_id != linked_id {
        eprintln!("[steam] store session {} does not match linked {linked_id}", session.steam_id);
        return Err(Error::Message(ACCOUNT_MISMATCH));
    }
    Ok(session)
}

/// fetchLibrary + mergeFamilyLibrary: owned games, the family library, and the best playtime of each.
async fn fetch_library(session: &Session, cancel: &AtomicBool) -> Result<Vec<LibraryGame>, Error> {
    let owned = with_retry("library", cancel, || {
        api_get(session, "IPlayerService/GetOwnedGames/v1/", vec![("include_appinfo", "1".into()), ("include_played_free_games", "1".into())], true)
    })
    .await
    .map_err(|e| match e.status() {
        Some(401) => Error::Message(SESSION_REQUIRED),
        Some(403) => Error::Message(PROFILE_PRIVATE),
        _ => e,
    })?;
    if owned.as_str().is_some_and(|m| m.contains(PROFILE_PRIVATE)) || owned["message"].as_str().is_some_and(|m| m.contains(PROFILE_PRIVATE)) {
        return Err(Error::Message(PROFILE_PRIVATE));
    }
    let owned = parse_owned_games(&owned);

    let mut playtime = match with_retry("last played times", cancel, || api_get(session, "IPlayerService/ClientGetLastPlayedTimes/v1/", Vec::new(), false)).await {
        Ok(payload) => parse_last_played(&payload),
        Err(e) if e.is_abort() => return Err(e),
        Err(e) => {
            eprintln!("[steam] last played times unavailable: {e:?}");
            HashMap::new()
        }
    };
    let mut family = Vec::new();
    match with_retry("family group", cancel, || api_get(session, "IFamilyGroupsService/GetFamilyGroupForUser/v1/", Vec::new(), false)).await.map(|p| family_group_id(&p)) {
        Ok(Some(group)) => {
            let shared = with_retry("family library", cancel, || {
                api_get(
                    session,
                    "IFamilyGroupsService/GetSharedLibraryApps/v1/",
                    vec![("family_groupid", group.clone()), ("include_own", "0".into()), ("include_excluded", "0".into()), ("include_free", "0".into()), ("include_non_games", "0".into()), ("language", "english".into())],
                    false,
                )
            })
            .await;
            match shared {
                Ok(payload) => {
                    family = parse_shared_apps(&payload);
                    for app in &family {
                        add_playtime(&mut playtime, app.app_id.clone(), Playtime { seconds: app.play_seconds, last_played: app.last_played });
                    }
                    let summary = with_retry("family playtime", cancel, || {
                        api_get(session, "IFamilyGroupsService/GetPlaytimeSummary/v1/", vec![("family_groupid", group.clone()), ("input_json", json!({ "family_groupid": group }).to_string())], false)
                    })
                    .await;
                    match summary {
                        Ok(payload) => {
                            for (id, p) in parse_family_playtime(&payload, &session.steam_id) {
                                add_playtime(&mut playtime, id, p);
                            }
                        }
                        Err(e) if e.is_abort() => return Err(e),
                        Err(e) => eprintln!("[steam] family playtime unavailable: {e:?}"),
                    }
                }
                Err(e) if e.is_abort() => return Err(e),
                Err(e) => eprintln!("[steam] family library unavailable: {e:?}"),
            }
        }
        Ok(None) => eprintln!("[steam] no family group"),
        Err(e) if e.is_abort() => return Err(e),
        Err(e) => eprintln!("[steam] family library unavailable: {e:?}"),
    }
    let merged = merge_owned_and_family(owned, family, &playtime);
    let games: Vec<LibraryGame> = merged.iter().filter(|g| !is_skipped_title(&g.name)).cloned().collect();
    eprintln!("[steam] library: {} games, {} skipped titles", games.len(), merged.len() - games.len());
    Ok(games)
}

/// fetchAchievements: community unlocks of every played game, 8 at a time, merged into the local store.
/// A first sync has no baseline, so it stores unlocks without announcing the whole history.
async fn sync_achievements(app: &AppHandle, session: &Session, games: &[LibraryGame], cancel: &Arc<AtomicBool>, on_progress: impl Fn(usize) + Send + Sync + 'static) -> Result<(), Error> {
    let queue: Arc<Mutex<std::collections::VecDeque<LibraryGame>>> = Arc::new(Mutex::new(games.iter().filter(|g| g.play_seconds > 0).cloned().collect()));
    let skipped = games.iter().filter(|g| g.play_seconds == 0).count();
    let processed = Arc::new(std::sync::atomic::AtomicUsize::new(skipped));
    let rate_limited = Arc::new(AtomicBool::new(false));
    let on_progress = Arc::new(on_progress);
    on_progress(skipped);
    let mut workers = Vec::new();
    for _ in 0..ACHIEVEMENT_CONCURRENCY {
        let (app, session, cancel, queue, processed, rate_limited, on_progress) = (app.clone(), session.clone(), cancel.clone(), queue.clone(), processed.clone(), rate_limited.clone(), on_progress.clone());
        workers.push(tauri::async_runtime::spawn(async move {
            loop {
                check(&cancel)?;
                let Some(game) = lock(&queue).pop_front() else { return Ok(()) };
                let result = if rate_limited.load(Ordering::Relaxed) {
                    Ok(None)
                } else {
                    match with_retry(&format!("achievements {}", game.app_id), &cancel, || community_unlocks(&session, &game.app_id)).await {
                        Ok(unlocks) => Ok(Some(unlocks)),
                        Err(e) if e.is_abort() || matches!(e, Error::Message(SESSION_REQUIRED)) || e.status() == Some(401) => Err(match e {
                            Error::Http { .. } => Error::Message(SESSION_REQUIRED),
                            other => other,
                        }),
                        Err(e) if e.achievement_skippable() => {
                            eprintln!("[steam] skipping achievements for {} {}: {e:?}", game.app_id, game.name);
                            if e.status() == Some(429) {
                                rate_limited.store(true, Ordering::Relaxed);
                            }
                            Ok(None)
                        }
                        Err(e) => Err(e),
                    }
                };
                if let Some(unlocks) = result? {
                    let key = game_key("steam", &game.app_id);
                    let baseline = !app.state::<Store>().get("unlockedAchievements", &key).unwrap_or(Value::Null).is_null();
                    match crate::achievements::merge(&app, "steam", &game.app_id, &unlocks, baseline, baseline).await {
                        Ok(n) if n > 0 => eprintln!("[steam] achievements for {} {}: {n} new of {} unlocked", game.app_id, game.name, unlocks.len()),
                        Ok(_) => {}
                        Err(e) => eprintln!("[steam] failed to store achievements for {}: {e}", game.app_id),
                    }
                }
                on_progress(processed.fetch_add(1, Ordering::Relaxed) + 1);
            }
        }));
    }
    let mut first_error = None;
    for worker in workers {
        let outcome = worker.await.map_err(|e| Error::Transport(e.to_string())).and_then(|r| r);
        if let Err(e) = outcome {
            // Stop the other workers at their next game.
            cancel.store(true, Ordering::Relaxed);
            if first_error.as_ref().is_none_or(|f: &Error| f.is_abort()) {
                first_error = Some(e);
            }
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    if rate_limited.load(Ordering::Relaxed) {
        return Err(Error::Message(RATE_LIMITED));
    }
    Ok(())
}

async fn run(app: &AppHandle, run_id: &str, cancel: &Arc<AtomicBool>) -> Result<Value, Error> {
    let sync = app.state::<SteamSync>();
    sync.progress(app, run_id, "library", 0, 0);
    let session = linked_session(app, cancel).await?;
    check(cancel)?;
    if !session.has_community_login {
        return Err(Error::Message(SESSION_REQUIRED));
    }
    let games = fetch_library(&session, cancel).await?;
    check(cancel)?;
    let ids: Vec<String> = games.iter().map(|g| g.app_id.clone()).collect();
    let art = fetch_store_art(cancel, &ids).await?;

    // Merged before achievements so their counts land on records that exist.
    sync.progress(app, run_id, "merging", games.len(), 0);
    {
        let store = app.state::<Store>();
        merge_library(&store, &games, &art).map_err(Error::Transport)?;
        // An empty library is far likelier a bad response than a cleared account.
        if !games.is_empty() {
            hide_removed(&store, &games.iter().map(|g| game_key("steam", &g.app_id)).collect()).map_err(Error::Transport)?;
        }
    }
    sync.progress(app, run_id, "merging", games.len(), games.len());
    let _ = app.emit("on-library-batch-complete", ());

    sync.progress(app, run_id, "achievements", games.len(), 0);
    let (progress_app, progress_id, total) = (app.clone(), run_id.to_string(), games.len());
    let cancel_flag = cancel.clone();
    sync_achievements(app, &session, &games, cancel, move |done| {
        if !cancel_flag.load(Ordering::Relaxed) {
            progress_app.state::<SteamSync>().progress(&progress_app, &progress_id, "achievements", total, done);
        }
    })
    .await?;
    check(cancel)?;

    // The startup backfill ran before these games joined the library; games with no new unlocks never fetched a catalogue.
    let backfill_app = app.clone();
    tauri::async_runtime::spawn(async move { crate::achievements::backfill_catalogues(&backfill_app).await });

    sync.progress(app, run_id, "executables", 0, 0);
    let linked = link_executables(app, None, |done, total| sync.progress(app, run_id, "executables", total, done)).await;
    eprintln!("[steam] linked {linked} executables");
    let _ = app.emit("on-library-batch-complete", ());

    sync.progress(app, run_id, "finishing", 0, 0);
    let store = app.state::<Store>();
    let mut linked_account = account(&store);
    if linked_account.is_object() {
        linked_account["lastSyncedAt"] = json!(iso_now());
        store.put("", ACCOUNT_KEY, &linked_account).map_err(Error::Transport)?;
    }
    Ok(status_of(linked_account))
}

/// startSteamSyncOnStartup: a linked account last synced over six hours ago syncs on launch.
pub fn start_on_launch(app: &AppHandle) {
    let last = account(&app.state::<Store>());
    if !last.is_object() {
        return;
    }
    let stale = last["lastSyncedAt"].as_str().and_then(parse_iso_ms).is_none_or(|at| now_ms().saturating_sub(at) > STARTUP_MIN_AGE_MS);
    if stale {
        start(app, "startup");
    } else {
        let app = app.clone();
        tauri::async_runtime::spawn(async move { repair_art(&app).await });
    }
}

/// syncSteamGameOnGamePage: refreshes the game's executable. The playtime and achievement
/// refresh needs a Hydra sign-in in Electron, which Chimaera does not have, so it answers false.
pub fn sync_game_page(app: &AppHandle, app_id: &str) -> bool {
    if !is_app_id(app_id) {
        return false;
    }
    let app = app.clone();
    let id = app_id.to_string();
    tauri::async_runtime::spawn(async move {
        if refresh_executable(&app, &id).await {
            let _ = app.emit("on-library-batch-complete", ());
        }
    });
    false
}

/// refreshSteamGameExecutable: clears a vanished executable, then links one if the app is installed.
async fn refresh_executable(app: &AppHandle, app_id: &str) -> bool {
    let key = game_key("steam", app_id);
    let before = app.state::<Store>().get("games", &key).unwrap_or(Value::Null);
    if before["hasActiveSteamImport"] != json!(true) || before["isDeleted"] == json!(true) {
        return false;
    }
    link_executables(app, Some(app_id), |_, _| {}).await;
    let after = app.state::<Store>().get("games", &key).unwrap_or(Value::Null);
    before["executablePath"] != after["executablePath"]
}

// ---- Install on Steam (install-game-on-steam.ts, steam-install-watcher.ts) ----

/// installGameOnSteam: opens Steam's install dialog, then checks the app's manifest after 3 s and 30 s.
/// shortcut: two timed checks instead of watching the steamapps folders; a slow install links on the next sync or game page visit.
pub fn install_on_steam(app: &AppHandle, app_id: &str) -> Result<(), String> {
    if !is_app_id(app_id) {
        return Ok(());
    }
    use tauri_plugin_opener::OpenerExt;
    app.opener().open_url(format!("steam://install/{app_id}"), None::<&str>).map_err(|e| e.to_string())?;
    let (app, id) = (app.clone(), app_id.to_string());
    tauri::async_runtime::spawn(async move {
        for delay in [3, 27] {
            tokio::time::sleep(Duration::from_secs(delay)).await;
            if refresh_executable(&app, &id).await {
                eprintln!("[steam] install state changed for {id}");
                let _ = app.emit("on-library-batch-complete", ());
            }
        }
    });
    Ok(())
}

// ---- Disconnect (disconnect-steam.ts, steam-imported-games.ts, clear-imported-steam-games.ts) ----

fn has_local_install(game: &Value) -> bool {
    game["executablePath"].as_str().is_some_and(|p| !p.is_empty())
        || game["installedSizeInBytes"].as_u64().is_some_and(|s| s > 0)
        || game["trackingExecutablePaths"].as_array().is_some_and(|p| !p.is_empty())
}

/// shouldRemoveImportedSteamGame, after the cleanup has run.
fn should_remove(game: &Value, steam_only: &HashSet<String>) -> bool {
    if game["shop"] != "steam" || game["isDeleted"] == json!(true) || game["playTimeInMilliseconds"].as_f64().unwrap_or(0.0) > 0.0 {
        return false;
    }
    if game["objectId"].as_str().is_some_and(|id| steam_only.contains(id)) {
        return true;
    }
    !has_local_install(game) && game["source"] == "steam"
}

/// The game records after a disconnect, before the account and cookies are dropped.
fn disconnect_games(store: &Store, delete_imported: bool) -> Result<(), String> {
    let games = store.entries("games")?;
    if !delete_imported {
        for (key, game) in &games {
            if game["hasActiveSteamImport"] == json!(true) {
                update(store, key, |g| {
                    g.insert("hasActiveSteamImport".into(), json!(false));
                })?;
            }
        }
        return Ok(());
    }
    let steam_only: HashSet<String> = games.iter().filter(|(_, g)| g["source"] == "steam").filter_map(|(_, g)| g["objectId"].as_str().map(str::to_string)).collect();
    for (key, game) in &games {
        let imported = game["shop"] == "steam"
            && (game["hasActiveSteamImport"] == json!(true)
                || game["steamPlayTimeInMilliseconds"].as_f64().unwrap_or(0.0) > 0.0
                || game["source"] == "steam"
                || game["objectId"].as_str().is_some_and(|id| steam_only.contains(id)));
        if !imported {
            continue;
        }
        let _ = store.del("unlockedAchievements", key);
        let updated = update(store, key, |g| {
            g.insert("steamPlayTimeInMilliseconds".into(), json!(0));
            if g.get("source").and_then(Value::as_str) == Some("steam") {
                g.insert("source".into(), json!("hydra"));
            }
        })?;
        let Some(game) = updated else { continue };
        if should_remove(&game, &steam_only) && store.get("downloads", key)?.is_null() {
            let mut removed = with_executable_path(game, None);
            removed["isDeleted"] = json!(true);
            removed["lastTimePlayed"] = Value::Null;
            for field in ["customIconUrl", "customLogoImageUrl", "customHeroImageUrl", "customCoverImageUrl"] {
                removed[field] = Value::Null;
            }
            store.put("games", key, &removed)?;
            store.del("artworkSelection", key)?;
        }
    }
    for (key, game) in store.entries("games")? {
        if game["hasActiveSteamImport"] == json!(true) {
            update(store, &key, |g| {
                g.insert("hasActiveSteamImport".into(), json!(false));
            })?;
        }
    }
    Ok(())
}

/// disconnectSteam: unlinks the account, optionally removes what the import brought in, and signs
/// the Steam webview out.
pub async fn disconnect(app: &AppHandle, delete_imported: bool) -> Result<(), String> {
    {
        let store = app.state::<Store>();
        disconnect_games(&store, delete_imported)?;
        store.del("", ACCOUNT_KEY)?;
    }
    clear_webview_data(app).await;
    let _ = app.emit("on-library-batch-complete", ());
    eprintln!("[steam] disconnected (delete imported data: {delete_imported})");
    Ok(())
}

/// session.clearStorageData for the Steam profile.
async fn clear_webview_data(app: &AppHandle) {
    let Ok(dir) = webview_dir(app) else { return };
    if !dir.exists() {
        return;
    }
    if let Some(old) = app.get_webview_window(SESSION_LABEL) {
        let _ = old.destroy();
    }
    let built = WebviewWindowBuilder::new(app, SESSION_LABEL, WebviewUrl::External("about:blank".parse().expect("static url"))).visible(false).data_directory(dir).build();
    match built {
        Ok(window) => {
            if let Err(e) = window.clear_all_browsing_data() {
                eprintln!("[steam] failed to clear the Steam webview data: {e}");
            }
            // The clear is asynchronous; give it a moment before the webview goes.
            tokio::time::sleep(Duration::from_secs(1)).await;
            let _ = window.destroy();
        }
        Err(e) => eprintln!("[steam] failed to open the Steam webview to clear it: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_config_yields_the_steam_id_and_token() {
        let html = r#"<div id="application_config" data-config="{}" data-userinfo="{&quot;logged_in&quot;:true,&quot;steamid&quot;:&quot;76561198000000001&quot;}" data-store_user_config="{&quot;webapi_token&quot;:&quot;tok&quot;}"></div>"#;
        assert_eq!(parse_store_config(EXPLORE_URL, html), Some(("76561198000000001".into(), "tok".into())));
        assert_eq!(parse_store_config(LOGIN_URL, html), None);
        let logged_out = html.replace("true", "false");
        assert_eq!(parse_store_config(EXPLORE_URL, &logged_out), None);
    }

    #[test]
    fn reads_unlocks_from_the_embedded_page_data() {
        let html = r#"{\"achievements\":{\"257\":{\"internal_key\":257,\"unlocked\":true,\"unlock_time\":1722061795},\"258\":{\"internal_key\":258,\"unlocked\":false,\"unlock_time\":0}}} [{\"internal_key\":257,\"api_name\":\"ACH_1\",\"name\":\"a\"},{\"internal_key\":258,\"api_name\":\"ACH_2\",\"name\":\"b\"}]"#;
        let unlocks = parse_achievement_ssr(html);
        assert_eq!(unlocks.len(), 1);
        assert_eq!((unlocks[0].name.as_str(), unlocks[0].unlock_time), ("ACH_1", 1722061795000));
    }

    #[test]
    fn logged_in_url_leaves_the_login_page() {
        assert!(is_logged_in_url(&"https://store.steampowered.com/explore/".parse().unwrap()));
        assert!(!is_logged_in_url(&LOGIN_URL.parse().unwrap()));
        assert!(!is_logged_in_url(&"https://login.steampowered.com/jwt".parse().unwrap()));
    }

    #[test]
    fn retry_follows_status_and_attempts() {
        let http = |status, retry_after| Error::Http { status, retry_after, body: String::new() };
        assert_eq!(retry_delay_ms(&http(429, None), 1), Some(1000));
        assert_eq!(retry_delay_ms(&http(429, None), 4), Some(8000));
        assert_eq!(retry_delay_ms(&http(429, Some(7)), 2), Some(7000));
        assert_eq!(retry_delay_ms(&http(429, None), 5), None);
        assert_eq!(retry_delay_ms(&http(502, None), 2), Some(1000));
        assert_eq!(retry_delay_ms(&http(502, None), 3), None);
        assert_eq!(retry_delay_ms(&http(500, None), 1), None);
    }

    #[test]
    fn library_merges_owned_family_and_playtime() {
        let owned = parse_owned_games(&json!({ "response": { "games": [
            { "appid": 10, "name": "Counter-Strike", "playtime_forever": 60, "playtime_disconnected": 5, "rtime_last_played": 1_700_000_000 },
            { "appid": 0, "name": "bad" },
        ] } }));
        assert_eq!(owned, vec![LibraryGame { app_id: "10".into(), name: "Counter-Strike".into(), play_seconds: 3900, last_played: Some(1_700_000_000_000) }]);
        let family = parse_shared_apps(&json!({ "response": { "apps": [
            { "appid": 20, "name": "Shared", "rt_playtime": 2, "rt_last_played": 0 },
            { "appid": 30, "name": "Excluded", "exclude_reason": 1 },
            { "appid": 40, "name": "Tool", "app_type": 4 },
            { "appid": 10, "name": "Counter-Strike" },
        ] } }));
        assert_eq!(family.len(), 2);
        let mut playtime = parse_last_played(&json!({ "response": { "games": [{ "appid": 10, "playtime_forever": 100, "last_playtime": 1_800_000_000 }] } }));
        for (id, p) in parse_family_playtime(&json!({ "response": { "entries": [
            { "appid": 20, "steamid": "me", "seconds": 600, "latest_played": 1_600_000_000 },
            { "appid": 20, "steamid": "someone else", "seconds": 99_999 },
        ] } }), "me") {
            add_playtime(&mut playtime, id, p);
        }
        let merged = merge_owned_and_family(owned, family, &playtime);
        assert_eq!(merged.len(), 2);
        assert_eq!((merged[0].play_seconds, merged[0].last_played), (6000, Some(1_800_000_000_000)));
        assert_eq!((merged[1].app_id.as_str(), merged[1].play_seconds, merged[1].last_played), ("20", 600, Some(1_600_000_000_000)));
        assert_eq!(family_group_id(&json!({ "response": { "family_groupid": "123" } })), Some("123".into()));
        assert_eq!(family_group_id(&json!({ "response": { "is_not_member_of_any_group": true, "family_groupid": "123" } })), None);
    }

    #[test]
    fn skips_demos_betas_and_test_clients() {
        for name in ["Game Demo", "Game (Beta)", "Game PTB", "Game Public Test", "Game Playtest", "Game Test Server", "Game Closed-Test"] {
            assert!(is_skipped_title(name), "{name}");
        }
        for name in ["Demolition Inc", "Alphabet", "Contest", "Testament"] {
            assert!(!is_skipped_title(name), "{name}");
        }
    }

    #[test]
    fn unlock_times_parse_in_both_orders_and_timezones() {
        let at = |y: i64, m: i64, d: i64, h: i64, min: i64| (days_from_civil(y, m, d) * 86_400 + h * 3600 + min * 60) * 1000;
        let now = at(2026, 10, 31, 12, 0);
        assert_eq!(parse_unlock_time("Unlocked 12 Mar, 2023 @ 4:05pm", 0, now), Some(at(2023, 3, 12, 16, 5)));
        assert_eq!(parse_unlock_time("Unlocked Mar 12, 2023 @ 12:30am", 0, now), Some(at(2023, 3, 12, 0, 30)));
        assert_eq!(parse_unlock_time("Unlocked 12 Mar @ 4:05 pm", 3600, now), Some(at(2026, 3, 12, 15, 5)));
        // A date without a year that would land in the future is last year's.
        assert_eq!(parse_unlock_time("Unlocked 20 Dec @ 1:00am", 0, now), Some(at(2025, 12, 20, 1, 0)));
        assert_eq!(parse_unlock_time("Locked", 0, now), None);
        assert_eq!(parse_iso_ms(&iso_from_ms(at(2023, 3, 12, 16, 5) as u64)), Some(at(2023, 3, 12, 16, 5) as u64));
        assert_eq!(timezone_offset(Some("-18000,0")), -18000);
        assert_eq!(timezone_offset(None), 0);
    }

    #[test]
    fn community_rows_map_to_schema_names() {
        let html = r#"<div class="achieveRow "><div class="achieveImgHolder"><img src="https://cdn/apps/10/0123456789abcdef0123.jpg"></div>
            <div class="achieveTxt"><h3 class="ellipsis">First &amp; Best</h3><h5>Win once</h5></div>
            <div class="achieveUnlockTime">Unlocked 1 Jan, 2024 @ 1:00am<br></div></div>
            <div class="achieveRow"><h3 class="ellipsis">Named</h3><h5></h5><div class="achieveUnlockTime">Unlocked Feb 2, 2024 @ 2:00pm</div></div>
            <div class="achieveRow"><h3 class="ellipsis">Locked one</h3></div>"#;
        let rows = parse_achievement_html(html, 0, i64::MAX / 4);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "First & Best");
        let schema = parse_schema(&json!({ "response": { "achievements": [
            { "internal_name": "ACH_WIN", "localized_name": "Other name", "icon": "0123456789abcdef0123.jpg" },
            { "internal_name": "ACH_NAMED", "localized_name": "Named", "icon": "" },
        ] } }));
        let unlocks = map_html_unlocks(&rows, &schema);
        let names: Vec<&str> = unlocks.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, ["ACH_WIN", "ACH_NAMED"]);
        assert_eq!(unlocks[0].unlock_time, (days_from_civil(2024, 1, 1) * 86_400 + 3600) * 1000);
    }

    #[test]
    fn xml_stats_keep_closed_achievements() {
        let xml = r#"<playerstats><achievements><achievement closed="1"><apiname><![CDATA[ACH_A]]></apiname><unlockTimestamp>1700000000</unlockTimestamp></achievement>
            <achievement closed="0"><apiname>ACH_B</apiname></achievement></achievements></playerstats>"#;
        let unlocks = parse_achievement_xml(xml);
        assert_eq!(unlocks.len(), 1);
        assert_eq!((unlocks[0].name.as_str(), unlocks[0].unlock_time), ("ACH_A", 1_700_000_000_000));
        assert!(is_login_page("https://steamcommunity.com/login/home/", ""));
        assert!(!is_login_page("https://steamcommunity.com/profiles/1/stats/10/?xml=1", xml));
    }

    #[test]
    fn store_art_uses_hashed_paths() {
        let payload = json!({ "response": { "store_items": [
            { "appid": 3527290, "assets": { "asset_url_format": "steam/apps/3527290/${FILENAME}?t=1", "library_capsule": "h1/library_600x900.jpg", "library_capsule_2x": "h1/library_600x900_2x.jpg", "library_hero": "h2/library_hero.jpg" } },
            { "appid": 460960, "assets": { "asset_url_format": "steam/apps/460960/${FILENAME}?t=2", "library_capsule": "portrait.png" } },
            { "appid": 360, "assets": { "asset_url_format": "steam/apps/360/${FILENAME}?t=3", "header": "header.jpg" } },
            { "id": 367540, "success": 15 },
        ] } });
        let art = parse_store_art(&payload);
        assert_eq!(art.len(), 3);
        assert_eq!(art["360"].hero.as_deref(), Some("https://shared.steamstatic.com/store_item_assets/steam/apps/360/header.jpg?t=3"));
        assert_eq!(art["3527290"].cover.as_deref(), Some("https://shared.steamstatic.com/store_item_assets/steam/apps/3527290/h1/library_600x900_2x.jpg?t=1"));
        assert_eq!(art["3527290"].hero.as_deref(), Some("https://shared.steamstatic.com/store_item_assets/steam/apps/3527290/h2/library_hero.jpg?t=1"));
        assert_eq!(art["460960"], StoreArt { cover: Some("https://shared.steamstatic.com/store_item_assets/steam/apps/460960/portrait.png?t=2".into()), hero: None });
    }

    #[test]
    fn guessed_art_is_found_and_replaced() {
        let store = Store::memory().unwrap();
        let mut guessed = serde_json::Map::new();
        put_art(&mut guessed, "30", None);
        store.put("gameShopAssets", "steam:30", &Value::Object(guessed.clone())).unwrap();
        store.put("gameShopAssets", "steam:40", &json!({ "coverImageUrl": "catalogue-cover" })).unwrap();
        assert_eq!(guessed_art_ids(&store).unwrap(), vec!["30".to_string()]);
        put_art(&mut guessed, "30", Some(&StoreArt { cover: Some("hashed".into()), hero: None }));
        assert_eq!((guessed["coverImageUrl"].as_str(), guessed["iconUrl"].as_str()), (Some("hashed"), Some("hashed")));
        assert_eq!(guessed["libraryHeroImageUrl"], format!("{STEAM_ASSETS}/30/library_hero.jpg"));
    }

    #[test]
    fn library_merge_keeps_local_fields_and_hides_removed_imports() {
        let store = Store::memory().unwrap();
        store.put("games", "steam:10", &json!({ "shop": "steam", "objectId": "10", "title": "Local", "favorite": true, "playTimeInMilliseconds": 5000, "source": "hydra", "isDeleted": false, "lastTimePlayed": "2030-01-01T00:00:00.000Z" })).unwrap();
        store.put("games", "steam:99", &json!({ "shop": "steam", "objectId": "99", "hasActiveSteamImport": true, "isDeleted": false })).unwrap();
        let games = vec![
            LibraryGame { app_id: "10".into(), name: "Remote".into(), play_seconds: 60, last_played: Some(1_000) },
            LibraryGame { app_id: "20".into(), name: "New".into(), play_seconds: 0, last_played: None },
        ];
        store.put("gameShopAssets", "steam:10", &json!({ "coverImageUrl": "catalogue-cover", "libraryHeroImageUrl": "catalogue-hero" })).unwrap();
        let art = HashMap::from([("20".to_string(), StoreArt { cover: Some("hashed-cover".into()), hero: None })]);
        assert_eq!(merge_library(&store, &games, &art).unwrap(), 1);
        let kept = store.get("gameShopAssets", "steam:10").unwrap();
        assert_eq!((kept["coverImageUrl"].as_str(), kept["libraryHeroImageUrl"].as_str()), (Some("catalogue-cover"), Some("catalogue-hero")));
        let fresh = store.get("gameShopAssets", "steam:20").unwrap();
        assert_eq!((fresh["coverImageUrl"].as_str(), fresh["iconUrl"].as_str()), (Some("hashed-cover"), Some("hashed-cover")));
        assert_eq!(fresh["libraryHeroImageUrl"], format!("{STEAM_ASSETS}/20/library_hero.jpg"));
        hide_removed(&store, &games.iter().map(|g| game_key("steam", &g.app_id)).collect()).unwrap();
        let local = store.get("games", "steam:10").unwrap();
        assert_eq!((local["title"].as_str(), local["favorite"].as_bool(), local["source"].as_str()), (Some("Local"), Some(true), Some("hydra")));
        assert_eq!((local["playTimeInMilliseconds"].as_u64(), local["steamPlayTimeInMilliseconds"].as_u64()), (Some(5000), Some(60_000)));
        assert_eq!(local["lastTimePlayed"], "2030-01-01T00:00:00.000Z");
        let new = store.get("games", "steam:20").unwrap();
        assert_eq!((new["source"].as_str(), new["hasActiveSteamImport"].as_bool()), (Some("steam"), Some(true)));
        assert_eq!(store.get("gameShopAssets", "steam:20").unwrap()["title"], "New");
        let gone = store.get("games", "steam:99").unwrap();
        assert_eq!((gone["isDeleted"].as_bool(), gone["hasActiveSteamImport"].as_bool()), (Some(true), Some(false)));
    }

    #[test]
    fn disconnect_removes_steam_only_games_and_keeps_installs() {
        let store = Store::memory().unwrap();
        store.put("games", "steam:1", &json!({ "shop": "steam", "objectId": "1", "source": "steam", "hasActiveSteamImport": true, "isDeleted": false, "steamPlayTimeInMilliseconds": 10 })).unwrap();
        store.put("games", "steam:2", &json!({ "shop": "steam", "objectId": "2", "source": "hydra", "hasActiveSteamImport": true, "isDeleted": false, "executablePath": "C:\\g.exe" })).unwrap();
        store.put("games", "steam:3", &json!({ "shop": "steam", "objectId": "3", "source": "steam", "hasActiveSteamImport": true, "isDeleted": false, "playTimeInMilliseconds": 1000 })).unwrap();
        store.put("unlockedAchievements", "steam:1", &json!([{ "name": "A", "unlockTime": 1 }])).unwrap();
        disconnect_games(&store, true).unwrap();
        let one = store.get("games", "steam:1").unwrap();
        assert_eq!((one["isDeleted"].as_bool(), one["steamPlayTimeInMilliseconds"].as_u64()), (Some(true), Some(0)));
        assert!(store.get("unlockedAchievements", "steam:1").unwrap().is_null());
        let two = store.get("games", "steam:2").unwrap();
        assert_eq!((two["isDeleted"].as_bool(), two["hasActiveSteamImport"].as_bool()), (Some(false), Some(false)));
        // Chimaera playtime keeps a game even when only Steam brought it in.
        let three = store.get("games", "steam:3").unwrap();
        assert_eq!((three["isDeleted"].as_bool(), three["source"].as_str()), (Some(false), Some("hydra")));
    }

    #[test]
    fn plain_disconnect_only_drops_the_import_flag() {
        let store = Store::memory().unwrap();
        store.put("games", "steam:1", &json!({ "shop": "steam", "objectId": "1", "source": "steam", "hasActiveSteamImport": true, "isDeleted": false })).unwrap();
        disconnect_games(&store, false).unwrap();
        let one = store.get("games", "steam:1").unwrap();
        assert_eq!((one["isDeleted"].as_bool(), one["hasActiveSteamImport"].as_bool(), one["source"].as_str()), (Some(false), Some(false), Some("steam")));
    }

    #[test]
    fn entities_and_profile_fields_decode() {
        assert_eq!(decode_entities("a &amp; b &#39;c&#x27; &bogus; &"), "a & b 'c' &bogus; &");
        let xml = "<profile><steamID><![CDATA[Gabe]]></steamID><avatarFull><![CDATA[https://a/b.jpg]]></avatarFull></profile>";
        assert_eq!(profile_field(xml, "steamID").as_deref(), Some("Gabe"));
        assert_eq!(profile_field(xml, "avatarFull").as_deref(), Some("https://a/b.jpg"));
        assert_eq!(icon_hash("https://cdn/x/0123456789ABCDEF.jpg?x=1").as_deref(), Some("0123456789abcdef"));
        assert_eq!(icon_hash("short.jpg"), None);
    }
}

