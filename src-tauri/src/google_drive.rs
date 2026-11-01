//! Google Drive save backups (port of `google-drive/drive.ts`, `drive-core.ts`, `google-oauth.ts`).
//! Local backup tars are uploaded to "Chimaera Saves/<shop>-<objectId>" with their metadata
//! in the file description, so Electron and Tauri see each other's uploads.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::save_backups;

const API: &str = "https://www.googleapis.com/drive/v3/files";
const UPLOAD_API: &str = "https://www.googleapis.com/upload/drive/v3/files";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const ROOT_FOLDER: &str = "Chimaera Saves";
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
const SCOPES: &str = "https://www.googleapis.com/auth/drive.file openid email";
const KEEP: usize = 10;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const KEYRING_SERVICE: &str = "chimaera";
const KEYRING_ACCOUNT: &str = "google-drive";

// shortcut: Electron's safeStorage-encrypted google-drive.json is not migrated; users reconnect Drive once.
static ACCESS: Mutex<Option<(String, Instant)>> = Mutex::new(None);
// Used when the OS keystore is unavailable, so the session lasts until restart.
static MEMORY_ACCOUNT: Mutex<Option<Value>> = Mutex::new(None);

fn credentials() -> Option<(&'static str, &'static str)> {
    let id = option_env!("CHIMAERA_GOOGLE_CLIENT_ID").filter(|v| !v.is_empty())?;
    Some((id, option_env!("CHIMAERA_GOOGLE_CLIENT_SECRET").unwrap_or_default()))
}

fn entry() -> Option<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT).ok()
}

fn load_account() -> Option<Value> {
    let stored = entry().and_then(|e| e.get_password().ok()).and_then(|text| serde_json::from_str(&text).ok());
    stored.or_else(|| MEMORY_ACCOUNT.lock().unwrap_or_else(|e| e.into_inner()).clone())
}

fn save_account(account: Value) {
    if let Some(e) = entry() {
        if let Err(err) = e.set_password(&account.to_string()) {
            eprintln!("[google-drive] keyring unavailable, keeping the session in memory: {err}");
        }
    }
    *MEMORY_ACCOUNT.lock().unwrap_or_else(|e| e.into_inner()) = Some(account);
}

fn set_access(token: &str, expires_in: u64) {
    *ACCESS.lock().unwrap_or_else(|e| e.into_inner()) = Some((token.to_string(), Instant::now() + Duration::from_secs(expires_in)));
}

/// `getGoogleDriveStatus`.
pub fn status() -> Value {
    let account = load_account();
    json!({ "connected": account.is_some(), "email": account.as_ref().map_or(Value::Null, |a| a["email"].clone()) })
}

async fn token_request(params: &[(&str, &str)]) -> Result<Value, String> {
    let (id, secret) = credentials().ok_or("google-drive/not-configured")?;
    let mut form = vec![("client_id", id), ("client_secret", secret)];
    form.extend_from_slice(params);
    let response = reqwest::Client::new().post(TOKEN_URL).form(&form).send().await.map_err(|e| e.to_string())?;
    response.error_for_status().map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())
}

fn pkce_pair() -> (String, String) {
    let random: Vec<u8> = (0..48).map(|_| fastrand::u8(..)).collect();
    let verifier = URL_SAFE_NO_PAD.encode(random);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

fn auth_url(client_id: &str, redirect_uri: &str, challenge: &str, state: &str) -> String {
    url::Url::parse_with_params(
        "https://accounts.google.com/o/oauth2/v2/auth",
        [
            ("client_id", client_id),
            ("redirect_uri", redirect_uri),
            ("response_type", "code"),
            ("scope", SCOPES),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
            // Without these Google only returns a refresh token on the first consent.
            ("access_type", "offline"),
            ("prompt", "consent"),
        ],
    )
    .map(String::from)
    .unwrap_or_default()
}

/// The `code` of a loopback callback request line, if it carries our `state`.
fn callback_code(request: &str, state: &str) -> Option<String> {
    let target = request.lines().next()?.split_whitespace().nth(1)?;
    let url = url::Url::parse(&format!("http://127.0.0.1{target}")).ok()?;
    let param = |name: &str| url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned());
    (param("state")? == state).then(|| param("code")).flatten()
}

fn page(message: &str) -> String {
    format!("<!doctype html><meta charset=\"utf-8\"><title>Chimaera</title><body style=\"font-family:sans-serif;padding:2rem\">{message}</body>")
}

