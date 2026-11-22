//! Direct links for file hosters, ported from src/main/services/hosters/, src/shared/archive-org.ts,
//! download-filename.ts and the per-hoster option builders in download-manager.ts.

use std::time::Duration;

use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde_json::Value;
use tauri::webview::{DownloadEvent, NewWindowResponse};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use crate::http_download::{self, USER_AGENT};

pub const GOFILE: i64 = 2;
pub const PIXELDRAIN: i64 = 3;
pub const DATANODES: i64 = 4;
pub const MEDIAFIRE: i64 = 5;
pub const FUCKINGFAST: i64 = 9;
pub const VIKINGFILE: i64 = 10;
pub const ROOTZ: i64 = 11;
pub const ARCHIVE_ORG: i64 = 14;
pub const FILEKEEPER: i64 = 16;

/// Hosters that fetch every part of a multi-part mirror in order (`MULTIPART_DOWNLOADERS`).
pub const MULTIPART: [i64; 8] = [GOFILE, PIXELDRAIN, DATANODES, MEDIAFIRE, FUCKINGFAST, VIKINGFILE, ROOTZ, FILEKEEPER];

const LOGIN_REQUIRED: &str = "download_error_hoster_unlock_login_required";
const FILE_NOT_FOUND: &str = "download_error_hoster_unlock_file_not_found";
const UNAVAILABLE: &str = "download_error_hoster_unlock_unavailable";
const MULTIPART_INVALID: &str = "download_error_multipart_invalid";
pub const MULTIPART_NO_NAME: &str = "download_error_multipart_no_name";
const FUCKINGFAST_TIMEOUT_ERROR: &str = "download_error_fuckingfast_timeout";
const DATANODES_CLOSED: &str = "download_error_datanodes_closed";
const DATANODES_LABEL: &str = "datanodes";
/// Datanodes' pop-under ad swallows every click on "Continue to Download" until its pop-up opens.
/// Each pop-up gets a hidden window that never loads the ad, so the click goes through while
/// `window.open` stays native: Cloudflare's Turnstile check fails when it is replaced.
const DATANODES_POPUP_PREFIX: &str = "datanodes-popup-";
const ARCHIVE_ORG_INVALID: &str = "download_error_archive_org_invalid_file_url";
const CHROME_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
const GOFILE_MIRROR: &str = "https://gofilecdn.eu.cc";
const ROOTZ_URL: &str = "https://www.rootz.so";

/// `encodeURIComponent`'s set.
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'!').remove(b'~').remove(b'*').remove(b'\'').remove(b'(').remove(b')');

/// Downloaders this module resolves to a plain HTTP file.
pub fn is_hoster(downloader: i64) -> bool {
    matches!(downloader, GOFILE | PIXELDRAIN | DATANODES | MEDIAFIRE | FUCKINGFAST | VIKINGFILE | ROOTZ | ARCHIVE_ORG | FILEKEEPER)
}

/// `getDownloadParts`: the ordered part links of a multi-part download, None for a single link.
pub fn download_parts(uri: &str, parts: &Value) -> Result<Option<Vec<String>>, String> {
    if parts.is_null() {
        return Ok(None);
    }
    let invalid = || MULTIPART_INVALID.to_string();
    let parts: Vec<String> = parts.as_array().ok_or_else(invalid)?.iter().map(|p| p.as_str().filter(|s| !s.is_empty()).map(str::to_string)).collect::<Option<_>>().ok_or_else(invalid)?;
    let unique: std::collections::HashSet<&String> = parts.iter().collect();
    if parts.len() < 2 || parts[0] != uri || unique.len() != parts.len() {
        return Err(invalid());
    }
    Ok(Some(parts))
}

/// `getMultipartFolderName`: the folder for every part, named after the first part's file.
pub fn multipart_folder(first_part: &str) -> String {
    let file = first_part.rsplit(['/', '\\']).next().unwrap_or_default();
    let base = first_volume_base(file).unwrap_or_else(|| file.rfind('.').map_or(file, |end| &file[..end]));
    let trimmed = base.trim_end_matches(|c: char| c.is_whitespace() || "._-".contains(c));
    if trimmed.is_empty() { "download".into() } else { trimmed.to_string() }
}

