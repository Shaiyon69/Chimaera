//! Debrid services, ported from src/main/services/download/real-debrid*.ts, torbox*.ts,
//! all-debrid.ts, premiumize.ts, debrid-files.ts and the batch builders in download-manager.ts.
//!
//! Core scope (decided 2026-10-31): sign-in check, file lists, and downloads of what the service
//! already has. A torrent the service has not finished yet fails with its "not ready" error
//! instead of Electron's awaiting-debrid state that keeps polling in the background.
//! Tokens are read from `userPreferences` on each use, so a changed token applies at once.

use std::collections::HashSet;
use std::time::Duration;

use serde_json::{json, Value};
use unicode_normalization::UnicodeNormalization;

use crate::hosters;
use crate::store::Store;

pub const REAL_DEBRID: i64 = 0;
pub const TORBOX: i64 = 6;
pub const PREMIUMIZE: i64 = 12;
pub const ALL_DEBRID: i64 = 13;

const NOT_CACHED_REAL_DEBRID: &str = "download_error_not_cached_on_real_debrid";
const REAL_DEBRID_NOT_READY: &str = "download_error_real_debrid_torrent_not_ready";
const REAL_DEBRID_LINKS_NOT_READY: &str = "download_error_real_debrid_links_not_ready";
const NOT_CACHED_TORBOX: &str = "download_error_not_cached_on_torbox";
const TORBOX_NOT_READY: &str = "download_error_torbox_torrent_not_ready";
const TORBOX_LINK_UNAVAILABLE: &str = "download_error_torbox_link_unavailable";
const NOT_CACHED_PREMIUMIZE: &str = "download_error_not_cached_on_premiumize";
const PREMIUMIZE_TRANSFER_STARTED: &str = "download_error_premiumize_transfer_started";
const NOT_CACHED_ALL_DEBRID: &str = "download_error_not_cached_on_alldebrid";
const FILES_UNAVAILABLE: &str = "download_error_torrent_files_unavailable";
const INVALID_MAGNET: &str = "download_error_invalid_magnet";

const RD_API: &str = "https://api.real-debrid.com/rest/1.0";
const TORBOX_API: &str = "https://api.torbox.app/v1/api";
const AD_API: &str = "https://api.alldebrid.com";
const PM_API: &str = "https://www.premiumize.me/api";
const RD_FAILED: [&str; 4] = ["error", "dead", "virus", "magnet_error"];

pub fn is_debrid(downloader: i64) -> bool {
    matches!(downloader, REAL_DEBRID | TORBOX | PREMIUMIZE | ALL_DEBRID)
}

fn agent() -> String {
    format!("Chimaera/{}", env!("CARGO_PKG_VERSION"))
}

/// The saved API token, or the service's "account not authorized" error.
fn token(store: &Store, downloader: i64) -> Result<String, String> {
    let (key, missing) = match downloader {
        REAL_DEBRID => ("realDebridApiToken", "download_error_real_debrid_account_not_authorized"),
        TORBOX => ("torBoxApiToken", "download_error_torbox_account_not_authorized"),
        PREMIUMIZE => ("premiumizeApiToken", "download_error_premiumize_account_not_authorized"),
        _ => ("allDebridApiToken", "download_error_alldebrid_account_not_authorized"),
    };
    store.get("", "userPreferences")?[key].as_str().filter(|t| !t.trim().is_empty()).map(str::to_string).ok_or_else(|| missing.into())
}

fn client(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder().timeout(timeout).build().map_err(|e| e.to_string())
}

/// JSON body of a response; an HTTP error becomes the API's message or the status.
async fn json_of(response: reqwest::Response) -> Result<Value, String> {
    let status = response.status().as_u16();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if status >= 400 {
        let message = ["error", "message", "detail"].iter().find_map(|k| body[k].as_str().map(str::to_string));
        return Err(message.unwrap_or_else(|| format!("Request failed with status code {status}")));
    }
    Ok(body)
}

/// The info hash of a magnet as lowercase hex (parse-torrent turns base32 hashes into hex).
pub fn info_hash(magnet: &str) -> Option<String> {
    let url = url::Url::parse(magnet.trim()).ok()?;
    let hash = url.query_pairs().find_map(|(k, v)| (k == "xt").then(|| v.strip_prefix("urn:btih:").map(str::to_string)).flatten())?;
    if hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(hash.to_ascii_lowercase());
    }
    if hash.len() != 32 {
        return None;
    }
    let (mut bits, mut value, mut out) = (0u32, 0u64, String::new());
    for c in hash.to_ascii_uppercase().bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        };
        value = (value << 5) | u64::from(v);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push_str(&format!("{:02x}", (value >> bits) & 0xff));
        }
    }
    Some(out)
}

fn magnet_name(magnet: &str) -> Option<String> {
    url::Url::parse(magnet.trim()).ok()?.query_pairs().find_map(|(k, v)| (k == "dn").then(|| v.into_owned()))
}

// ---- File manifests (torbox-files.ts, real-debrid-files.ts) ----

