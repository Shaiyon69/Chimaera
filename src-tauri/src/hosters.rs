//! Direct links for file hosters, ported from src/main/services/hosters/, src/shared/archive-org.ts,
//! download-filename.ts and the per-hoster option builders in download-manager.ts.

use std::time::Duration;

use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde_json::Value;

use crate::http_download::USER_AGENT;

pub const GOFILE: i64 = 2;
pub const PIXELDRAIN: i64 = 3;
pub const DATANODES: i64 = 4;
pub const MEDIAFIRE: i64 = 5;
pub const FUCKINGFAST: i64 = 9;
pub const VIKINGFILE: i64 = 10;
pub const ROOTZ: i64 = 11;
pub const ARCHIVE_ORG: i64 = 14;

const LOGIN_REQUIRED: &str = "download_error_hoster_unlock_login_required";
const FILE_NOT_FOUND: &str = "download_error_hoster_unlock_file_not_found";
const UNAVAILABLE: &str = "download_error_hoster_unlock_unavailable";
const ARCHIVE_ORG_INVALID: &str = "download_error_archive_org_invalid_file_url";
const CHROME_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
const GOFILE_MIRROR: &str = "https://gofilecdn.eu.cc";
const ROOTZ_URL: &str = "https://www.rootz.so";

/// `encodeURIComponent`'s set.
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'!').remove(b'~').remove(b'*').remove(b'\'').remove(b'(').remove(b')');

/// Downloaders this module resolves to a plain HTTP file.
pub fn is_hoster(downloader: i64) -> bool {
    matches!(downloader, GOFILE | PIXELDRAIN | DATANODES | MEDIAFIRE | FUCKINGFAST | VIKINGFILE | ROOTZ | ARCHIVE_ORG)
}

pub struct Resolved {
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// The file name to save under when the download has none yet.
    pub filename: Option<String>,
}

fn client(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder().timeout(timeout).build().map_err(|e| e.to_string())
}

/// The direct link for `uri` on hoster `downloader`.
pub async fn resolve(downloader: i64, uri: &str) -> Result<Resolved, String> {
    let url = match downloader {
        GOFILE => gofile(uri).await?,
        PIXELDRAIN => pixeldrain(uri).await?,
        // Both unlock through the Hydra API with a signed-in account; Chimaera has no sign-in.
        DATANODES | VIKINGFILE => return Err(LOGIN_REQUIRED.into()),
        MEDIAFIRE => mediafire(uri).await?,
        FUCKINGFAST => fuckingfast(uri).await?,
        ROOTZ => rootz(uri).await?,
        ARCHIVE_ORG => {
            let (url, filename) = archive_org(uri).ok_or(ARCHIVE_ORG_INVALID)?;
            return Ok(Resolved { url, headers: vec![], filename: Some(sanitize(&filename)) });
        }
        other => return Err(format!("Unsupported download host: {other}")),
    };
    Ok(Resolved { filename: filename_hint(uri, &url).map(|f| sanitize(&f)), url, headers: vec![] })
}