/// `name.part01.rar` or `name.7z.001` (any zero padding, any case): the `name` before it.
fn first_volume_base(file: &str) -> Option<&str> {
    let lower = file.to_ascii_lowercase();
    let is_one = |digits: &str| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) && digits.trim_start_matches('0') == "1";
    if let Some(stem) = lower.strip_suffix(".rar") {
        let at = stem.rfind(".part")?;
        return is_one(&stem[at + 5..]).then(|| &file[..at]);
    }
    let dot = lower.rfind('.')?;
    if !is_one(&lower[dot + 1..]) {
        return None;
    }
    let stem = &lower[..dot];
    [".7z", ".zip", ".rar"].iter().find(|ext| stem.ends_with(*ext)).map(|ext| &file[..dot - ext.len()])
}

/// `estimateMultipartTotalBytes`: split archives share one volume size, so an unknown part counts
/// as the largest known one. Only the last part is smaller.
pub fn estimate_parts_total(sizes: &[Option<u64>], fallback: u64) -> u64 {
    let known: Vec<u64> = sizes.iter().flatten().copied().filter(|&s| s > 0).collect();
    match known.iter().max() {
        None => fallback,
        Some(&largest) => known.iter().sum::<u64>() + (sizes.len() - known.len()) as u64 * largest,
    }
}

#[derive(Debug, Clone)]
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
pub async fn resolve(app: &AppHandle, downloader: i64, uri: &str) -> Result<Resolved, String> {
    let url = match downloader {
        GOFILE => gofile(uri).await?,
        PIXELDRAIN => pixeldrain(uri).await?,
        DATANODES => datanodes(app, uri).await?,
        // Unlocks through the Hydra API with a signed-in account; Chimaera has no sign-in.
        VIKINGFILE => return Err(LOGIN_REQUIRED.into()),
        MEDIAFIRE => mediafire(uri).await?,
        FUCKINGFAST => fuckingfast(app, uri).await?,
        ROOTZ => rootz(uri).await?,
        FILEKEEPER => filekeeper(uri).await?,
        ARCHIVE_ORG => {
            let (url, filename) = archive_org(uri).ok_or(ARCHIVE_ORG_INVALID)?;
            return Ok(Resolved { url, headers: vec![], filename: Some(sanitize(&filename)) });
        }
        other => return Err(format!("Unsupported download host: {other}")),
    };
    Ok(resolved(uri, url))
}

fn resolved(uri: &str, url: String) -> Resolved {
    Resolved { filename: filename_hint(uri, &url).map(|f| sanitize(&f)), url, headers: vec![] }
}