fn decode_entities(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let decoded = tail.find(';').filter(|end| *end <= 12).and_then(|end| {
            let code = tail[1..end].to_ascii_lowercase();
            let ch = match code.as_str() {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ => {
                    let number = code.strip_prefix("#x").map(|h| u32::from_str_radix(h, 16)).or_else(|| code.strip_prefix('#').map(str::parse))?;
                    number.ok().filter(|p| *p > 0).and_then(char::from_u32)
                }
            };
            ch.map(|c| (c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `sanitizeSegment`: one safe path segment on every OS.
fn sanitize_segment(segment: &str) -> String {
    let normalized: String = decode_entities(segment).nfc().collect();
    let mapped: String = normalized.chars().map(|c| if (c as u32) < 32 || "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect();
    let trimmed = mapped.trim();
    let without_dots = trimmed.trim_end_matches('.');
    let cleaned = if without_dots.len() == trimmed.len() { trimmed.to_string() } else { format!("{without_dots}_") };
    let lower = cleaned.to_ascii_lowercase();
    let stem = lower.split('.').next().unwrap_or_default();
    let reserved = matches!(stem, "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4 && (stem.starts_with("com") || stem.starts_with("lpt")) && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        format!("_{cleaned}")
    } else {
        cleaned
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ManifestFile {
    pub id: i64,
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct Manifest {
    pub torrent_id: i64,
    pub name: String,
    pub files: Vec<ManifestFile>,
    pub archive_only: bool,
}

/// (id, raw path, size, zipped) of one provider file.
type RawFile = (i64, String, i64, bool);

/// `buildTorBoxDownloadManifest`: every path becomes `<name>/<safe segments>`, unique on disk.
fn manifest(service: &str, torrent_id: i64, torrent_name: &str, files: &[RawFile], magnet_name: Option<&str>) -> Result<Manifest, String> {
    let split = |p: &str| -> Vec<String> { p.split(['/', '\\']).filter(|s| !s.is_empty()).map(str::to_string).collect() };
    let same = |a: &str, b: &str| sanitize_segment(a).to_lowercase() == sanitize_segment(b).to_lowercase();
    let unsafe_path = || format!("{service} returned an unsafe file path.");
    let shared_root = files.first().and_then(|f| f.1.split(['/', '\\']).next().map(str::to_string)).filter(|r| !r.is_empty());
    let has_shared_root = shared_root.as_ref().is_some_and(|root| {
        files.iter().all(|f| {
            let parts = split(&f.1);
            parts.len() > 1 && same(&parts[0], root)
        })
    });
    let provider_name = if has_shared_root { shared_root.unwrap_or_default() } else { torrent_name.to_string() };
    let opaque = |v: &str| {
        let core = v.strip_suffix(".zip").or_else(|| v.strip_suffix(".ZIP")).unwrap_or(v);
        (32..=64).contains(&core.len()) && core.bytes().all(|b| b.is_ascii_hexdigit())
    };
    let chosen = match magnet_name {
        Some(m) if opaque(&provider_name) && !opaque(m) => m.to_string(),
        _ => provider_name.clone(),
    };
    let name = sanitize_segment(&chosen);
    if name.is_empty() || name == "." || name == ".." || files.is_empty() {
        return Err(format!("{service} did not provide downloadable files."));
    }
    let (mut ids, mut paths, mut dirs) = (HashSet::new(), HashSet::new(), HashSet::new());
    let mut out = vec![];
    for (id, raw, size, _) in files {
        if *id < 0 || !ids.insert(*id) {
            return Err(format!("{service} returned an invalid file ID."));
        }
        let drive = raw.len() >= 2 && raw.as_bytes()[1] == b':' && raw.as_bytes()[0].is_ascii_alphabetic();
        if raw.is_empty() || raw.starts_with(['/', '\\']) || drive {
            return Err(unsafe_path());
        }
        let parts: Vec<&str> = raw.split(['/', '\\']).collect();
        if parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..") || *size < 0 {
            return Err(format!("{service} returned an unsafe file path or size."));
        }
        let relative = if parts.len() > 1 && same(parts[0], &provider_name) { &parts[1..] } else { &parts[..] };
        let safe: Vec<String> = relative.iter().map(|p| sanitize_segment(p)).collect();
        if safe.is_empty() || safe.iter().any(|p| p.is_empty() || p == "." || p == "..") {
            return Err(unsafe_path());
        }
        let path = std::iter::once(name.clone()).chain(safe.iter().cloned()).collect::<Vec<_>>().join("/");
        let key = path.to_lowercase();
        let dir_keys: Vec<String> = (1..safe.len()).map(|n| std::iter::once(name.clone()).chain(safe[..n].iter().cloned()).collect::<Vec<_>>().join("/").to_lowercase()).collect();
        if paths.contains(&key) || dirs.contains(&key) || dir_keys.iter().any(|d| paths.contains(d)) {
            return Err(format!("{service} returned files with the same local path."));
        }
        paths.insert(key);
        dirs.extend(dir_keys);
        out.push(ManifestFile { id: *id, path, size: *size as u64 });
    }
    Ok(Manifest { torrent_id, name, archive_only: out.len() == 1 && files[0].3, files: out })
}

/// `selectDebridFiles` / `selectTorBoxFiles`.
fn select<T: Clone>(files: &[T], id: impl Fn(&T) -> i64, indices: Option<&[i64]>) -> Result<Vec<T>, String> {
    let Some(indices) = indices else { return Ok(files.to_vec()) };
    let requested: HashSet<i64> = indices.iter().copied().collect();
    let chosen: Vec<T> = files.iter().filter(|f| requested.contains(&id(f))).cloned().collect();
    if requested.is_empty() || chosen.len() != requested.len() {
        return Err("The selected debrid files are no longer available.".into());
    }
    Ok(chosen)
}

fn files_response(name: &str, files: &[ManifestFile], archive_only: Option<bool>) -> Value {
    let mut data = json!({
        "infoHash": "",
        "name": name,
        "totalSize": files.iter().map(|f| f.size).sum::<u64>(),
        "files": files.iter().map(|f| json!({ "index": f.id, "path": f.path, "length": f.size })).collect::<Vec<_>>(),
    });
    if let Some(archive) = archive_only {
        data["archiveOnly"] = json!(archive);
    }
    data
}

/// `getDebridRootFolderName`: the folder every path shares, if there are several files.
fn root_folder(paths: &[String]) -> Option<String> {
    if paths.len() < 2 {
        return None;
    }
    let first = paths[0].split(['/', '\\']).find(|s| !s.is_empty())?;
    let ok = first != "." && first != ".." && paths.iter().all(|p| {
        let parts: Vec<&str> = p.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
        parts.len() > 1 && parts[0] == first
    });
    ok.then(|| first.to_string())
}

/// `sanitizeRelativePath`.
fn sanitize_relative(path: &str) -> String {
    path.split(['/', '\\']).filter(|s| !s.is_empty() && *s != "." && *s != "..").map(hosters::sanitize).collect::<Vec<_>>().join("/")
}

/// `getDebridOutputPath`: a lone zip needs no wrapper folder.
fn output_path(path: &str, count: usize) -> String {
    if count == 1 && path.to_lowercase().ends_with(".zip") {
        return path.rsplit('/').next().unwrap_or(path).to_string();
    }
    path.to_string()
}

// ---- Links and plans ----

/// How a file's download URL is obtained, just before it starts (links expire).
#[derive(Debug, Clone)]
pub enum Link {
    Direct(String),
    RealDebrid { token: String, link: String, expected_name: String, expected_size: u64 },
    TorBox { token: String, torrent_id: i64, file_id: i64 },
    AllDebrid { token: String, link: String },
    /// One part of a multi-part hoster mirror, resolved when its turn comes. `prepared` is the
    /// first part's link, already resolved while the download started. A part without a path
    /// takes the file name its link gives, inside `folder`.
    Hoster { downloader: i64, uri: String, folder: String, prepared: Option<crate::hosters::Resolved> },
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub link: Link,
    /// Path under the download folder; None lets the server name the file.
    pub path: Option<String>,
    pub size: Option<u64>,
    /// The file must have exactly `size` bytes (Real-Debrid and TorBox, not archives).
    pub exact_size: bool,
    pub resource_id: String,
    pub headers: Vec<(String, String)>,
}

pub struct Plan {
    pub entries: Vec<Entry>,
    /// The folder the files land in, shown as the download's folder name.
    pub folder: Option<String>,
}

fn indices(download: &Value) -> Option<Vec<i64>> {
    download["fileIndices"].as_array().filter(|a| !a.is_empty()).map(|a| a.iter().filter_map(Value::as_i64).collect())
}

/// The files of a debrid download, ready for the batch downloader.
pub async fn plan(store: &Store, download: &Value) -> Result<Plan, String> {
    let downloader = download["downloader"].as_i64().unwrap_or(-1);
    let uri = download["uri"].as_str().unwrap_or_default();
    let token = token(store, downloader)?;
    let selection = indices(download);
    match downloader {
        REAL_DEBRID => real_debrid_plan(&token, uri, selection.as_deref()).await,
        TORBOX => torbox_plan(&token, uri, selection.as_deref()).await,
        ALL_DEBRID => all_debrid_plan(&token, uri).await,
        _ => {
            let url = premiumize_url(&token, uri).await?.ok_or(NOT_CACHED_PREMIUMIZE)?;
            Ok(single(url, uri))
        }
    }
}

/// One file named like `resolveFilename` names it.
fn single(url: String, uri: &str) -> Plan {
    let path = hosters::filename_hint(uri, &url).map(|f| hosters::sanitize(&f));
    Plan { entries: vec![Entry { link: Link::Direct(url), path, size: None, exact_size: false, resource_id: uri.to_string(), headers: vec![] }], folder: None }
}

/// Builds batch entries the way the `prepare*Batch` helpers do.
fn batch(provider: &str, uri: &str, files: Vec<(Link, String, Option<u64>, String, bool)>) -> Plan {
    let count = files.len();
    let paths: Vec<String> = files.iter().map(|f| sanitize_relative(&f.1)).collect();
    let folder = root_folder(&paths);
    let entries = files
        .into_iter()
        .map(|(link, path, size, identity, exact)| Entry {
            link,
            path: Some(sanitize_relative(&output_path(&path, count))),
            size,
            exact_size: exact,
            resource_id: format!("{provider}:{uri}#{identity}"),
            headers: vec![],
        })
        .collect();
    Plan { entries, folder }
}

/// The download URL of `link`.
pub async fn unlock(link: &Link) -> Result<String, String> {
    match link {
        Link::Direct(url) => Ok(url.clone()),
        Link::RealDebrid { token, link, expected_name, expected_size } => {
            let file = rd_unrestrict(token, link).await?;
            let base = |p: &str| p.rsplit(['/', '\\']).next().unwrap_or(p).nfc().collect::<String>().to_lowercase();
            let size = file["filesize"].as_u64().unwrap_or(0);
            let name = file["filename"].as_str().unwrap_or_default();
            if (*expected_size > 0 && size != *expected_size) || (!expected_name.is_empty() && !name.is_empty() && base(expected_name) != base(name)) {
                return Err("Real-Debrid returned a link for a different torrent file.".into());
            }
            file["download"].as_str().map(str::to_string).ok_or_else(|| "The download link is unavailable.".into())
        }
        Link::TorBox { token, torrent_id, file_id } => torbox_link(token, *torrent_id, *file_id).await,
        Link::AllDebrid { token, link } => ad_unlock(token, link).await.map(|(url, _, _)| url),
        // Resolved by the batch, which holds the app the hoster may need.
        Link::Hoster { .. } => Err("A hoster part needs the batch to resolve it.".into()),
    }
}

// ---- Real-Debrid (real-debrid.ts, real-debrid-links.ts) ----

fn rd_client() -> Result<reqwest::Client, String> {
    // Electron forces IPv4 for Real-Debrid.
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
        .build()
        .map_err(|e| e.to_string())
}

async fn rd_get(token: &str, path: &str) -> Result<Value, String> {
    json_of(rd_client()?.get(format!("{RD_API}{path}")).bearer_auth(token).send().await.map_err(|e| e.to_string())?).await
}

async fn rd_post(token: &str, path: &str, form: &[(&str, &str)]) -> Result<Value, String> {
    let response = rd_client()?.post(format!("{RD_API}{path}")).bearer_auth(token).form(form).send().await.map_err(|e| e.to_string())?;
    // selectFiles answers 204 with no body.
    if response.status().as_u16() == 204 {
        return Ok(Value::Null);
    }
    json_of(response).await
}

async fn rd_unrestrict(token: &str, link: &str) -> Result<Value, String> {
    rd_post(token, "/unrestrict/link", &[("link", link)]).await
}

/// `getTorrentId`: the user's matching torrent (a finished one first), else a new one.
async fn rd_torrent_id(token: &str, magnet: &str) -> Result<String, String> {
    let hash = info_hash(magnet).ok_or("The magnet link has no torrent hash.")?;
    let list = rd_get(token, "/torrents?limit=5000").await?;
    let mut matches: Vec<&Value> = list
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["hash"].as_str().is_some_and(|h| h.eq_ignore_ascii_case(&hash)) && !RD_FAILED.contains(&t["status"].as_str().unwrap_or_default()))
        .collect();
    matches.sort_by_key(|t| std::cmp::Reverse(t["bytes"].as_u64().unwrap_or(0)));
    if let Some(t) = matches.iter().find(|t| t["status"] == "downloaded").or(matches.first()) {
        return t["id"].as_str().map(str::to_string).ok_or_else(|| "Real-Debrid returned an invalid torrent.".into());
    }
    rd_add(token, magnet).await
}

async fn rd_add(token: &str, magnet: &str) -> Result<String, String> {
    rd_post(token, "/torrents/addMagnet", &[("magnet", magnet)]).await?["id"].as_str().map(str::to_string).ok_or_else(|| "Real-Debrid did not add the torrent.".into())
}

fn rd_failed(info: &Value) -> Result<(), String> {
    let status = info["status"].as_str().unwrap_or_default();
    if RD_FAILED.contains(&status) {
        return Err(format!("Real-Debrid torrent failed ({status})."));
    }
    Ok(())
}

/// `pollTorrentFiles`: waits up to 15 s for the file list.
async fn rd_info_with_files(token: &str, id: &str, allow_unselected: bool) -> Result<Value, String> {
    for attempt in 0..15 {
        let info = rd_get(token, &format!("/torrents/info/{id}")).await?;
        rd_failed(&info)?;
        let has_files = info["files"].as_array().is_some_and(|f| !f.is_empty());
        if has_files || (allow_unselected && info["status"] == "waiting_files_selection") {
            return Ok(info);
        }
        if attempt < 14 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    Err(REAL_DEBRID_NOT_READY.into())
}

#[derive(Debug, Clone)]
struct RdFile {
    id: i64,
    path: String,
    size: u64,
    source: String,
    selected: bool,
}

/// `getRealDebridFiles`: provider paths turned into local ones under the torrent folder.
fn rd_files(info: &Value) -> Result<Vec<RdFile>, String> {
    let name = info["original_filename"].as_str().filter(|n| !n.is_empty()).or(info["filename"].as_str()).unwrap_or_default();
    if name.contains(['/', '\\']) {
        return Err("Real-Debrid returned an unsafe torrent name.".into());
    }
    let files = info["files"].as_array().cloned().unwrap_or_default();
    let mut raw: Vec<RawFile> = vec![];
    for f in &files {
        let path = f["path"].as_str().unwrap_or_default();
        let relative = path.strip_prefix('/').unwrap_or(path);
        if relative.starts_with(['/', '\\']) {
            return Err("Real-Debrid returned an unsafe file path.".into());
        }
        let first = relative.split(['/', '\\']).next().unwrap_or_default();
        let named = if files.len() > 1 && first != name { format!("{name}/{relative}") } else { relative.to_string() };
        raw.push((f["id"].as_i64().unwrap_or(-1), named, f["bytes"].as_i64().unwrap_or(-1), false));
    }
    let built = manifest("Real-Debrid", 0, name, &raw, None)?;
    Ok(built
        .files
        .into_iter()
        .zip(&files)
        .map(|(m, f)| {
            let source = f["path"].as_str().unwrap_or_default().to_string();
            let flat = files.len() == 1 && !source.strip_prefix('/').unwrap_or(&source).contains(['/', '\\']);
            let path = if flat { m.path.split('/').skip(1).collect::<Vec<_>>().join("/") } else { m.path };
            RdFile { id: m.id, path, size: m.size, source, selected: f["selected"].as_i64().unwrap_or(0) != 0 }
        })
        .collect())
}

fn rd_has_selection(files: &[RdFile], requested: Option<&[i64]>) -> bool {
    let selected: Vec<i64> = files.iter().filter(|f| f.selected).map(|f| f.id).collect();
    let wanted: HashSet<i64> = requested.map_or_else(|| files.iter().map(|f| f.id).collect(), |r| r.iter().copied().collect());
    wanted.len() == selected.len() && selected.iter().all(|id| wanted.contains(id))
}

fn rd_archive_candidate(info: &Value, files: &[RdFile]) -> bool {
    info["status"] == "downloaded"
        && info["links"].as_array().is_some_and(|l| l.len() == 1)
        && files.iter().filter(|f| f.selected).count() >= 2
        && info["ended"].as_str().is_some_and(|e| !e.is_empty())
}

async fn real_debrid_plan(token: &str, uri: &str, selection: Option<&[i64]>) -> Result<Plan, String> {
    if !uri.starts_with("magnet:") {
        let file = rd_unrestrict(token, uri).await?;
        let url = file["download"].as_str().ok_or(NOT_CACHED_REAL_DEBRID)?.to_string();
        let mut plan = single(url, uri);
        plan.entries[0].path = file["filename"].as_str().map(hosters::sanitize).or(plan.entries[0].path.take());
        return Ok(plan);
    }
    let id = rd_torrent_id(token, uri).await?;
    let mut info = rd_info_with_files(token, &id, selection.is_none()).await?;
    if info["files"].as_array().is_none_or(Vec::is_empty) {
        // A fresh torrent waits for a selection; pick everything and let the next try find it ready.
        rd_post(token, &format!("/torrents/selectFiles/{id}"), &[("files", "all")]).await?;
        return Err(NOT_CACHED_REAL_DEBRID.into());
    }
    let files = rd_files(&info)?;
    let requested = select(&files, |f| f.id, selection)?;
    let requested_ids: Vec<i64> = requested.iter().map(|f| f.id).collect();
    let selectable = info["status"] != "waiting_files_selection" && files.iter().any(|f| f.selected);
    if selectable && !rd_has_selection(&files, Some(&requested_ids)) {
        // A torrent's selection cannot change: add it again for the new selection.
        let fresh = rd_add(token, uri).await?;
        info = rd_info_with_files(token, &fresh, false).await?;
    }
    let id = info["id"].as_str().unwrap_or(&id).to_string();
    if info["status"] == "waiting_files_selection" {
        let list = if selection.is_none() { "all".to_string() } else { requested_ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",") };
        rd_post(token, &format!("/torrents/selectFiles/{id}"), &[("files", &list)]).await?;
    }
    // `waitForRealDebridLinks`: only a finished torrent has links.
    let mut ready = None;
    for attempt in 0..10 {
        let current = rd_get(token, &format!("/torrents/info/{id}")).await?;
        rd_failed(&current)?;
        if current["status"] != "downloaded" {
            return Err(NOT_CACHED_REAL_DEBRID.into());
        }
        let current_files = rd_files(&current)?;
        let selected = current_files.iter().filter(|f| f.selected).count();
        let links = current["links"].as_array().map_or(0, Vec::len);
        if (selected > 0 && selected == links) || rd_archive_candidate(&current, &current_files) {
            ready = Some((current, current_files));
            break;
        }
        if attempt < 9 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    let (info, files) = ready.ok_or(REAL_DEBRID_LINKS_NOT_READY)?;
    if !rd_has_selection(&files, Some(&requested_ids)) {
        return Err(NOT_CACHED_REAL_DEBRID.into());
    }
    let links: Vec<String> = info["links"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
    if rd_archive_candidate(&info, &files) {
        // Real-Debrid packed the selection into one archive.
        let file = rd_unrestrict(token, &links[0]).await?;
        let name = file["filename"].as_str().unwrap_or_default();
        let lower = name.rsplit(['/', '\\']).next().unwrap_or_default().to_lowercase();
        let is_archive = [".zip", ".rar", ".7z"].iter().any(|e| lower.ends_with(e));
        if !is_archive || files.iter().any(|f| f.selected && f.source.rsplit(['/', '\\']).next().unwrap_or_default().to_lowercase() == lower) {
            return Err("Real-Debrid returned an invalid torrent archive.".into());
        }
        let selection_key = {
            let mut ids: Vec<i64> = files.iter().filter(|f| f.selected).map(|f| f.id).collect();
            ids.sort_unstable();
            ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
        };
        let hash = info["hash"].as_str().unwrap_or_default().to_lowercase();
        let url = file["download"].as_str().ok_or(NOT_CACHED_REAL_DEBRID)?.to_string();
        return Ok(Plan {
            entries: vec![Entry {
                link: Link::Direct(url),
                path: Some(hosters::sanitize(&lower_preserving(name))),
                size: file["filesize"].as_u64(),
                exact_size: false,
                resource_id: format!("realDebrid:{hash}:archive:{selection_key}"),
                headers: vec![],
            }],
            folder: None,
        });
    }
    let selected: Vec<&RdFile> = files.iter().filter(|f| f.selected).collect();
    let wanted: HashSet<i64> = requested_ids.iter().copied().collect();
    let entries = selected
        .iter()
        .zip(links)
        .filter(|(f, _)| wanted.contains(&f.id))
        .map(|(f, link)| {
            let rd = Link::RealDebrid { token: token.to_string(), link, expected_name: f.source.clone(), expected_size: f.size };
            (rd, f.path.clone(), Some(f.size), f.id.to_string(), true)
        })
        .collect();
    Ok(batch("realDebrid", uri, entries))
}

/// The base name of an archive Real-Debrid returned, case kept.
fn lower_preserving(name: &str) -> String {
    name.rsplit(['/', '\\']).next().unwrap_or(name).to_string()
}

// ---- TorBox (torbox.ts) ----

fn torbox_client() -> Result<reqwest::Client, String> {
    client(Duration::from_secs(20))
}

async fn torbox_get(token: &str, path: &str) -> Result<Value, String> {
    let response = torbox_client()?.get(format!("{TORBOX_API}{path}")).bearer_auth(token).header("user-agent", agent()).send().await.map_err(|e| e.to_string())?;
    json_of(response).await
}

fn torbox_ready(t: &Value) -> bool {
    t["download_finished"] == true && t["download_present"] == true
}

async fn torbox_info(token: &str, id: i64) -> Result<Option<Value>, String> {
    let data = torbox_get(token, &format!("/torrents/mylist?id={id}&bypass_cache=true")).await?["data"].take();
    Ok(match data {
        Value::Array(list) => list.into_iter().find(|t| t["id"].as_i64() == Some(id)),
        v @ Value::Object(_) if v["id"].as_i64() == Some(id) => Some(v),
        _ => None,
    })
}

async fn torbox_wait_ready(token: &str, id: i64) -> Result<Value, String> {
    for attempt in 1..=6 {
        if let Some(t) = torbox_info(token, id).await?.filter(torbox_ready) {
            return Ok(t);
        }
        if attempt < 6 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    Err(TORBOX_NOT_READY.into())
}

/// `createtorrent` takes multipart form data.
async fn torbox_add(token: &str, magnet: &str, cached_only: bool) -> Result<Value, String> {
    let boundary = format!("----chimaera{}", uuid::Uuid::new_v4().simple());
    let mut fields = vec![("magnet", magnet), ("allow_zip", "false")];
    if cached_only {
        fields.push(("add_only_if_cached", "true"));
    }
    let mut body = String::new();
    for (key, value) in fields {
        body.push_str(&format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{key}\"\r\n\r\n{value}\r\n"));
    }
    body.push_str(&format!("--{boundary}--\r\n"));
    let response = torbox_client()?
        .post(format!("{TORBOX_API}/torrents/createtorrent"))
        .bearer_auth(token)
        .header("user-agent", agent())
        .header("content-type", format!("multipart/form-data; boundary={boundary}"))
        .body(body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    let data: Value = response.json().await.unwrap_or(Value::Null);
    if data["success"] != true || status >= 400 {
        let detail = data["detail"].as_str().unwrap_or_default();
        let reason = format!("{} {detail}", data["error"].as_str().unwrap_or_default()).to_lowercase();
        let not_cached = ["not cached", "not_cached", "not-cached", "notcached", "uncached"].iter().any(|p| reason.contains(p));
        return Err(if cached_only && not_cached { NOT_CACHED_TORBOX.into() } else if detail.is_empty() { FILES_UNAVAILABLE.into() } else { detail.to_string() });
    }
    Ok(data["data"].clone())
}

/// `getDownloadFiles`: the user's copy of the torrent (added if missing) as a manifest.
async fn torbox_manifest(token: &str, magnet: &str, cached_only: bool) -> Result<Manifest, String> {
    let hash = info_hash(magnet).ok_or(INVALID_MAGNET)?;
    let mut mine = vec![];
    for page in 0..10 {
        let list = torbox_get(token, &format!("/torrents/mylist?bypass_cache=true&offset={}&limit=1000", page * 1000)).await?;
        let entries = list["data"].as_array().cloned().unwrap_or_default();
        let full = entries.len() == 1000;
        mine.extend(entries.into_iter().filter(|t| t["hash"].as_str().is_some_and(|h| h.eq_ignore_ascii_case(&hash))));
        if !full {
            break;
        }
    }
    let torrent = match mine.iter().find(|t| torbox_ready(t)).or(mine.first()) {
        Some(t) if torbox_ready(t) => t.clone(),
        Some(t) => torbox_wait_ready(token, t["id"].as_i64().unwrap_or(-1)).await?,
        None => {
            let added = torbox_add(token, magnet, cached_only).await?;
            let mut ready = torbox_wait_ready(token, added["torrent_id"].as_i64().unwrap_or(-1)).await?;
            if ready["name"].as_str().is_none_or(str::is_empty) {
                ready["name"] = added["name"].clone();
            }
            ready
        }
    };
    let files: Vec<RawFile> = torrent["files"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|f| {
            let path = f["name"].as_str().filter(|n| !n.is_empty()).or(f["short_name"].as_str()).unwrap_or_default().to_string();
            (f["id"].as_i64().unwrap_or(-1), path, f["size"].as_i64().unwrap_or(-1), f["zipped"] == true)
        })
        .collect();
    let id = torrent["id"].as_i64().filter(|i| *i >= 0).ok_or("TorBox returned an invalid torrent ID.")?;
    manifest("TorBox", id, torrent["name"].as_str().unwrap_or_default(), &files, magnet_name(magnet).as_deref())
}

async fn torbox_link(token: &str, torrent_id: i64, file_id: i64) -> Result<String, String> {
    let path = format!("/torrents/requestdl?token={}&torrent_id={torrent_id}&file_id={file_id}&zip_link=false", percent_encoding::utf8_percent_encode(token, percent_encoding::NON_ALPHANUMERIC));
    let data = torbox_get(token, &path).await?;
    match data["data"].as_str() {
        Some(url) if data["success"] == true && !url.is_empty() => Ok(url.to_string()),
        _ => Err(TORBOX_LINK_UNAVAILABLE.into()),
    }
}

// shortcut: TorBox files download one after another; Electron's TorBoxBatchDownloader runs several at once.
async fn torbox_plan(token: &str, uri: &str, selection: Option<&[i64]>) -> Result<Plan, String> {
    let manifest = torbox_manifest(token, uri, false).await?;
    let selected = select(&manifest.files, |f| f.id, selection)?;
    let lone = selected.len() == 1 && !selected[0].path.contains('/');
    let entries = selected
        .iter()
        .map(|f| {
            let link = Link::TorBox { token: token.to_string(), torrent_id: manifest.torrent_id, file_id: f.id };
            (link, f.path.clone(), Some(f.size), f.id.to_string(), !manifest.archive_only)
        })
        .collect();
    let mut plan = batch("torBox", uri, entries);
    plan.folder = if lone { None } else { Some(manifest.name) };
    Ok(plan)
}

// ---- AllDebrid (all-debrid.ts) ----

/// One AllDebrid call, retried on 429/503 like the axios interceptor.
async fn ad_call(token: &str, path: &str, form: &[(&str, String)]) -> Result<Value, String> {
    let url = if path.starts_with("/v4.1") { format!("{AD_API}{path}") } else { format!("{AD_API}/v4{path}") };
    let mut query = vec![("agent", agent())];
    if !path.starts_with("/v4.1") {
        query.push(("apikey", token.to_string()));
    }
    for retry in 0..=5u32 {
        let response = client(Duration::from_secs(30))?.post(&url).query(&query).bearer_auth(token).form(form).send().await.map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        if (status == 429 || status == 503) && retry < 5 {
            tokio::time::sleep(Duration::from_millis(2000 << retry)).await;
            continue;
        }
        let body = json_of(response).await?;
        if body["status"] == "error" {
            return Err(body["error"]["message"].as_str().unwrap_or("AllDebrid API error").to_string());
        }
        return Ok(body["data"].clone());
    }
    Err("AllDebrid is rate-limiting requests.".into())
}

/// Arrays, single objects and id-keyed maps all mean a list here.
fn ad_list(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.clone(),
        Value::Object(o) if o.contains_key("id") => vec![v.clone()],
        Value::Object(o) => o.values().cloned().collect(),
        _ => vec![],
    }
}

/// (download URL, file name, size) for a hoster link, waiting for a delayed one.
async fn ad_unlock(token: &str, link: &str) -> Result<(String, Option<String>, Option<u64>), String> {
    let data = ad_call(token, "/link/unlock", &[("link", link.to_string())]).await?;
    let mut url = data["link"].as_str().unwrap_or_default().to_string();
    if let Some(delayed) = data["delayed"].as_i64().filter(|d| *d != 0) {
        url.clear();
        for _ in 0..120 {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let state = ad_call(token, "/link/delayed", &[("id", delayed.to_string())]).await?;
            match state["status"].as_i64() {
                Some(2) if state["link"].is_string() => {
                    url = state["link"].as_str().unwrap_or_default().to_string();
                    break;
                }
                Some(3) => return Err("[AllDebrid] Delayed link generation failed (status=3)".into()),
                _ => {}
            }
        }
        if url.is_empty() {
            return Err("[AllDebrid] Delayed link timed out after 120 attempts".into());
        }
    }
    if url.is_empty() {
        return Err("[AllDebrid] /link/unlock returned neither a download link nor a delayed ID".into());
    }
    let decoded = percent_encoding::percent_decode_str(&url).decode_utf8().map(|s| s.into_owned()).unwrap_or(url);
    Ok((decoded, data["filename"].as_str().map(str::to_string), data["filesize"].as_u64()))
}

fn ad_file_name(link: &str) -> String {
    url::Url::parse(link).ok().and_then(|u| u.path_segments().and_then(|mut s| s.rfind(|p| !p.is_empty()).map(str::to_string))).unwrap_or_else(|| "file".into())
}

/// `extractFileEntries`: (link, relative path, size) for each file in the tree.
fn ad_tree(nodes: &[Value], parent: &[String], out: &mut Vec<(String, String, Option<u64>)>) {
    for node in nodes {
        let name = node["n"].as_str().filter(|n| !n.is_empty());
        if let Some(link) = node["l"].as_str() {
            let file = name.map_or_else(|| ad_file_name(link), str::to_string);
            out.push((link.to_string(), parent.iter().cloned().chain([file]).collect::<Vec<_>>().join("/"), node["s"].as_u64()));
        }
        if let Some(children) = node["e"].as_array().filter(|c| !c.is_empty()) {
            let next: Vec<String> = parent.iter().cloned().chain(name.map(str::to_string)).collect();
            ad_tree(children, &next, out);
        }
    }
}

async fn all_debrid_plan(token: &str, uri: &str) -> Result<Plan, String> {
    if !uri.starts_with("magnet:") {
        let (url, name, size) = ad_unlock(token, uri).await?;
        let path = name.unwrap_or_else(|| ad_file_name(&url));
        return Ok(batch("allDebrid", uri, vec![(Link::Direct(url), path.clone(), size, path, false)]));
    }
    let hash = info_hash(uri).ok_or(NOT_CACHED_ALL_DEBRID)?;
    let magnets = ad_list(&ad_call(token, "/v4.1/magnet/status", &[]).await?["magnets"]);
    let id = match magnets.iter().find(|m| m["hash"].as_str().is_some_and(|h| h.eq_ignore_ascii_case(&hash))) {
        Some(m) => m["id"].as_i64(),
        None => ad_list(&ad_call(token, "/magnet/upload", &[("magnets[]", uri.to_string())]).await?["magnets"]).first().and_then(|m| m["id"].as_i64()),
    }
    .ok_or(NOT_CACHED_ALL_DEBRID)?;
    let status = ad_list(&ad_call(token, "/v4.1/magnet/status", &[("id", id.to_string())]).await?["magnets"]);
    if status.first().and_then(|m| m["statusCode"].as_i64()).is_some_and(|code| code != 4) {
        return Err(NOT_CACHED_ALL_DEBRID.into());
    }
    let files = match ad_call(token, "/v4.1/magnet/files", &[("id[]", id.to_string())]).await {
        Ok(v) => v,
        Err(_) => ad_call(token, "/magnet/files", &[("id[]", id.to_string())]).await?,
    };
    let first = ad_list(&files["magnets"]).into_iter().next().unwrap_or(Value::Null);
    let mut tree = vec![];
    if first["error"].is_null() {
        ad_tree(first["files"].as_array().map_or(&[][..], Vec::as_slice), &[], &mut tree);
    }
    if tree.is_empty() {
        return Err(NOT_CACHED_ALL_DEBRID.into());
    }
    let entries = tree
        .into_iter()
        .map(|(link, path, size)| {
            let identity = sanitize_relative(&path);
            (Link::AllDebrid { token: token.to_string(), link }, path, size, identity, false)
        })
        .collect();
    Ok(batch("allDebrid", uri, entries))
}

// ---- Premiumize (premiumize.ts) ----

async fn pm_call(token: &str, method: reqwest::Method, path: &str, params: &[(&str, String)]) -> Result<Value, String> {
    let mut all = vec![("apikey", token.to_string())];
    all.extend(params.iter().cloned());
    let request = client(Duration::from_secs(30))?.request(method.clone(), format!("{PM_API}{path}"));
    let request = if method == reqwest::Method::GET { request.query(&all) } else { request.form(&all) };
    let body = json_of(request.send().await.map_err(|e| e.to_string())?).await?;
    if body["status"] != "success" {
        return Err(body["message"].as_str().or(body["error"].as_str()).unwrap_or("Premiumize API error").to_string());
    }
    Ok(body)
}

/// `getDownloadUrl`. A single cached file downloads directly; several are zipped by Premiumize.
// shortcut: an uncached hoster link is not routed through a Premiumize transfer; it reports not cached.
async fn premiumize_url(token: &str, uri: &str) -> Result<Option<String>, String> {
    let direct = pm_call(token, reqwest::Method::POST, "/transfer/directdl", &[("src", uri.to_string())]).await.ok();
    let content = direct.as_ref().and_then(|d| d["content"].as_array().cloned()).unwrap_or_default();
    if let [only] = content.as_slice() {
        if let Some(link) = only["link"].as_str() {
            return Ok(Some(percent_encoding::percent_decode_str(link).decode_utf8().map(|s| s.into_owned()).unwrap_or_else(|_| link.to_string())));
        }
    }
    if content.len() > 1 || uri.starts_with("magnet:") {
        let created = pm_call(token, reqwest::Method::POST, "/transfer/create", &[("src", uri.to_string())]).await;
        if content.len() <= 1 {
            // Not cached: Premiumize fetches it now; the user retries later.
            return match created {
                Ok(_) => Err(PREMIUMIZE_TRANSFER_STARTED.into()),
                Err(_) => Ok(None),
            };
        }
        let id = created.ok().and_then(|c| c["id"].as_str().map(str::to_string));
        let Some(id) = id else { return Ok(None) };
        // Cached multi-file transfers finish at once; give a slow one a minute.
        for _ in 0..12 {
            let transfers = pm_call(token, reqwest::Method::GET, "/transfer/list", &[]).await?;
            let transfer = transfers["transfers"].as_array().and_then(|t| t.iter().find(|t| t["id"] == id.as_str()).cloned());
            let status = transfer.as_ref().and_then(|t| t["status"].as_str()).unwrap_or("finished").to_lowercase();
            if matches!(status.as_str(), "error" | "banned" | "timeout") {
                return Err(format!("[Premiumize] Transfer failed: {status}"));
            }
            if status == "finished" || status == "seeding" {
                let (files, folders) = match transfer {
                    Some(t) if t["folder_id"].is_string() => (vec![], vec![t["folder_id"].as_str().unwrap_or_default().to_string()]),
                    Some(t) if t["file_id"].is_string() => (vec![t["file_id"].as_str().unwrap_or_default().to_string()], vec![]),
                    _ => return Ok(None),
                };
                let mut params: Vec<(&str, String)> = files.into_iter().map(|f| ("files[]", f)).collect();
                params.extend(folders.into_iter().map(|f| ("folders[]", f)));
                let zip = pm_call(token, reqwest::Method::POST, "/zip/generate", &params).await?;
                return Ok(zip["location"].as_str().map(str::to_string));
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        return Err(NOT_CACHED_PREMIUMIZE.into());
    }
    Ok(None)
}

// ---- Channels ----

/// `authenticate*(token)`: checks the token and returns the account, as each client's `getUser`.
pub async fn authenticate(downloader: i64, token: &str) -> Result<Value, String> {
    match downloader {
        REAL_DEBRID => rd_get(token, "/user").await,
        TORBOX => Ok(torbox_get(token, "/user/me").await?["data"].clone()),
        ALL_DEBRID => {
            let response = client(Duration::from_secs(30))?
                .get(format!("{AD_API}/v4/user"))
                .query(&[("apikey", token.to_string()), ("agent", agent())])
                .bearer_auth(token)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let body = json_of(response).await?;
            if body["status"] == "error" {
                return Err(body["error"]["message"].as_str().unwrap_or("AllDebrid API error").to_string());
            }
            Ok(body["data"]["user"].clone())
        }
        _ => {
            let info = pm_call(token, reqwest::Method::GET, "/account/info", &[]).await?;
            let customer = match &info["customer_id"] {
                Value::Null => String::new(),
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            Ok(json!({
                "customer_id": customer,
                "premium_until": info["premium_until"].as_f64().unwrap_or(0.0),
                "space_used": info["space_used"].as_f64().unwrap_or(0.0),
                "limit_used": info["limit_used"].as_f64().unwrap_or(0.0),
            }))
        }
    }
}

/// `getDebridFiles(magnet, provider)`: the file list for the picker (Real-Debrid and TorBox).
pub async fn files(store: &Store, magnet: &str, downloader: i64) -> Value {
    if !magnet.starts_with("magnet:") {
        return json!({ "ok": false, "error": INVALID_MAGNET });
    }
    let result = async {
        match downloader {
            TORBOX => {
                let m = torbox_manifest(&token(store, TORBOX)?, magnet, true).await?;
                Ok(files_response(&m.name, &m.files, Some(m.archive_only)))
            }
            REAL_DEBRID => {
                let token = token(store, REAL_DEBRID)?;
                let id = rd_torrent_id(&token, magnet).await?;
                let info = rd_info_with_files(&token, &id, false).await?;
                let files: Vec<ManifestFile> = rd_files(&info)?.into_iter().map(|f| ManifestFile { id: f.id, path: f.path, size: f.size }).collect();
                Ok(files_response(info["filename"].as_str().unwrap_or_default(), &files, None))
            }
            _ => Err(FILES_UNAVAILABLE.to_string()),
        }
    }
    .await;
    match result {
        Ok(data) => json!({ "ok": true, "data": data }),
        Err(error) => {
            eprintln!("[debrid] file list failed: {error}");
            json!({ "ok": false, "error": error })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_hashes_come_out_as_hex() {
        let hex = "c12fe1c06bba254a9dc9f519b335aa7c1367a88a";
        assert_eq!(info_hash(&format!("magnet:?xt=urn:btih:{}&dn=x", hex.to_uppercase())).as_deref(), Some(hex));
        // Base32 of the same 20 bytes.
        assert_eq!(info_hash("magnet:?xt=urn:btih:YEX6DQDLXISUVHOJ6UM3GNNKPQJWPKEK").as_deref(), Some(hex));
        assert_eq!(info_hash("magnet:?dn=nohash"), None);
    }

    #[test]
    fn segments_are_safe_like_torbox_files() {
        assert_eq!(sanitize_segment("a&amp;b &#x41;"), "a&b A");
        assert_eq!(sanitize_segment("name..."), "name_");
        assert_eq!(sanitize_segment("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_segment("com1"), "_com1");
        assert_eq!(sanitize_segment("a:b?c"), "a_b_c");
        assert_eq!(sanitize_segment("console"), "console");
    }

    #[test]
    fn manifests_share_one_root() {
        let files = vec![(1, "Game/setup.exe".to_string(), 10, false), (2, "Game/data/a.bin".to_string(), 5, false)];
        let m = manifest("TorBox", 7, "ignored", &files, None).unwrap();
        assert_eq!(m.name, "Game");
        assert_eq!(m.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["Game/setup.exe", "Game/data/a.bin"]);
        assert_eq!(m.files.iter().map(|f| f.size).sum::<u64>(), 15);
        // An opaque provider name gives way to the magnet's display name.
        let flat = vec![(1, "a.rar".to_string(), 1, true)];
        let m = manifest("TorBox", 7, "0123456789abcdef0123456789abcdef", &flat, Some("Nice Name")).unwrap();
        assert_eq!((m.name.as_str(), m.archive_only), ("Nice Name", true));
        assert!(manifest("TorBox", 7, "x", &[(1, "../evil".into(), 1, false)], None).is_err());
        assert!(manifest("TorBox", 7, "x", &[(1, "A".into(), 1, false), (2, "a".into(), 1, false)], None).is_err());
    }

    #[test]
    fn real_debrid_paths() {
        let info = json!({
            "filename": "Game",
            "files": [
                { "id": 1, "path": "/setup.exe", "bytes": 10, "selected": 1 },
                { "id": 2, "path": "/data/a.bin", "bytes": 5, "selected": 0 },
            ],
        });
        let files = rd_files(&info).unwrap();
        assert_eq!(files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["Game/setup.exe", "Game/data/a.bin"]);
        assert!(rd_has_selection(&files, Some(&[1])));
        assert!(!rd_has_selection(&files, None));
        let single = json!({ "filename": "Game.iso", "files": [{ "id": 1, "path": "/Game.iso", "bytes": 3, "selected": 1 }] });
        assert_eq!(rd_files(&single).unwrap()[0].path, "Game.iso");
    }

    #[test]
    fn batch_paths_and_folders() {
        assert_eq!(root_folder(&["G/a".into(), "G/b/c".into()]).as_deref(), Some("G"));
        assert_eq!(root_folder(&["G/a".into(), "H/b".into()]), None);
        assert_eq!(output_path("Game/pack.zip", 1), "pack.zip");
        assert_eq!(output_path("Game/pack.zip", 2), "Game/pack.zip");
        assert_eq!(sanitize_relative("../a/./b:c"), "a/b_c");
        let mut tree = vec![];
        ad_tree(&[json!({ "n": "Game", "e": [{ "n": "a.bin", "l": "https://x/1", "s": 4 }, { "l": "https://x/files/b.bin" }] })], &[], &mut tree);
        assert_eq!(tree, [("https://x/1".into(), "Game/a.bin".into(), Some(4)), ("https://x/files/b.bin".into(), "Game/b.bin".into(), None)]);
    }
}