/// Waits on a one-shot loopback server for Google's redirect and returns the code.
async fn wait_for_code(listener: tokio::net::TcpListener, state: &str) -> Result<String, String> {
    loop {
        let (mut socket, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 8192];
        let n = socket.read(&mut buf).await.unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]);
        // Stray hits (favicon etc.) and callbacks without our state get a 400 and are ignored.
        let (code, status, body) = match callback_code(&request, state) {
            Some(code) => (Some(code), "200 OK", page("Google Drive connected. You can close this tab.")),
            None => (None, "400 Bad Request", page("Invalid request.")),
        };
        let response = format!("HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        let _ = socket.write_all(response.as_bytes()).await;
        if let Some(code) = code {
            return Ok(code);
        }
    }
}

/// The `email` claim of an id_token (decoded, not verified: it is only a display label).
fn id_token_email(id_token: &str) -> Option<String> {
    let payload = URL_SAFE_NO_PAD.decode(id_token.split('.').nth(1)?.trim_end_matches('=')).ok()?;
    serde_json::from_slice::<Value>(&payload).ok()?["email"].as_str().map(str::to_string)
}

/// `connectGoogleDrive`: OAuth with PKCE in the browser, keeps the refresh token.
pub async fn connect(app: &AppHandle) -> Result<Value, String> {
    let (client_id, _) = credentials().ok_or("google-drive/not-configured")?;
    let state: String = (0..16).map(|_| format!("{:02x}", fastrand::u8(..))).collect();
    let (verifier, challenge) = pkce_pair();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(|e| e.to_string())?;
    let redirect_uri = format!("http://127.0.0.1:{}", listener.local_addr().map_err(|e| e.to_string())?.port());
    app.opener().open_url(auth_url(client_id, &redirect_uri, &challenge, &state), None::<&str>).map_err(|e| e.to_string())?;
    let code = tokio::time::timeout(CONNECT_TIMEOUT, wait_for_code(listener, &state)).await.map_err(|_| "google-drive/timeout")??;

    let tokens = token_request(&[("grant_type", "authorization_code"), ("code", &code), ("code_verifier", &verifier), ("redirect_uri", &redirect_uri)]).await?;
    let refresh = tokens["refresh_token"].as_str().ok_or("google-drive/no-refresh-token")?;
    let email = tokens["id_token"].as_str().and_then(id_token_email);
    save_account(json!({ "refreshToken": refresh, "email": email }));
    set_access(tokens["access_token"].as_str().unwrap_or_default(), tokens["expires_in"].as_u64().unwrap_or(0));
    Ok(json!({ "connected": true, "email": email }))
}

/// `disconnectGoogleDrive`: forget locally, then revoke on Google's side (best effort).
pub async fn disconnect() {
    let account = load_account();
    *ACCESS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *MEMORY_ACCOUNT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    if let Some(e) = entry() {
        let _ = e.delete_credential();
    }
    let Some(token) = account.as_ref().and_then(|a| a["refreshToken"].as_str()) else { return };
    let revoked = reqwest::Client::new().post("https://oauth2.googleapis.com/revoke").form(&[("token", token)]).send().await;
    if let Err(e) = revoked {
        eprintln!("[google-drive] failed to revoke token: {e}");
    }
}

async fn access_token() -> Result<String, String> {
    if let Some((token, expires)) = ACCESS.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        if expires > Instant::now() + Duration::from_secs(60) {
            return Ok(token);
        }
    }
    let account = load_account().ok_or("google-drive/not-connected")?;
    let tokens = token_request(&[("grant_type", "refresh_token"), ("refresh_token", account["refreshToken"].as_str().unwrap_or_default())]).await?;
    let token = tokens["access_token"].as_str().ok_or("google-drive/no-access-token")?.to_string();
    set_access(&token, tokens["expires_in"].as_u64().unwrap_or(0));
    Ok(token)
}

async fn authed(request: reqwest::RequestBuilder) -> Result<reqwest::Response, String> {
    let response = request.bearer_auth(access_token().await?).send().await.map_err(|e| e.to_string())?;
    response.error_for_status().map_err(|e| e.to_string())
}