/// The parts of a multi-part mirror to resolve before it starts: all of them for Datanodes, whose
/// per-file checks then come back to back in one window instead of between part downloads; only
/// the first for other hosters, whose later parts resolve on their own when their turn comes.
/// May return fewer than `uris` (never none); the rest then resolve in turn.
pub async fn resolve_upfront(app: &AppHandle, downloader: i64, uris: &[String]) -> Result<Vec<Resolved>, String> {
    if downloader == DATANODES {
        let urls = datanodes_all(app, uris).await?;
        return Ok(uris.iter().zip(urls).map(|(uri, url)| resolved(uri, url)).collect());
    }
    let first = uris.first().ok_or(MULTIPART_INVALID)?;
    Ok(vec![resolve(app, downloader, first).await?])
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

/// Destroys every window whose label starts with `prefix` when dropped, so a resolve the batch
/// drops mid-way (a pause) does not leave its hoster window open.
struct CloseWindows {
    app: AppHandle,
    prefix: &'static str,
}

impl Drop for CloseWindows {
    fn drop(&mut self) {
        for (label, window) in self.app.webview_windows() {
            if label.starts_with(self.prefix) {
                let _ = window.destroy();
            }
        }
    }
}

// ---- Datanodes ----

/// Pages the Datanodes window may load: the site and its Cloudflare Turnstile check.
fn is_datanodes_page(url: &url::Url) -> bool {
    matches!(url.scheme(), "about" | "blob" | "data")
        || matches!(url.host_str(), Some("datanodes.to" | "www.datanodes.to" | "challenges.cloudflare.com"))
}

async fn datanodes(app: &AppHandle, uri: &str) -> Result<String, String> {
    datanodes_all(app, &[uri.to_string()]).await.map(|mut urls| urls.remove(0))
}

fn datanodes_title(index: usize, total: usize) -> String {
    if total == 1 { "Datanodes".into() } else { format!("Datanodes (part {} of {total})", index + 1) }
}

/// Datanodes gates free downloads behind a Cloudflare Turnstile check, which only a person can pass,
/// and its token is single use, so every file needs its own check. Each page opens in turn in one
/// window; the file link it then navigates to (or downloads) is caught, checked to answer with a
/// file, and the window moves on to the next page, closing after the last. Returns the links of the
/// pages passed in order, which may be fewer than asked if the window closes or a check times out.
async fn datanodes_all(app: &AppHandle, uris: &[String]) -> Result<Vec<String>, String> {
    let pages = uris.iter().map(|uri| uri.parse::<url::Url>().map_err(|_| format!("Invalid datanodes URL: {uri}"))).collect::<Result<Vec<_>, _>>()?;
    let page = pages.first().cloned().ok_or(MULTIPART_INVALID)?;
    if let Some(old) = app.get_webview_window(DATANODES_LABEL) {
        let _ = old.destroy();
    }
    // None means the window closed.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Option<String>>();
    let (nav_tx, download_tx) = (tx.clone(), tx.clone());
    let popup_app = app.clone();
    let mut builder = WebviewWindowBuilder::new(app, DATANODES_LABEL, WebviewUrl::External(page))
        .title(datanodes_title(0, pages.len()))
        // Under the site's 768px breakpoint the verification and download button stack in one column,
        // so they stay visible in a narrow (tiled) window.
        .inner_size(600.0, 800.0)
        .on_new_window(move |_, features| popup_sink(&popup_app, features))
        .on_navigation(move |url| {
            if is_datanodes_page(url) {
                return true;
            }
            let _ = nav_tx.send(Some(url.to_string()));
            false
        })
        .on_download(move |_, event| {
            if let DownloadEvent::Requested { url, .. } = event {
                let _ = download_tx.send(Some(url.to_string()));
            }
            false
        });
    if let Some(main) = app.get_webview_window("main") {
        builder = builder.parent(&main).map_err(|e| e.to_string())?;
    }
    let window = builder.build().map_err(|e| e.to_string())?;
    // DATANODES_POPUP_PREFIX starts with DATANODES_LABEL, so the pop-ups close too.
    let _close = CloseWindows { app: app.clone(), prefix: DATANODES_LABEL };
    window.on_window_event(move |event| {
        if matches!(event, WindowEvent::Destroyed) {
            let _ = tx.send(None);
        }
    });
    let mut found = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        if index > 0 {
            close_datanodes_popups(app);
            let _ = window.set_title(&datanodes_title(index, pages.len()));
            window.navigate(page.clone()).map_err(|e| e.to_string())?;
        }
        let url = tokio::time::timeout(Duration::from_secs(600), async {
            // An ad redirect answers with a web page, so only a link that serves a file counts.
            while let Some(Some(url)) = rx.recv().await {
                // A late second catch of an earlier page's link (navigation, then download) is no new part.
                if found.contains(&url) {
                    continue;
                }
                // Out of the way while the link is checked; it comes back if the link is no file.
                let _ = window.hide();
                match http_download::preflight(&url, &[]).await {
                    Ok(()) => return Some(url),
                    Err(e) => {
                        eprintln!("[datanodes] caught link is not a file: {e}");
                        let _ = window.show();
                    }
                }
            }
            None
        })
        .await
        .ok()
        .flatten();
        match url {
            Some(url) => found.push(url),
            None => break,
        }
    }
    let _ = window.destroy();
    close_datanodes_popups(app);
    // The checks passed so far are kept; the parts after them resolve on their own when their turn comes.
    if found.is_empty() {
        Err(DATANODES_CLOSED.into())
    } else {
        Ok(found)
    }
}