/// `sanitizeFilename`.
pub fn sanitize(name: &str) -> String {
    name.chars().map(|c| if "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect()
}

fn is_magnet(value: &str) -> bool {
    value.trim().to_ascii_lowercase().starts_with("magnet:")
}

fn hash_hint(value: &str) -> Option<String> {
    if is_magnet(value) {
        return None;
    }
    let hash = value.split('#').nth(1)?;
    (!hash.is_empty() && !hash.starts_with("http") && hash.contains('.') && !hash.contains('=')).then(|| hash.to_string())
}

fn path_name(value: &str) -> Option<String> {
    if is_magnet(value) {
        return None;
    }
    let url = url::Url::parse(value).ok()?;
    let last = url.path().rsplit('/').next()?;
    if !last.contains('.') {
        return None;
    }
    percent_encoding::percent_decode_str(last).decode_utf8().ok().map(|s| s.into_owned())
}

/// `resolveFilename` without a resumed name: a `#name.ext` hint, then the URL path.
pub fn filename_hint(uri: &str, url: &str) -> Option<String> {
    hash_hint(url).or_else(|| hash_hint(uri)).or_else(|| path_name(uri)).or_else(|| path_name(url))
}

// ---- Gofile ----

fn gofile_id(uri: &str) -> Option<String> {
    let trimmed = uri.trim();
    let normalized = if trimmed.starts_with("http://") || trimmed.starts_with("https://") { trimmed.to_string() } else { format!("https://{trimmed}") };
    let id = match url::Url::parse(&normalized) {
        Ok(url) => url.path_segments()?.rfind(|s| !s.is_empty())?.to_string(),
        Err(_) => normalized.split('?').next()?.split('/').rfind(|s| !s.is_empty())?.to_string(),
    };
    (!id.is_empty()).then_some(id)
}

/// Gofile through its download mirror, Electron's fallback.
// shortcut: the official Gofile API needs an X-Website-Token derived by running Gofile's obfuscated
// wt.obf.js in a JS VM (gofile.ts); the Rust side has no JS engine, so only the mirror is used.
async fn gofile(uri: &str) -> Result<String, String> {
    let id = gofile_id(uri).ok_or("Invalid gofile URL")?;
    let url = format!("{GOFILE_MIRROR}/{}", utf8_percent_encode(&id, COMPONENT));
    let response = client(Duration::from_secs(5))?
        .get(&url)
        .header("user-agent", CHROME_USER_AGENT)
        .header("accept", "*/*")
        .header("range", "bytes=0-0")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let content_type = response.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or_default().to_ascii_lowercase();
    let error_body = ["text/html", "application/xhtml", "text/plain", "application/json"].iter().any(|t| content_type.contains(t));
    if response.status().is_success() && !error_body {
        return Ok(url);
    }
    Err(format!("Gofile file {id} is not available on the Gofile download mirror (HTTP {}).", response.status().as_u16()))
}

// ---- PixelDrain ----

async fn pixeldrain(uri: &str) -> Result<String, String> {
    let invalid = || format!("Invalid pixeldrain URL: {uri}");
    let url = url::Url::parse(uri).map_err(|_| invalid())?;
    let parts: Vec<&str> = url.path_segments().map(|s| s.filter(|p| !p.is_empty()).collect()).unwrap_or_default();
    let (Some(&"u"), Some(id)) = (parts.first(), parts.get(1)) else { return Err(invalid()) };
    let response = client(Duration::from_secs(30))?.get(format!("https://pixeldrain.com/api/file/{id}/info")).send().await.map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    if status == 404 {
        return Err(FILE_NOT_FOUND.into());
    }
    let info: Value = response.json().await.unwrap_or(Value::Null);
    let blocked = info["availability"].as_str().is_some_and(|a| !a.is_empty()) || info["can_download"] == false;
    if status >= 400 || blocked {
        return Err(UNAVAILABLE.into());
    }
    Ok(format!("https://pixeldrain.com/api/file/{id}?download"))
}

// ---- Mediafire ----

fn mediafire_page(url: &str) -> String {
    let mut processed = url.replacen("http://", "https://", 1);
    if !processed.is_empty() && processed.chars().all(|c| c.is_ascii_alphanumeric()) {
        processed = format!("https://mediafire.com/?{processed}");
    }
    if !(processed.starts_with("http://") || processed.starts_with("https://")) {
        processed = if processed.starts_with("//") { format!("https:{processed}") } else { format!("https://{processed}") };
    }
    processed
}

/// `(https?:)?(//)?(www.)?mediafire.com/(file|view|download)/<path>?dkey=<key>`
fn is_mediafire_pre_link(text: &str) -> bool {
    let rest = text.strip_prefix("https:").or_else(|| text.strip_prefix("http:")).unwrap_or(text);
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let Some(rest) = rest.strip_prefix("mediafire.com/") else { return false };
    let Some(rest) = ["file/", "view/", "download/"].iter().find_map(|p| rest.strip_prefix(p)) else { return false };
    let Some((path, query)) = rest.split_once('?') else { return false };
    !path.is_empty() && query.strip_prefix("dkey=").is_some_and(|k| !k.is_empty())
}

/// `https?://download<digits>.mediafire.com/<path>`
fn is_mediafire_direct(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("https://").or_else(|| text.strip_prefix("http://")) else { return false };
    let Some(rest) = rest.strip_prefix("download") else { return false };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && rest[digits..].strip_prefix(".mediafire.com/").is_some_and(|p| !p.is_empty())
}

/// The quoted strings of a page; Electron's regexes match between two quote characters.
fn quoted(html: &str) -> impl Iterator<Item = &str> {
    let quotes: Vec<usize> = html.match_indices(['"', '\'']).map(|(i, _)| i).collect();
    (0..quotes.len().saturating_sub(1)).map(move |i| &html[quotes[i] + 1..quotes[i + 1]])
}

fn mediafire_link(html: &str) -> Option<String> {
    if let Some(pre) = quoted(html).find(|s| is_mediafire_pre_link(s)) {
        return Some(if pre.starts_with("//") { format!("https:{pre}") } else { pre.to_string() });
    }
    quoted(html).find(|s| is_mediafire_direct(s)).map(str::to_string)
}

async fn mediafire(uri: &str) -> Result<String, String> {
    let failed = || "Failed to get download URL".to_string();
    let response = client(Duration::from_secs(30))?.get(mediafire_page(uri)).send().await.map_err(|_| failed())?;
    if !response.status().is_success() {
        return Err(failed());
    }
    let html = response.text().await.map_err(|_| failed())?;
    mediafire_link(&html).ok_or_else(failed)
}

// ---- FuckingFast ----

fn fuckingfast_link(html: &str) -> Option<String> {
    const MARKER: &str = "window.open(\"https://fuckingfast.co/dl/";
    let mut from = 0;
    while let Some(at) = html[from..].find(MARKER) {
        let start = from + at + "window.open(\"".len();
        let end = start + html[start..].find('"')?;
        if html[end + 1..].starts_with(')') {
            return Some(html[start..end].to_string());
        }
        from = start;
    }
    None
}

async fn fuckingfast(uri: &str) -> Result<String, String> {
    if !uri.to_ascii_lowercase().contains("fuckingfast.co") {
        return Err("Unsupported domain. Supported domains: fuckingfast.co".into());
    }
    let response = client(Duration::from_secs(30))?.get(uri).header("user-agent", USER_AGENT).send().await.map_err(|e| e.to_string())?;
    match response.status().as_u16() {
        404 => return Err("File not found".into()),
        429 => return Err("Rate limit exceeded. Please try again later.".into()),
        403 => return Err("Access denied. File may be private or deleted.".into()),
        s if s >= 400 => return Err(format!("Network error: {s}")),
        _ => {}
    }
    let html = response.text().await.map_err(|e| e.to_string())?;
    if html.to_lowercase().contains("rate limit") {
        return Err("Rate limit exceeded. Please wait a few minutes and try again.".into());
    }
    if html.contains("File Not Found Or Deleted") {
        return Err("File not found or deleted".into());
    }
    fuckingfast_link(&html).ok_or_else(|| "Could not extract download link from page".into())
}

// ---- Rootz ----

/// `\\?"pageToken\\?"\s*:\s*\\?"([^"\\]+)`, for plain or JSON-escaped page data.
fn rootz_page_token(html: &str) -> Option<String> {
    for (at, _) in html.match_indices("pageToken") {
        let rest = &html[at + "pageToken".len()..];
        let rest = rest.strip_prefix('\\').unwrap_or(rest);
        let Some(rest) = rest.strip_prefix('"') else { continue };
        let Some(rest) = rest.trim_start().strip_prefix(':') else { continue };
        let rest = rest.trim_start();
        let rest = rest.strip_prefix('\\').unwrap_or(rest);
        let Some(rest) = rest.strip_prefix('"') else { continue };
        let token: String = rest.chars().take_while(|c| *c != '"' && *c != '\\').collect();
        if !token.is_empty() {
            return Some(token);
        }
    }
    None
}

async fn rootz(uri: &str) -> Result<String, String> {
    let url = url::Url::parse(uri).map_err(|e| e.to_string())?;
    let parts: Vec<&str> = url.path_segments().map(|s| s.filter(|p| !p.is_empty()).collect()).unwrap_or_default();
    let (Some(&"d"), Some(id)) = (parts.first(), parts.get(1)) else { return Err("Invalid rootz URL format".into()) };
    let page_url = format!("{ROOTZ_URL}/d/{id}");
    let http = client(Duration::from_secs(30))?;
    let request_error = |status: u16, body: &Value| -> String {
        body["error"].as_str().map(str::to_string).unwrap_or_else(|| if status == 404 { "File not found".into() } else { format!("Request failed with status code {status}") })
    };
    let page = http.get(&page_url).header("user-agent", CHROME_USER_AGENT).header("accept", "text/html").send().await.map_err(|e| e.to_string())?;
    let status = page.status().as_u16();
    let html = page.text().await.map_err(|e| e.to_string())?;
    if status >= 400 {
        return Err(request_error(status, &serde_json::from_str(&html).unwrap_or(Value::Null)));
    }
    let token = rootz_page_token(&html).ok_or("Rootz page token not found")?;
    let api = format!("{ROOTZ_URL}/api/files/download-by-short?shortId={}", utf8_percent_encode(id, COMPONENT));
    let response = http
        .get(api)
        .header("user-agent", CHROME_USER_AGENT)
        .header("accept", "application/json")
        .header("referer", &page_url)
        .header("x-page-token", token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if status >= 400 {
        return Err(request_error(status, &body));
    }
    let data = &body["data"];
    let Some(file_id) = data["fileId"].as_str().filter(|_| body["success"] == true) else {
        return Err("Failed to get download URL from rootz API".into());
    };
    if let Some(state) = data["status"].as_str().filter(|s| *s != "active") {
        return Err(format!("Rootz file is {state}"));
    }
    if data["downloadAllowed"] == false {
        return Err("Rootz download is not allowed".into());
    }
    if data["passwordProtected"] == true {
        return Err("Rootz file is password protected".into());
    }
    Ok(format!("{ROOTZ_URL}/api/files/proxy-download/{file_id}"))
}

// ---- archive.org (src/shared/archive-org.ts) ----

fn is_archive_identifier(id: &str) -> bool {
    let bytes = id.as_bytes();
    (2..=100).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..].iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `resolveArchiveOrgFile`: (download URL, file name) for a file page or download link.
pub fn archive_org(uri: &str) -> Option<(String, String)> {
    let url = url::Url::parse(uri.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.path().ends_with('/') {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    let id_index = if host == "archive.org" || host == "www.archive.org" {
        (segments.first() == Some(&"download")).then_some(1)?
    } else if host.ends_with(".archive.org") {
        segments.iter().position(|s| *s == "items")? + 1
    } else {
        return None;
    };
    let identifier = *segments.get(id_index)?;
    if !is_archive_identifier(identifier) {
        return None;
    }
    let parts: Vec<String> = segments[id_index + 1..]
        .iter()
        .map(|s| percent_encoding::percent_decode_str(s).decode_utf8().map(|d| d.into_owned()).unwrap_or_else(|_| s.to_string()))
        .filter(|s| s != "." && s != "..")
        .collect();
    let path = parts.join("/");
    let extension = path.rsplit_once('.').map(|(_, e)| e)?;
    if extension.is_empty() || extension.len() > 10 || !extension.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let encoded: Vec<String> = path.split('/').map(|s| utf8_percent_encode(s, COMPONENT).to_string()).collect();
    let filename = path.rsplit('/').next().unwrap_or(&path).to_string();
    Some((format!("https://archive.org/download/{}/{}", utf8_percent_encode(identifier, COMPONENT), encoded.join("/")), filename))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_hints_follow_resolve_filename() {
        assert_eq!(filename_hint("https://host/f/abc#Game.part1.rar", "https://cdn/x").as_deref(), Some("Game.part1.rar"));
        assert_eq!(filename_hint("https://host/Game%20Setup.zip", "https://cdn/token").as_deref(), Some("Game Setup.zip"));
        assert_eq!(filename_hint("https://host/f/abc", "https://cdn/files/a.7z?x=1").as_deref(), Some("a.7z"));
        assert_eq!(filename_hint("https://host/f/abc#k=v", "https://cdn/abc"), None);
    }

    #[test]
    fn mediafire_links() {
        assert_eq!(mediafire_page("abc123"), "https://mediafire.com/?abc123");
        assert_eq!(mediafire_page("http://www.mediafire.com/file/x"), "https://www.mediafire.com/file/x");
        let html = r#"<a href="https://download1234.mediafire.com/k/file.zip">x</a><a href='//www.mediafire.com/file/abc/f.zip?dkey=zz'>"#;
        assert_eq!(mediafire_link(html).as_deref(), Some("https://www.mediafire.com/file/abc/f.zip?dkey=zz"));
        assert_eq!(mediafire_link(r#"<a href="https://download99.mediafire.com/k/f.zip">"#).as_deref(), Some("https://download99.mediafire.com/k/f.zip"));
        assert_eq!(mediafire_link("<p>nothing</p>"), None);
    }

    #[test]
    fn page_scrapers() {
        assert_eq!(fuckingfast_link(r#"x window.open("https://fuckingfast.co/dl/abc?x=1") y"#).as_deref(), Some("https://fuckingfast.co/dl/abc?x=1"));
        assert_eq!(rootz_page_token(r#"{\"pageToken\":\"tok123\"}"#).as_deref(), Some("tok123"));
        assert_eq!(rootz_page_token(r#""pageToken" : "abc""#).as_deref(), Some("abc"));
        assert_eq!(gofile_id("gofile.io/d/AbC12?password=x").as_deref(), Some("AbC12"));
    }

    #[test]
    fn archive_org_urls() {
        assert_eq!(
            archive_org("https://archive.org/download/my-item/Folder/Game%20Disc.iso"),
            Some(("https://archive.org/download/my-item/Folder/Game%20Disc.iso".into(), "Game Disc.iso".into()))
        );
        assert!(archive_org("https://ia800.us.archive.org/1/items/my-item/game.zip").is_some());
        assert_eq!(archive_org("https://archive.org/details/my-item"), None);
        assert_eq!(archive_org("https://archive.org/download/my-item/folder/"), None);
        assert_eq!(archive_org("https://example.com/download/my-item/a.zip"), None);
    }
}