/// Escapes a value for use inside a single-quoted Drive `q` string.
fn escape_query(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

async fn list_files(client: &reqwest::Client, q: &str) -> Result<Vec<Value>, String> {
    let request = client.get(API).query(&[("q", format!("{q} and trashed=false").as_str()), ("fields", "files(id,name,description,size)"), ("pageSize", "200")]);
    let data: Value = authed(request).await?.json().await.map_err(|e| e.to_string())?;
    Ok(data["files"].as_array().cloned().unwrap_or_default())
}

async fn ensure_folder(client: &reqwest::Client, name: &str, parent: &str) -> Result<String, String> {
    let q = format!("name='{}' and mimeType='{FOLDER_MIME}' and '{parent}' in parents", escape_query(name));
    if let Some(id) = list_files(client, &q).await?.first().and_then(|f| f["id"].as_str()) {
        return Ok(id.to_string());
    }
    let request = client.post(API).query(&[("fields", "id")]).json(&json!({ "name": name, "mimeType": FOLDER_MIME, "parents": [parent] }));
    let created: Value = authed(request).await?.json().await.map_err(|e| e.to_string())?;
    created["id"].as_str().map(str::to_string).ok_or_else(|| "google-drive/no-folder-id".into())
}

async fn game_folder(client: &reqwest::Client, shop: &str, object_id: &str) -> Result<String, String> {
    let root = ensure_folder(client, ROOT_FOLDER, "root").await?;
    ensure_folder(client, &save_backups::game_folder_name(shop, object_id), &root).await
}

/// A Drive file as a backup: its description holds the sidecar metadata.
fn to_backup(file: &Value) -> Option<Value> {
    let mut meta: Value = serde_json::from_str(file["description"].as_str()?).ok()?;
    let fields = meta.as_object_mut()?;
    fields.insert("file".into(), file["name"].clone());
    fields.insert("driveFileId".into(), file["id"].clone());
    Some(meta)
}

/// `listDriveSaveBackups`, newest first.
pub async fn list(shop: &str, object_id: &str) -> Result<Vec<Value>, String> {
    let client = reqwest::Client::new();
    let folder = game_folder(&client, shop, object_id).await?;
    let mut backups: Vec<Value> = list_files(&client, &format!("'{folder}' in parents and name contains '.tar'")).await?.iter().filter_map(to_backup).collect();
    backups.sort_by(|a, b| b["createdAt"].as_str().unwrap_or_default().cmp(a["createdAt"].as_str().unwrap_or_default()));
    Ok(backups)
}

/// Drive multipart upload body: JSON metadata part, then the file bytes.
// shortcut: whole tar in memory, save tars are small; use a resumable upload if they reach hundreds of MB.
fn multipart_body(metadata: &Value, content: &[u8], boundary: &str) -> Vec<u8> {
    let mut body = format!(
        "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{metadata}\r\n--{boundary}\r\nContent-Type: application/x-tar\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(content);
    body.extend_from_slice(format!("\r\n--{boundary}--").as_bytes());
    body
}

/// `uploadSaveBackupToDrive`; a backup already on Drive under the same name is left alone.
pub async fn upload(app: &AppHandle, shop: &str, object_id: &str, file: &str) -> Result<(), String> {
    let dir = save_backups::game_dir(app, shop, object_id)?;
    let mut meta = save_backups::list_in(&dir).into_iter().find(|b| b["file"] == file).ok_or("save-backup/not-found")?;
    if list(shop, object_id).await?.iter().any(|b| b["file"] == file) {
        return Ok(());
    }
    let client = reqwest::Client::new();
    let folder = game_folder(&client, shop, object_id).await?;
    if let Some(fields) = meta.as_object_mut() {
        fields.remove("file");
        fields.remove("sizeBytes");
    }
    let content = std::fs::read(save_backups::backup_path(&dir, file)?).map_err(|e| e.to_string())?;
    let boundary = uuid::Uuid::new_v4().to_string();
    let body = multipart_body(&json!({ "name": file, "parents": [folder], "description": meta.to_string() }), &content, &boundary);
    let request = client
        .post(UPLOAD_API)
        .query(&[("uploadType", "multipart"), ("fields", "id")])
        .header(reqwest::header::CONTENT_TYPE, format!("multipart/related; boundary={boundary}"))
        .body(body);
    authed(request).await.map(|_| ())
}

/// `downloadDriveSaveBackup`: into the local backups folder, from where the normal restore applies it.
pub async fn download(app: &AppHandle, shop: &str, object_id: &str, drive_file_id: &str) -> Result<(), String> {
    let mut meta = list(shop, object_id).await?.into_iter().find(|b| b["driveFileId"] == drive_file_id).ok_or("save-backup/not-found")?;
    let bytes = authed(reqwest::Client::new().get(format!("{API}/{drive_file_id}")).query(&[("alt", "media")])).await?.bytes().await.map_err(|e| e.to_string())?;
    let tmp = std::env::temp_dir().join(format!("{}.tar", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    if let Some(fields) = meta.as_object_mut() {
        for key in ["file", "sizeBytes", "driveFileId"] {
            fields.remove(key);
        }
    }
    let stored = save_backups::store_in(&save_backups::game_dir(app, shop, object_id)?, &tmp, meta);
    let _ = std::fs::remove_file(&tmp);
    stored.map(|_| ())
}

/// `deleteDriveSaveBackup`.
pub async fn delete(drive_file_id: &str) -> Result<(), String> {
    authed(reqwest::Client::new().delete(format!("{API}/{drive_file_id}"))).await.map(|_| ())
}

/// Ids of the oldest backups beyond the newest `keep`.
fn to_prune(backups: &[Value], keep: usize) -> Vec<String> {
    let mut sorted: Vec<&Value> = backups.iter().collect();
    sorted.sort_by(|a, b| b["createdAt"].as_str().unwrap_or_default().cmp(a["createdAt"].as_str().unwrap_or_default()));
    sorted.iter().skip(keep).filter_map(|b| b["driveFileId"].as_str().map(str::to_string)).collect()
}

/// Runs after a game closes: snapshot, upload, then drop Drive copies past the limit.
pub async fn auto_backup(app: &AppHandle, shop: &str, object_id: &str) -> Result<(), String> {
    if load_account().is_none() {
        return Ok(());
    }
    let backup = save_backups::create(app, shop, object_id, None, true).await?;
    upload(app, shop, object_id, backup["file"].as_str().unwrap_or_default()).await?;
    for id in to_prune(&list(shop, object_id).await?, KEEP) {
        if let Err(e) = delete(&id).await {
            eprintln!("[google-drive] failed to prune {id}: {e}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_the_verifiers_sha256() {
        let (verifier, challenge) = pkce_pair();
        assert_eq!(verifier.len(), 64);
        assert_eq!(challenge, URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())));
    }

    #[test]
    fn auth_url_asks_for_offline_drive_file_access() {
        let url = url::Url::parse(&auth_url("id", "http://127.0.0.1:5000", "ch", "st")).unwrap();
        let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(params["scope"], SCOPES);
        assert_eq!(params["access_type"], "offline");
        assert_eq!(params["redirect_uri"], "http://127.0.0.1:5000");
    }

    #[test]
    fn callback_needs_our_state() {
        assert_eq!(callback_code("GET /?code=abc&state=s1 HTTP/1.1\r\n", "s1").as_deref(), Some("abc"));
        assert_eq!(callback_code("GET /?code=abc&state=other HTTP/1.1\r\n", "s1"), None);
        assert_eq!(callback_code("GET /favicon.ico HTTP/1.1\r\n", "s1"), None);
    }

    #[test]
    fn email_comes_from_the_id_token_payload() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"email":"a@b.c"}"#);
        assert_eq!(id_token_email(&format!("h.{payload}.sig")).as_deref(), Some("a@b.c"));
        assert_eq!(id_token_email("garbage"), None);
    }

    #[test]
    fn query_values_are_escaped() {
        assert_eq!(escape_query(r"it's a\b"), r"it\'s a\\b");
    }

    #[test]
    fn multipart_wraps_metadata_then_bytes() {
        let body = multipart_body(&json!({ "name": "x.tar" }), b"DATA", "B");
        let text = String::from_utf8(body).unwrap();
        assert!(text.starts_with("--B\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{\"name\":\"x.tar\"}\r\n--B\r\n"));
        assert!(text.ends_with("application/x-tar\r\n\r\nDATA\r\n--B--"));
    }

    #[test]
    fn pruning_keeps_the_newest() {
        let backups: Vec<Value> = (1..=12).map(|d| json!({ "driveFileId": format!("f{d}"), "createdAt": format!("2026-10-{d:02}T00:00:00Z") })).collect();
        assert_eq!(to_prune(&backups, 10), ["f2", "f1"]);
    }

    #[test]
    fn drive_files_without_metadata_are_skipped() {
        let ok = json!({ "id": "1", "name": "a.tar", "description": "{\"label\":\"x\",\"createdAt\":\"t\"}" });
        assert_eq!(to_backup(&ok).unwrap()["driveFileId"], "1");
        assert!(to_backup(&json!({ "id": "2", "name": "b.tar" })).is_none());
    }
}