fn close_datanodes_popups(app: &AppHandle) {
    for (label, popup) in app.webview_windows() {
        if label.starts_with(DATANODES_POPUP_PREFIX) {
            let _ = popup.destroy();
        }
    }
}

/// A hidden window for a Datanodes pop-up: the opener gets a real window, the ad never loads.
fn popup_sink(app: &AppHandle, features: tauri::webview::NewWindowFeatures) -> NewWindowResponse<tauri::Wry> {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let label = format!("{DATANODES_POPUP_PREFIX}{}", NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let blank = WebviewUrl::External("about:blank".parse().expect("valid URL"));
    match WebviewWindowBuilder::new(app, label, blank)
        .window_features(features)
        .visible(false)
        .focused(false)
        .skip_taskbar(true)
        .on_navigation(|url| url.scheme() == "about")
        .build()
    {
        Ok(window) => NewWindowResponse::Create { window },
        Err(_) => NewWindowResponse::Deny,
    }
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

const FUCKINGFAST_LABEL: &str = "fuckingfast";
const FUCKINGFAST_TIMEOUT: Duration = Duration::from_secs(45);
/// The page script reports here; the window never loads it.
const FUCKINGFAST_REPORT: &str = "chimaera.invalid";
/// Waits for the page's download button, posts it like the site does (htmx) and reports the
/// HX-Redirect file link, or why there is none, by navigating to FUCKINGFAST_REPORT.
const FUCKINGFAST_SCRIPT: &str = r#"(() => {
  if (!/(^|\.)fuckingfast\.(co|net)$/.test(location.hostname) || window.__chimaeraFF) return;
  window.__chimaeraFF = true;
  const id = location.pathname.split("/").filter(Boolean)[0];
  if (!id) return;
  const go = "/f/" + encodeURIComponent(id) + "/go";
  const report = (query) => { location.href = "https://chimaera.invalid/ff?" + query; };
  const tick = async () => {
    if (document.querySelector('[hx-post="' + go + '"]')) {
      try {
        const r = await fetch(go, { method: "POST", headers: { "HX-Request": "true", "HX-Current-URL": location.href } });
        const link = r.headers.get("HX-Redirect");
        report(link ? "link=" + encodeURIComponent(new URL(link, location.href).href) : "status=" + r.status);
      } catch (e) { report("status=0"); }
      return;
    }
    const text = document.body ? document.body.innerText : "";
    if (text.includes("File Not Found Or Deleted")) return report("state=not-found");
    if (/rate limit/i.test(text)) return report("state=rate-limited");
    setTimeout(tick, 500);
  };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", tick); else tick();
})();"#;

fn is_fuckingfast_host(url: &url::Url) -> bool {
    url.scheme() == "https" && url.host_str().is_some_and(|h| ["fuckingfast.co", "fuckingfast.net"].iter().any(|d| h == *d || h.ends_with(&format!(".{d}"))))
}

/// Pages the FuckingFast window may load: the site and its Cloudflare check.
fn is_fuckingfast_page(url: &url::Url) -> bool {
    matches!(url.scheme(), "about" | "blob" | "data") || is_fuckingfast_host(url) || url.host_str() == Some("challenges.cloudflare.com")
}

/// What the page script reported: the file link, or the error to show.
fn fuckingfast_report(url: &url::Url) -> Result<String, String> {
    let query: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
    if let Some(link) = query.get("link") {
        return match url::Url::parse(link) {
            Ok(link) if is_fuckingfast_host(&link) => Ok(link.to_string()),
            _ => Err("Could not extract download link from page".into()),
        };
    }
    Err(match query.get("state").map(String::as_str) {
        Some("not-found") => "File not found or deleted".into(),
        Some("rate-limited") => "Rate limit exceeded. Please wait a few minutes and try again.".into(),
        _ if query.get("status").map(String::as_str) == Some("429") => "Rate limit exceeded. Please wait a few minutes and try again.".into(),
        _ => format!("Could not extract download link from page (HTTP {})", query.get("status").map_or("?", String::as_str)),
    })
}

/// FuckingFast pages sit behind a Cloudflare check that only a browser passes, so a hidden window
/// opens the page and asks for the link; the file itself downloads without it. One link at a time.
async fn fuckingfast(app: &AppHandle, uri: &str) -> Result<String, String> {
    static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let mut page: url::Url = uri.parse().map_err(|_| "Invalid FuckingFast URL".to_string())?;
    if !is_fuckingfast_host(&page) {
        return Err("Unsupported domain. Supported domains: fuckingfast.co, fuckingfast.net".into());
    }
    page.set_fragment(None);
    let _turn = ONE_AT_A_TIME.lock().await;
    if let Some(old) = app.get_webview_window(FUCKINGFAST_LABEL) {
        let _ = old.destroy();
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Result<String, String>>();
    let window = WebviewWindowBuilder::new(app, FUCKINGFAST_LABEL, WebviewUrl::External(page))
        .title("FuckingFast")
        .visible(false)
        .focused(false)
        .skip_taskbar(true)
        .initialization_script(FUCKINGFAST_SCRIPT)
        .on_new_window(|_, _| NewWindowResponse::Deny)
        .on_navigation(move |url| {
            if url.host_str() == Some(FUCKINGFAST_REPORT) {
                let _ = tx.send(fuckingfast_report(url));
                return false;
            }
            is_fuckingfast_page(url)
        })
        .build()
        .map_err(|e| e.to_string())?;
    let _close = CloseWindows { app: app.clone(), prefix: FUCKINGFAST_LABEL };
    let result = tokio::time::timeout(FUCKINGFAST_TIMEOUT, rx.recv()).await;
    let _ = window.destroy();
    match result {
        Ok(Some(result)) => result,
        _ => Err(FUCKINGFAST_TIMEOUT_ERROR.into()),
    }
}

// ---- FileKeeper ----

const FILEKEEPER_MAX_REDIRECTS: usize = 5;
const FILEKEEPER_MAX_COUNTDOWN_SECS: u64 = 30;

/// `data-<name>="..."` on the FileKeeper page, with HTML entities decoded.
fn data_attr(html: &str, name: &str) -> Option<String> {
    let key = format!("data-{name}=\"");
    let start = html.find(&key)? + key.len();
    let raw = &html[start..start + html[start..].find('"')?];
    Some(raw.replace("&quot;", "\"").replace("&#39;", "'").replace("&#x27;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&"))
}

/// filekeeper.net or one of its subdomains, over https.
fn is_filekeeper(url: &url::Url) -> bool {
    url.scheme() == "https" && url.host_str().is_some_and(|h| h == "filekeeper.net" || h.ends_with(".filekeeper.net"))
}

/// One request that follows FileKeeper's own redirects, keeping its cookies. Returns the response
/// and its URL, plus true when a redirect left FileKeeper: that URL is the file.
async fn filekeeper_request(client: &reqwest::Client, mut url: String, cookies: &mut Vec<(String, String)>, mut form: Option<Vec<(&str, String)>>) -> Result<(reqwest::Response, String, bool), String> {
    for _ in 0..=FILEKEEPER_MAX_REDIRECTS {
        let cookie = cookies.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ");
        let mut request = match &form {
            Some(fields) => client.post(&url).header("referer", &url).form(fields),
            None => client.get(&url),
        };
        request = request.header("user-agent", USER_AGENT);
        if !cookie.is_empty() {
            request = request.header("cookie", cookie);
        }
        let response = request.send().await.map_err(|e| e.to_string())?;
        for set in response.headers().get_all("set-cookie").iter().filter_map(|v| v.to_str().ok()) {
            if let Some((name, value)) = set.split(';').next().and_then(|pair| pair.split_once('=')) {
                let name = name.trim();
                cookies.retain(|(k, _)| k != name);
                cookies.push((name.trim().to_string(), value.trim().to_string()));
            }
        }
        let Some(location) = response.headers().get("location").and_then(|v| v.to_str().ok()) else {
            return Ok((response, url, false));
        };
        let next = url::Url::parse(&url).and_then(|base| base.join(location)).map_err(|e| e.to_string())?;
        url = next.to_string();
        if !is_filekeeper(&next) {
            return Ok((response, url, true));
        }
        // The site moves the file code into a cookie and redirects with a GET.
        form = None;
    }
    Err("FileKeeper redirected too many times".into())
}

/// FileKeeper runs XFileSharing: the file page shows a short countdown, then a form post
/// redirects to the file on its download host.
async fn filekeeper(uri: &str) -> Result<String, String> {
    let page = url::Url::parse(uri).map_err(|_| "Invalid FileKeeper URL".to_string())?;
    if !is_filekeeper(&page) {
        return Err("Invalid FileKeeper URL".into());
    }
    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).redirect(reqwest::redirect::Policy::none()).build().map_err(|e| e.to_string())?;
    let mut cookies = Vec::new();
    let (response, page_url, left) = filekeeper_request(&client, uri.to_string(), &mut cookies, None).await?;
    let status = response.status().as_u16();
    if status == 404 {
        return Err("File not found or deleted".into());
    }
    if !response.status().is_success() || left {
        return Err(format!("FileKeeper returned HTTP {status}"));
    }
    let html = response.text().await.map_err(|e| e.to_string())?;
    let code = data_attr(&html, "code").filter(|c| !c.is_empty()).ok_or("Could not find the FileKeeper download form")?;
    if data_attr(&html, "has-password").as_deref() == Some("true") {
        return Err("FileKeeper file is password protected".into());
    }
    if data_attr(&html, "has-captcha").as_deref() == Some("true") {
        return Err("FileKeeper requires a captcha for this file".into());
    }
    // Honor the page's countdown in case the server starts enforcing it.
    let countdown = data_attr(&html, "countdown").and_then(|c| c.parse::<u64>().ok()).unwrap_or(0).min(FILEKEEPER_MAX_COUNTDOWN_SECS);
    tokio::time::sleep(Duration::from_secs(countdown)).await;
    let method = data_attr(&html, "method").filter(|m| !m.is_empty()).unwrap_or_else(|| "Free download".into());
    let form = vec![
        ("op", "download2".to_string()),
        ("id", code),
        ("rand", data_attr(&html, "rand").unwrap_or_default()),
        ("referer", data_attr(&html, "referer").unwrap_or_default()),
        ("method_free", method),
        ("down_direct", "1".to_string()),
    ];
    let (_, file_url, left) = filekeeper_request(&client, page_url, &mut cookies, Some(form)).await?;
    if !left {
        return Err("FileKeeper did not return a download link".into());
    }
    Ok(file_url)
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
    fn datanodes_window_keeps_only_site_and_turnstile_pages() {
        let page = |u: &str| is_datanodes_page(&u.parse().unwrap());
        assert!(page("https://datanodes.to/download"));
        assert!(page("https://challenges.cloudflare.com/cdn-cgi/challenge-platform/x"));
        assert!(page("about:blank"));
        assert!(!page("https://tunnel5.dlproxy.uk/download/abc"));
        assert!(!page("https://rwbad2.xyz/pop"));
    }

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
    fn multipart_parts_must_be_ordered_unique_links_starting_at_uri() {
        let parts = |v: Value| download_parts("a", &v);
        assert_eq!(parts(Value::Null), Ok(None));
        assert_eq!(parts(serde_json::json!(["a", "b"])), Ok(Some(vec!["a".to_string(), "b".to_string()])));
        assert!(parts(serde_json::json!(["a"])).is_err());
        assert!(parts(serde_json::json!(["b", "a"])).is_err());
        assert!(parts(serde_json::json!(["a", "a"])).is_err());
        assert!(parts(serde_json::json!(["a", ""])).is_err());
        assert!(parts(serde_json::json!(["a", 3])).is_err());
        assert!(parts(serde_json::json!("a")).is_err());
    }

    #[test]
    fn multipart_folder_follows_the_first_volume() {
        assert_eq!(multipart_folder("Game.part01.rar"), "Game");
        assert_eq!(multipart_folder("Game_v1.2.PART001.RAR"), "Game_v1.2");
        assert_eq!(multipart_folder("dir/Game.part1.rar"), "Game");
        assert_eq!(multipart_folder("Game.7z.001"), "Game");
        assert_eq!(multipart_folder("Game.zip.01"), "Game");
        assert_eq!(multipart_folder("Game - .part02.rar"), "Game - .part02");
        assert_eq!(multipart_folder("Setup-Game.bin"), "Setup-Game");
        assert_eq!(multipart_folder(".part1.rar"), "download");
    }

    #[test]
    fn multipart_total_counts_unknown_parts_as_the_largest() {
        assert_eq!(estimate_parts_total(&[Some(10), Some(4)], 99), 14);
        assert_eq!(estimate_parts_total(&[Some(10), None, None], 99), 30);
        assert_eq!(estimate_parts_total(&[None, None], 99), 99);
        assert_eq!(estimate_parts_total(&[Some(0), None], 7), 7);
    }

    #[test]
    fn fuckingfast_window_and_reports() {
        let url = |u: &str| u.parse::<url::Url>().unwrap();
        assert!(is_fuckingfast_page(&url("https://fuckingfast.co/abc")));
        assert!(is_fuckingfast_page(&url("https://fuckingfast.net/abc")));
        assert!(is_fuckingfast_page(&url("https://challenges.cloudflare.com/x")));
        assert!(!is_fuckingfast_page(&url("https://ads.example/pop")));
        assert!(!is_fuckingfast_page(&url("http://fuckingfast.co/abc")));
        let link = "https://fuckingfast.co/dl/abc?t=1";
        let report = |q: &str| fuckingfast_report(&url(&format!("https://chimaera.invalid/ff?{q}")));
        assert_eq!(report(&format!("link={}", utf8_percent_encode(link, COMPONENT))), Ok(link.to_string()));
        assert!(report("link=https%3A%2F%2Fevil.example%2Fx").is_err());
        assert_eq!(report("state=not-found"), Err("File not found or deleted".into()));
        assert!(report("status=429").unwrap_err().starts_with("Rate limit"));
        assert!(report("status=500").unwrap_err().contains("HTTP 500"));
    }

    #[test]
    fn page_scrapers() {
        let html = r#"<div data-code="k3x" data-rand="r9" data-has-password="false" data-countdown="5"></div>"#;
        assert_eq!(data_attr(html, "code").as_deref(), Some("k3x"));
        assert_eq!(data_attr(html, "countdown").as_deref(), Some("5"));
        assert_eq!(data_attr(html, "referer"), None);
        assert_eq!(data_attr(r#"data-referer="https://s/?a=1&amp;b=&quot;2&quot;""#, "referer").as_deref(), Some("https://s/?a=1&b=\"2\""));
        assert!(is_filekeeper(&"https://www.filekeeper.net/x".parse().unwrap()));
        assert!(!is_filekeeper(&"https://filekeeper.net.evil.example/x".parse().unwrap()));
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

#[cfg(test)]
mod live {
    /// `CHIMAERA_LIVE_FILEKEEPER=<file page> cargo test --lib live_filekeeper -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_filekeeper() {
        let uri = std::env::var("CHIMAERA_LIVE_FILEKEEPER").expect("set CHIMAERA_LIVE_FILEKEEPER");
        let url = super::filekeeper(&uri).await.expect("resolves");
        println!("file link host: {:?}", url::Url::parse(&url).unwrap().host_str());
        crate::http_download::preflight(&url, &[]).await.expect("serves a file");
        println!("name: {:?}", crate::http_download::probe_filename(&url, &[]).await.or_else(|| super::filename_hint(&uri, &url)));
    }
}
