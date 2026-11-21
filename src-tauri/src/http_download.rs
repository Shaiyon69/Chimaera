//! One HTTP file download, ported from src/main/services/download/js-http-downloader.ts,
//! parallel-range-download.ts, range-download-state.ts and download-throttle.ts.
//!
//! A server that serves byte ranges with a strong validator (a strong ETag, or a Last-Modified at
//! least 60 s older than Date) gets 4 parallel 8 MB ranges. Their progress is kept in
//! `DownloadRanges/<sha256(path)>.json`, the same name and format Electron uses, so a partial file
//! with holes resumes safely. Anything else is one stream, resumed from the file size.
//! `Batch` runs several such downloads one after another (debrid torrents, or one hoster file).

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Response};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::debrid::{Entry, Link};

pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:144.0) Gecko/20100101 Firefox/144.0";
const RANGE_SIZE: u64 = 8 * 1024 * 1024;
const CONNECTIONS: usize = 4;
const MAX_RETRY_ATTEMPTS: u32 = 10;
const MAX_STATUS_RETRY_ATTEMPTS: u32 = 4;
const MAX_RETRY_AFTER: Duration = Duration::from_secs(20);
const INITIAL_RETRY_DELAY_MS: u64 = 1_000;
const MAX_RETRY_DELAY_MS: u64 = 15_000;
const STALL_TIMEOUT: Duration = Duration::from_secs(30);
const PROGRESS_RESET_THRESHOLD_BYTES: u64 = 16 * 1024 * 1024;
const MAX_BUDGET_RESETS: u32 = 50;
const MAX_RESTARTS_FROM_ZERO: u32 = 3;
const PARALLEL_FAILURE_LIMIT: u32 = 2;
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(1);
const STRONG_LAST_MODIFIED_AGE_SECS: i64 = 60;
const MAX_RANGE_STATE_SIZE: u64 = 1024 * 1024;
const PREFLIGHT_ATTEMPTS: u32 = 3;
/// Tries for a connection-bound link to land on its IP: with 1 connection in 3 on it, 20 tries
/// miss about 1 in 3,000 times.
const MAX_BOUND_RETRIES: u32 = 20;
const BOUND_PREFLIGHT_ATTEMPTS: u32 = 12;
const BOUND_RETRY_DELAY: Duration = Duration::from_millis(500);
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(15);
const RETRYABLE_STATUS: [u16; 6] = [408, 429, 500, 502, 503, 504];

/// Datanodes' file servers lock a link to one public IP and answer 403 ("made for another
/// connection") to any other. Behind a carrier-grade NAT that hands each connection a different
/// IP, a 403 there is worth retrying on a new connection.
fn connection_bound(url: &reqwest::Url, status: u16) -> bool {
    status == 403 && url.host_str().is_some_and(|h| h == "dlproxy.uk" || h.ends_with(".dlproxy.uk"))
}

fn new_client() -> Result<Client, String> {
    Client::builder().connect_timeout(STALL_TIMEOUT).build().map_err(|e| e.to_string())
}

// ---- Failures ----

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    /// Not worth retrying; the download stops with this error.
    Fatal,
    /// Connection trouble: retried with backoff.
    Network,
    /// No data for STALL_TIMEOUT.
    Stall,
    /// An HTTP error status; retryable ones get their own budget and honor Retry-After.
    Status { retryable: bool, retry_after: Option<Duration> },
    /// The server stopped serving matching byte ranges: retry with one connection.
    RangesUnsupported,
    /// Paused or cancelled.
    Stopped,
}

#[derive(Debug)]
struct Failure {
    kind: Kind,
    message: String,
    /// Raised by the parallel range path, which two failures downgrade to one connection.
    parallel: bool,
    /// A 403 that a new connection may clear (see `connection_bound`).
    bound: bool,
}

impl Failure {
    fn new(kind: Kind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into(), parallel: false, bound: false }
    }
    fn fatal(message: impl Into<String>) -> Self {
        Self::new(Kind::Fatal, message)
    }
    fn stopped() -> Self {
        Self::new(Kind::Stopped, "stopped")
    }
    fn network(error: reqwest::Error) -> Self {
        let kind = if error.is_builder() { Kind::Fatal } else { Kind::Network };
        Self::new(kind, error.to_string())
    }
    fn io(error: std::io::Error) -> Self {
        Self::fatal(error.to_string())
    }
    fn status(response: &Response) -> Self {
        let code = response.status().as_u16();
        let retry_after = header(response, "retry-after").and_then(|v| retry_after(&v, unix_now()));
        let bound = connection_bound(response.url(), code);
        let mut failure = Self::new(
            Kind::Status { retryable: RETRYABLE_STATUS.contains(&code) || bound, retry_after },
            format!("The download link is not available (HTTP {code})."),
        );
        failure.bound = bound;
        failure
    }
}

// ---- Header parsing (parallel-range-download.ts, js-http-downloader-helpers.ts) ----

fn header(response: &Response, name: &str) -> Option<String> {
    response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
}

fn is_html(response: &Response) -> bool {
    header(response, "content-type").is_some_and(|t| {
        let t = t.to_ascii_lowercase();
        t.contains("text/html") || t.contains("application/xhtml")
    })
}

fn is_encoded(response: &Response) -> bool {
    header(response, "content-encoding").is_some_and(|e| {
        let e = e.trim().to_ascii_lowercase();
        !e.is_empty() && e != "identity"
    })
}

fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

/// IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`) to Unix seconds.
fn http_date(value: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let parts: Vec<&str> = value.split_whitespace().collect();
    if parts.len() != 6 || parts[5] != "GMT" {
        return None;
    }
    let day: i64 = parts[1].parse().ok()?;
    let month = MONTHS.iter().position(|m| *m == parts[2])? as i64 + 1;
    let year: i64 = parts[3].parse().ok()?;
    let time: Vec<i64> = parts[4].split(':').map(|t| t.parse().ok()).collect::<Option<_>>()?;
    let [h, m, s] = time[..] else { return None };
    Some(days_from_civil(year, month, day) * 86_400 + h * 3600 + m * 60 + s)
}

/// `parseRetryAfterMs`: delta-seconds or an HTTP date.
fn retry_after(value: &str, now: i64) -> Option<Duration> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        return value.parse().ok().map(Duration::from_secs);
    }
    http_date(value).map(|at| Duration::from_secs((at - now).max(0) as u64))
}

/// `getStrongRangeValidator`.
fn strong_validator(etag: Option<&str>, modified: Option<&str>, date: Option<&str>) -> Option<String> {
    if let Some(etag) = etag {
        let bytes = etag.as_bytes();
        let strong = bytes.len() >= 2
            && bytes[0] == b'"'
            && bytes[bytes.len() - 1] == b'"'
            && bytes[1..bytes.len() - 1].iter().all(|&b| b == 0x21 || (0x23..=0x7e).contains(&b) || b >= 0x80);
        return strong.then(|| etag.to_string());
    }
    let (modified, date) = (modified?, date?);
    (http_date(date)? - http_date(modified)? >= STRONG_LAST_MODIFIED_AGE_SECS).then(|| modified.to_string())
}

fn response_validator(response: &Response) -> Option<String> {
    strong_validator(header(response, "etag").as_deref(), header(response, "last-modified").as_deref(), header(response, "date").as_deref())
}

/// `bytes a-b/t` parts of a Content-Range.
fn content_range(value: &str) -> Option<(u64, u64, u64)> {
    let rest = value.trim().strip_prefix("bytes ").or_else(|| value.trim().strip_prefix("Bytes "))?;
    let (span, total) = rest.split_once('/')?;
    let (start, end) = span.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

/// `getRangeTotal`: the total size when the response is exactly the requested range.
fn range_total(status: u16, range: Option<&str>, length: Option<&str>, encoded: bool, start: u64, end: u64) -> Option<u64> {
    if status != 206 || encoded {
        return None;
    }
    let (actual_start, actual_end, total) = content_range(range?)?;
    if actual_start != start || total <= start || actual_end != end.min(total - 1) {
        return None;
    }
    if length.is_some_and(|l| l.trim().parse::<u64>().ok() != Some(actual_end - start + 1)) {
        return None;
    }
    Some(total)
}

fn response_range_total(response: &Response, start: u64, end: u64) -> Option<u64> {
    let range = header(response, "content-range");
    let length = header(response, "content-length");
    range_total(response.status().as_u16(), range.as_deref(), length.as_deref(), is_encoded(response), start, end)
}

/// The name in a Content-Disposition header, made safe for one path segment.
fn disposition_filename(value: &str) -> Option<String> {
    let find = |key: &str| -> Option<String> {
        let lower = value.to_ascii_lowercase();
        let mut from = 0;
        while let Some(at) = lower[from..].find(key) {
            let after = &value[from + at + key.len()..];
            let trimmed = after.trim_start();
            if let Some(rest) = trimmed.strip_prefix('=') {
                let raw = rest.split(';').next().unwrap_or_default().trim().trim_matches(|c| c == '"' || c == '\'');
                return Some(raw.to_string());
            }
            from += at + key.len();
        }
        None
    };
    let sanitize = |name: &str| -> Option<String> {
        let decoded = percent_encoding::percent_decode_str(name).decode_utf8().map(|s| s.into_owned()).unwrap_or_else(|_| name.to_string());
        let base = decoded.rsplit(['/', '\\']).next().unwrap_or_default();
        let clean: String = base.chars().map(|c| if "<>:\"/\\|?*".contains(c) { '_' } else { c }).filter(|c| *c as u32 >= 32).collect();
        let clean = clean.trim();
        (!clean.is_empty()).then(|| clean.to_string())
    };
    if let Some(star) = find("filename*") {
        let encoded = star.split_once("''").map_or(star.as_str(), |(_, v)| v);
        if let Some(name) = sanitize(encoded) {
            return Some(name);
        }
    }
    // Skip "filename*" when looking for the plain parameter.
    let plain = value.to_ascii_lowercase().match_indices("filename").find(|(at, _)| !value[at + 8..].starts_with('*')).map(|(at, _)| at)?;
    let rest = value[plain + 8..].trim_start().strip_prefix('=')?;
    sanitize(rest.split(';').next().unwrap_or_default().trim().trim_matches(|c| c == '"' || c == '\''))
}

fn filename_from_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let last = parsed.path().rsplit('/').next()?;
    if !last.contains('.') {
        return None;
    }
    percent_encoding::percent_decode_str(last).decode_utf8().ok().map(|s| s.into_owned())
}

// ---- Range state (range-download-state.ts) ----

#[derive(Debug, Clone, PartialEq)]
struct RangeState {
    total: u64,
    validator: String,
    resource_id: Option<String>,
    /// Written byte ranges, end exclusive, sorted and disjoint.
    ranges: Vec<(u64, u64)>,
}

impl RangeState {
    /// `markSavedRange`: merges [start, end) into the list.
    fn mark(&mut self, start: u64, end: u64) {
        let (mut start, mut end) = (start, end);
        let first = self.ranges.iter().position(|r| r.1 >= start).unwrap_or(self.ranges.len());
        let mut last = first;
        while last < self.ranges.len() && self.ranges[last].0 <= end {
            start = start.min(self.ranges[last].0);
            end = end.max(self.ranges[last].1);
            last += 1;
        }
        self.ranges.splice(first..last, [(start, end)]);
    }

    fn missing(&self) -> Vec<(u64, u64)> {
        let mut gaps = vec![];
        let mut next = 0;
        for &(start, end) in &self.ranges {
            if start > next {
                gaps.push((next, start));
            }
            next = end;
        }
        if next < self.total {
            gaps.push((next, self.total));
        }
        gaps
    }

    fn saved(&self) -> u64 {
        self.ranges.iter().map(|(s, e)| e - s).sum()
    }

    fn to_json(&self) -> Value {
        let ranges: Vec<[u64; 2]> = self.ranges.iter().map(|&(s, e)| [s, e]).collect();
        json!({ "version": 1, "total": self.total, "validator": self.validator, "resourceId": self.resource_id, "ranges": ranges })
    }
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Same key as Electron's `rangeStatePath`: sha256 of the absolute path, lowercased on Windows.
fn state_path(dir: &Path, file: &Path) -> PathBuf {
    let absolute = std::path::absolute(file).unwrap_or_else(|_| file.to_path_buf());
    let mut identity = absolute.to_string_lossy().into_owned();
    if cfg!(windows) {
        identity = identity.to_lowercase();
    }
    dir.join(format!("{}.json", sha256_hex(&identity)))
}

fn parse_state(text: &str, file_size: u64) -> Option<RangeState> {
    let v: Value = serde_json::from_str(text).ok()?;
    let total = v["total"].as_u64().filter(|t| *t > 0)?;
    let validator = v["validator"].as_str().filter(|s| !s.is_empty())?.to_string();
    let resource_id = match &v["resourceId"] {
        Value::Null => None,
        Value::String(s) if s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) => Some(s.clone()),
        _ => return None,
    };
    if v["version"] != 1 || file_size > total {
        return None;
    }
    let mut ranges = vec![];
    let mut previous_end: Option<u64> = None;
    for range in v["ranges"].as_array()? {
        let (start, end) = (range.get(0)?.as_u64()?, range.get(1)?.as_u64()?);
        if range.as_array()?.len() != 2 || previous_end.is_some_and(|p| start <= p) || end <= start || end > total || end > file_size {
            return None;
        }
        previous_end = Some(end);
        ranges.push((start, end));
    }
    Some(RangeState { total, validator, resource_id, ranges })
}

/// `readRangeState`. A state whose file is gone is dropped; an unreadable one stops the download.
fn read_state(dir: &Path, file: &Path) -> Result<Option<RangeState>, Failure> {
    let path = state_path(dir, file);
    let invalid = || Failure::fatal("The saved byte ranges are invalid. Keeping the partial file.");
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(invalid()),
        Ok(meta) if !meta.is_file() || meta.len() > MAX_RANGE_STATE_SIZE => return Err(invalid()),
        Ok(_) => {}
    }
    let file_size = match std::fs::symlink_metadata(file) {
        Ok(meta) if meta.is_file() => meta.len(),
        Ok(_) => return Err(invalid()),
        Err(_) => {
            let _ = std::fs::remove_file(&path);
            return Ok(None);
        }
    };
    let text = std::fs::read_to_string(&path).map_err(|_| invalid())?;
    parse_state(&text, file_size).map(Some).ok_or_else(invalid)
}

fn save_state(dir: &Path, file: &Path, state: &RangeState) -> Result<(), Failure> {
    std::fs::create_dir_all(dir).map_err(Failure::io)?;
    let path = state_path(dir, file);
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, state.to_json().to_string()).map_err(Failure::io)?;
    std::fs::rename(&temp, &path).map_err(Failure::io)
}

fn remove_state(dir: &Path, file: &Path) {
    let _ = std::fs::remove_file(state_path(dir, file));
}

/// Copies Electron's range states the first time, so its half-done parallel downloads resume.
pub fn adopt_electron_ranges(electron_dir: &Path, dir: &Path) {
    if dir.exists() || !electron_dir.is_dir() {
        return;
    }
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    for entry in std::fs::read_dir(electron_dir).into_iter().flatten().flatten() {
        if entry.file_name().to_string_lossy().ends_with(".json") {
            let _ = std::fs::copy(entry.path(), dir.join(entry.file_name()));
        }
    }
}

// ---- Throttle (download-throttle.ts) ----

struct Throttle {
    limit: Option<u64>,
    updated: Instant,
    available: f64,
}

// ---- The download ----

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum State {
    #[default]
    Active,
    Paused,
    Complete,
    Error,
}

#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub folder_name: String,
    pub file_size: u64,
    pub bytes: u64,
    pub speed: f64,
    pub state: State,
    pub reconnecting: bool,
    pub error: Option<String>,
}

pub struct Options {
    pub url: String,
    pub save_path: PathBuf,
    /// The name to keep, e.g. the folder name of a resumed download. None: from the server.
    pub filename: Option<String>,
    pub headers: Vec<(String, String)>,
    /// Ties a range-state file to its source (Electron passes the download URI).
    pub resource_id: String,
}

struct Inner {
    progress: Progress,
    speed_at: Instant,
    speed_bytes: u64,
    attempt_bytes: u64,
    /// Set once the server named the file (Content-Disposition) or a fresh file started.
    resolved_name: Option<String>,
    throttle: Throttle,
}

struct Ctx {
    options: Options,
    ranges_dir: PathBuf,
    /// Replaced before a status retry, so the retry opens a new connection.
    client: Mutex<Client>,
    inner: Mutex<Inner>,
    stop: watch::Receiver<bool>,
}

pub struct HttpDownload {
    ctx: Arc<Ctx>,
    stop: watch::Sender<bool>,
    task: tokio::sync::Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

impl HttpDownload {
    pub fn start(options: Options, ranges_dir: PathBuf, limit: Option<u64>) -> Result<Arc<Self>, String> {
        let client = Mutex::new(new_client()?);
        let (stop, stop_rx) = watch::channel(false);
        let ctx = Arc::new(Ctx {
            inner: Mutex::new(Inner {
                progress: Progress::default(),
                speed_at: Instant::now(),
                speed_bytes: 0,
                attempt_bytes: 0,
                resolved_name: None,
                throttle: Throttle { limit, updated: Instant::now(), available: 0.0 },
            }),
            options,
            ranges_dir,
            client,
            stop: stop_rx,
        });
        let task = tauri::async_runtime::spawn(run(ctx.clone()));
        Ok(Arc::new(Self { ctx, stop, task: tokio::sync::Mutex::new(Some(task)) }))
    }

    pub fn progress(&self) -> Progress {
        self.ctx.lock().progress.clone()
    }

    pub fn set_limit(&self, limit: Option<u64>) {
        let mut inner = self.ctx.lock();
        inner.throttle = Throttle { limit, updated: Instant::now(), available: 0.0 };
    }

    /// Waits for the transfer to end on its own (finished, failed or stopped).
    pub async fn wait(&self) {
        if let Some(task) = self.task.lock().await.take() {
            let _ = task.await;
        }
    }

    /// Pauses: stops the transfer and waits for it, keeping the partial file.
    pub async fn stop(&self) {
        let _ = self.stop.send(true);
        if let Some(task) = self.task.lock().await.take() {
            let _ = task.await;
        }
    }

    /// Stops and deletes the partial file and its range state, unless it finished.
    pub async fn cancel(&self) {
        self.stop().await;
        let progress = self.progress();
        if progress.state == State::Complete || progress.folder_name.is_empty() {
            return;
        }
        let file = self.ctx.options.save_path.join(&progress.folder_name);
        if file.is_file() && std::fs::remove_file(&file).is_ok() {
            remove_state(&self.ctx.ranges_dir, &file);
        }
    }
}

impl Ctx {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn stopped(&self) -> bool {
        *self.stop.borrow()
    }

    fn add_bytes(&self, n: u64) {
        let mut inner = self.lock();
        inner.progress.bytes += n;
        inner.attempt_bytes += n;
        inner.progress.reconnecting = false;
        let elapsed = inner.speed_at.elapsed().as_secs_f64();
        if elapsed >= 1.0 {
            inner.progress.speed = inner.progress.bytes.saturating_sub(inner.speed_bytes) as f64 / elapsed;
            inner.speed_at = Instant::now();
            inner.speed_bytes = inner.progress.bytes;
        }
    }

    fn set_bytes(&self, bytes: u64) {
        let mut inner = self.lock();
        inner.progress.bytes = bytes;
        inner.speed_at = Instant::now();
        inner.speed_bytes = bytes;
        inner.progress.speed = 0.0;
    }

    /// Waits until `n` bytes fit the speed limit. Shared by every connection of this download.
    async fn throttle(&self, n: u64) {
        let mut remaining = n as f64;
        loop {
            let delay = {
                let mut inner = self.lock();
                let t = &mut inner.throttle;
                let Some(limit) = t.limit else { return };
                let limit = limit as f64;
                // Keep at most one second of idle credit.
                t.available = (t.available + t.updated.elapsed().as_secs_f64() * limit).min(limit);
                t.updated = Instant::now();
                let used = remaining.min(t.available);
                remaining -= used;
                t.available -= used;
                if remaining <= 0.0 {
                    return;
                }
                Duration::from_millis(((remaining * 1000.0 / limit) as u64).clamp(1, 100))
            };
            if self.stopped() {
                return;
            }
            tokio::time::sleep(delay).await;
        }
    }

    fn current_name(&self) -> String {
        let resolved = self.lock().resolved_name.clone();
        resolved
            .or_else(|| self.options.filename.clone())
            .or_else(|| filename_from_url(&self.options.url))
            .unwrap_or_else(|| "download".into())
    }

    /// For a fresh file: the server's name wins unless a name was given (`resolveOutputPath`).
    fn output_path(&self, response: &Response, current: &Path) -> PathBuf {
        let mut inner = self.lock();
        if inner.resolved_name.is_some() || self.options.filename.is_some() {
            return current.to_path_buf();
        }
        let name = header(response, "content-disposition")
            .and_then(|v| disposition_filename(&v))
            .unwrap_or_else(|| current.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        inner.progress.folder_name = name.clone();
        inner.resolved_name = Some(name.clone());
        self.options.save_path.join(name)
    }

    /// One GET; `extra` headers win over the defaults and the hoster's own.
    async fn get(&self, extra: &[(&str, String)]) -> Result<Response, Failure> {
        let mut headers = HeaderMap::new();
        headers.insert("user-agent", HeaderValue::from_static(USER_AGENT));
        headers.insert("accept-encoding", HeaderValue::from_static("identity"));
        let pairs = self.options.headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).chain(extra.iter().map(|(k, v)| (*k, v.as_str())));
        for (key, value) in pairs {
            let name = HeaderName::from_bytes(key.as_bytes()).map_err(|e| Failure::fatal(e.to_string()))?;
            let value = HeaderValue::from_str(value).map_err(|e| Failure::fatal(e.to_string()))?;
            headers.insert(name, value);
        }
        let mut stop = self.stop.clone();
        tokio::select! {
            _ = stop.wait_for(|s| *s) => Err(Failure::stopped()),
            sent = tokio::time::timeout(STALL_TIMEOUT, self.client.lock().unwrap_or_else(|e| e.into_inner()).clone().get(&self.options.url).headers(headers).send()) => match sent {
                Err(_) => Err(Failure::new(Kind::Stall, "The download server did not answer")),
                Ok(Err(e)) => Err(Failure::network(e)),
                Ok(Ok(response)) => Ok(response),
            },
        }
    }
}

/// The next body chunk, with stall detection; also stops on `abort` (a sibling range failed).
async fn next_chunk(ctx: &Ctx, response: &mut Response, abort: &mut watch::Receiver<bool>) -> Result<Option<bytes::Bytes>, Failure> {
    let mut stop = ctx.stop.clone();
    tokio::select! {
        _ = stop.wait_for(|s| *s) => Err(Failure::stopped()),
        _ = abort.wait_for(|s| *s) => Err(Failure::stopped()),
        chunk = tokio::time::timeout(STALL_TIMEOUT, response.chunk()) => match chunk {
            Err(_) => Err(Failure::new(Kind::Stall, "stall detected")),
            Ok(Err(e)) => Err(Failure::network(e)),
            Ok(Ok(chunk)) => Ok(chunk),
        },
    }
}

async fn run(ctx: Arc<Ctx>) {
    let result = run_with_retry(&ctx).await;
    let mut inner = ctx.lock();
    inner.progress.speed = 0.0;
    inner.progress.reconnecting = false;
    match result {
        Ok(()) => inner.progress.state = State::Complete,
        Err(f) if f.kind == Kind::Stopped => inner.progress.state = State::Paused,
        Err(f) => {
            eprintln!("[http-download] {}", f.message);
            inner.progress.state = State::Error;
            inner.progress.error = Some(f.message);
        }
    }
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_millis((INITIAL_RETRY_DELAY_MS << attempt.saturating_sub(1).min(16)).min(MAX_RETRY_DELAY_MS))
}

/// `startDownloadWithRetry` and `handleDownloadErrorWithRetry`.
async fn run_with_retry(ctx: &Ctx) -> Result<(), Failure> {
    let (mut retries, mut status_retries, mut budget_resets, mut restarts) = (0u32, 0u32, 0u32, 0u32);
    let (mut parallel_failures, mut parallel_disabled, mut bound_retries) = (0u32, false, 0u32);
    loop {
        if ctx.stopped() {
            return Err(Failure::stopped());
        }
        ctx.lock().attempt_bytes = 0;
        let failure = match attempt(ctx, parallel_disabled, &mut restarts).await {
            Ok(()) => return Ok(()),
            Err(f) if f.kind == Kind::Stopped || ctx.stopped() => return Err(Failure::stopped()),
            Err(f) => f,
        };
        // Data is flowing again: a long download keeps its retry budget.
        if ctx.lock().attempt_bytes >= PROGRESS_RESET_THRESHOLD_BYTES && budget_resets < MAX_BUDGET_RESETS {
            retries = 0;
            status_retries = 0;
            budget_resets += 1;
        }
        // Bytes arrived, so this connection had the link's IP; each later drop gets a full set of tries.
        if ctx.lock().attempt_bytes > 0 {
            bound_retries = 0;
        }
        if failure.kind == Kind::RangesUnsupported {
            parallel_disabled = true;
        } else if failure.parallel && matches!(failure.kind, Kind::Network | Kind::Stall) {
            parallel_failures += 1;
            parallel_disabled |= parallel_failures >= PARALLEL_FAILURE_LIMIT;
        }
        let delay = match failure.kind {
            // Each new connection gets another public IP; quick tries until one is the link's.
            Kind::Status { .. } if failure.bound && bound_retries < MAX_BOUND_RETRIES => {
                bound_retries += 1;
                // One stream keeps the connection that landed on the right IP; parallel ranges would
                // each need their own lucky connection.
                parallel_disabled = true;
                *ctx.client.lock().unwrap_or_else(|e| e.into_inner()) = new_client().map_err(Failure::fatal)?;
                BOUND_RETRY_DELAY
            }
            Kind::Status { retryable: true, retry_after } => {
                if status_retries >= MAX_STATUS_RETRY_ATTEMPTS {
                    return Err(Failure::fatal(format!(
                        "The download server is rate-limiting or temporarily unavailable ({}). Try again later or use another source.",
                        failure.message
                    )));
                }
                status_retries += 1;
                if let Ok(client) = new_client() {
                    *ctx.client.lock().unwrap_or_else(|e| e.into_inner()) = client;
                }
                retry_after.map_or_else(|| backoff(status_retries), |d| d.min(MAX_RETRY_AFTER))
            }
            Kind::Network | Kind::Stall | Kind::RangesUnsupported if retries < MAX_RETRY_ATTEMPTS => {
                retries += 1;
                let mut inner = ctx.lock();
                inner.progress.reconnecting = true;
                inner.progress.speed = 0.0;
                backoff(retries)
            }
            Kind::Stall => return Err(Failure::fatal("Download stalled repeatedly and could not be resumed after multiple retries.")),
            _ => return Err(failure),
        };
        eprintln!("[http-download] retrying in {} ms: {}", delay.as_millis(), failure.message);
        let mut stop = ctx.stop.clone();
        tokio::select! {
            _ = stop.wait_for(|s| *s) => return Err(Failure::stopped()),
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

/// One try: parallel ranges when the server supports them safely, else one stream.
async fn attempt(ctx: &Ctx, parallel_disabled: bool, restarts: &mut u32) -> Result<(), Failure> {
    let name = ctx.current_name();
    let path = ctx.options.save_path.join(&name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(Failure::io)?;
    }
    ctx.lock().progress.folder_name = name;
    let state = read_state(&ctx.ranges_dir, &path)?;
    let resource_id = sha256_hex(&ctx.options.resource_id);
    if state.as_ref().is_some_and(|s| s.resource_id.as_deref() != Some(resource_id.as_str())) {
        return Err(Failure::fatal("The saved file belongs to a different download. Keeping the partial file."));
    }
    let file_len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let start = match &state {
        Some(s) => s.missing().first().map_or(s.total, |g| g.0),
        None => file_len,
    };
    ctx.set_bytes(state.as_ref().map_or(file_len, RangeState::saved));
    if let Some(s) = &state {
        ctx.lock().progress.file_size = s.total;
        if s.missing().is_empty() {
            remove_state(&ctx.ranges_dir, &path);
            return Ok(());
        }
    }
    let resume_range = |from: u64| -> Vec<(&str, String)> { if from > 0 { vec![("range", format!("bytes={from}-"))] } else { vec![] } };
    if state.is_none() && (start > 0 || parallel_disabled) {
        // shortcut: the 64 KB resume-overlap check of a file without range state is not ported; it resumes from the file size.
        let response = ctx.get(&resume_range(start)).await?;
        return stream(ctx, response, &path, start, restarts).await;
    }
    let range_size = if parallel_disabled { u64::MAX } else { RANGE_SIZE };
    let gap_end = state.as_ref().and_then(|s| s.missing().first().map(|g| g.1 - 1)).unwrap_or(u64::MAX);
    let end = start.saturating_add(range_size - 1).min(gap_end);
    let mut probe_headers = vec![("range", format!("bytes={start}-{end}"))];
    if let Some(s) = state.as_ref().filter(|_| start > 0) {
        probe_headers.push(("if-range", s.validator.clone()));
    }
    let response = ctx.get(&probe_headers).await?;
    let total = response_range_total(&response, start, end);
    let validator = response_validator(&response);
    if let Some(s) = &state {
        if total != Some(s.total) || validator.as_deref() != Some(s.validator.as_str()) {
            if response.status().as_u16() >= 400 {
                return Err(Failure::status(&response));
            }
            return Err(Failure::fatal("The server cannot safely resume the saved byte ranges. Keeping the partial file."));
        }
    }
    if let (Some(total), Some(validator)) = (total, validator) {
        if (state.is_some() || total - start >= range_size.saturating_mul(2)) && !is_html(&response) {
            let path = if state.is_none() {
                let path = ctx.output_path(&response, &path);
                File::create(&path).map_err(Failure::io)?;
                path
            } else {
                path
            };
            ctx.lock().progress.file_size = total;
            let connections = if parallel_disabled { 1 } else { CONNECTIONS };
            let saved = state.unwrap_or(RangeState { total, validator, resource_id: Some(resource_id), ranges: vec![] });
            return parallel(ctx, response, &path, saved, range_size, connections).await.map_err(|mut f| {
                f.parallel = true;
                f
            });
        }
    }
    if state.is_some() {
        return Err(Failure::fatal("The server cannot safely resume the saved byte ranges. Keeping the partial file."));
    }
    // Not range-capable: keep the response when it already holds the whole file.
    let whole = response.status().as_u16() != 206 || total.is_some_and(|t| end >= t - 1);
    let response = if whole { response } else { ctx.get(&[]).await? };
    stream(ctx, response, &path, 0, restarts).await
}

/// One connection writing from `start` (`executeDownload`'s single-stream path).
async fn stream(ctx: &Ctx, mut response: Response, path: &Path, start: u64, restarts: &mut u32) -> Result<(), Failure> {
    let status = response.status().as_u16();
    if status == 416 && start > 0 {
        let total = header(&response, "content-range").and_then(|v| v.trim().strip_prefix("bytes */").and_then(|t| t.parse::<u64>().ok()));
        if total == Some(start) {
            let mut inner = ctx.lock();
            inner.progress.file_size = start;
            inner.progress.bytes = start;
            return Ok(());
        }
        return Err(Failure::fatal(format!(
            "Range not satisfiable for resumed download (local={start}, remote={}). Keeping the partial file.",
            total.map_or("unknown".into(), |t| t.to_string())
        )));
    }
    if status >= 400 {
        return Err(Failure::status(&response));
    }
    if !(200..300).contains(&status) {
        return Err(Failure::fatal(format!("HTTP error! status: {status}")));
    }
    if is_html(&response) {
        return Err(Failure::fatal("The download link returned a web page instead of a file. It may have expired or be invalid."));
    }
    let encoded = is_encoded(&response);
    let partial_start = header(&response, "content-range").and_then(|v| content_range(&v)).map(|r| r.0);
    // (write position, body bytes to drop first) as `resolveResumeAction`.
    let (mut write_at, mut skip, mut restart) = (start, 0u64, false);
    if start > 0 {
        if encoded {
            restart = true;
        } else if status == 200 {
            // The server ignored Range and resends everything; keep the partial and drop its prefix.
            skip = start;
        } else {
            match partial_start {
                Some(p) if p > start => restart = true,
                Some(p) => skip = start - p,
                None => {}
            }
        }
    }
    if restart {
        *restarts += 1;
        if *restarts > MAX_RESTARTS_FROM_ZERO {
            return Err(Failure::fatal("The server keeps refusing to resume and the download cannot make progress; aborting to avoid endless re-downloads."));
        }
        (write_at, skip) = (0, 0);
        ctx.set_bytes(0);
    }
    let file_size = if encoded {
        0
    } else if let Some((_, _, total)) = header(&response, "content-range").and_then(|v| content_range(&v)) {
        total
    } else {
        header(&response, "content-length").and_then(|l| l.trim().parse::<u64>().ok()).map_or(0, |l| if status == 206 { start + l } else { l })
    };
    if file_size > 0 {
        ctx.lock().progress.file_size = file_size;
    }
    let path = if write_at == 0 { ctx.output_path(&response, path) } else { path.to_path_buf() };
    let mut file = OpenOptions::new().write(true).create(true).truncate(write_at == 0).open(&path).map_err(Failure::io)?;
    file.seek(SeekFrom::Start(write_at)).map_err(Failure::io)?;
    let (_never, mut no_abort) = watch::channel(false);
    while let Some(chunk) = next_chunk(ctx, &mut response, &mut no_abort).await? {
        let mut data = &chunk[..];
        if skip > 0 {
            let n = skip.min(data.len() as u64);
            skip -= n;
            data = &data[n as usize..];
        }
        if data.is_empty() {
            continue;
        }
        ctx.throttle(data.len() as u64).await;
        if ctx.stopped() {
            return Err(Failure::stopped());
        }
        file.write_all(data).map_err(Failure::io)?;
        ctx.add_bytes(data.len() as u64);
    }
    if skip > 0 {
        return Err(Failure::fatal(format!(
            "Server body shorter than the existing partial (missing {skip} bytes); refusing to append a truncated file."
        )));
    }
    file.flush().map_err(Failure::io)?;
    let length = file.metadata().map_err(Failure::io)?.len();
    if file_size > 0 && length != file_size {
        return Err(Failure::new(Kind::Network, "The download ended before its expected size; keeping the partial file."));
    }
    let mut inner = ctx.lock();
    inner.progress.file_size = length;
    inner.progress.bytes = length;
    Ok(())
}

/// `downloadParallelRanges`: workers take ranges from one queue and write them in place.
async fn parallel(ctx: &Ctx, first: Response, path: &Path, state: RangeState, range_size: u64, connections: usize) -> Result<(), Failure> {
    let (total, validator) = (state.total, state.validator.clone());
    let mut jobs = VecDeque::new();
    for (gap_start, gap_end) in state.missing() {
        let mut offset = gap_start;
        while offset < gap_end {
            let next = offset.saturating_add(range_size).min(gap_end);
            jobs.push_back((offset, next - 1));
            offset = next;
        }
    }
    let first_start = jobs.front().map(|j| j.0);
    let shared = Shared {
        jobs: Mutex::new(jobs),
        file: Mutex::new(OpenOptions::new().write(true).open(path).map_err(Failure::io)?),
        state: Mutex::new(state),
        first: Mutex::new(Some(first)),
        last_checkpoint: Mutex::new(Instant::now()),
    };
    shared.persist(ctx, path)?;
    let (abort, abort_rx) = watch::channel(false);
    let worker = |index: usize| {
        let (shared, abort, mut abort_rx) = (&shared, &abort, abort_rx.clone());
        let validator = validator.as_str();
        async move {
            if index >= connections {
                return Ok(());
            }
            let result = range_worker(ctx, shared, path, first_start, total, validator, &mut abort_rx).await;
            if result.is_err() {
                let _ = abort.send(true);
            }
            result
        }
    };
    let results = tokio::join!(worker(0), worker(1), worker(2), worker(3));
    let persisted = shared.persist(ctx, path);
    let missing = shared.lock_state().missing();
    let committed = shared.lock_state().saved();
    // The first real failure explains the stop; siblings only report that they were aborted.
    let failure = [results.0, results.1, results.2, results.3]
        .into_iter()
        .filter_map(Result::err)
        .min_by_key(|f| matches!(f.kind, Kind::Stopped) as u8);
    if let Some(failure) = failure {
        ctx.set_bytes(committed);
        return Err(failure);
    }
    persisted?;
    if !missing.is_empty() {
        return Err(Failure::new(Kind::Network, "Some byte ranges are still missing"));
    }
    remove_state(&ctx.ranges_dir, path);
    Ok(())
}

struct Shared {
    jobs: Mutex<VecDeque<(u64, u64)>>,
    file: Mutex<File>,
    state: Mutex<RangeState>,
    first: Mutex<Option<Response>>,
    last_checkpoint: Mutex<Instant>,
}

impl Shared {
    fn lock_state(&self) -> std::sync::MutexGuard<'_, RangeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Syncs the file before saving the map, so the map never claims bytes the disk has not seen.
    fn persist(&self, ctx: &Ctx, path: &Path) -> Result<(), Failure> {
        let snapshot = self.lock_state().clone();
        self.file.lock().unwrap_or_else(|e| e.into_inner()).sync_data().map_err(Failure::io)?;
        save_state(&ctx.ranges_dir, path, &snapshot)
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> Result<(), Failure> {
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        file.seek(SeekFrom::Start(offset)).map_err(Failure::io)?;
        file.write_all(data).map_err(Failure::io)
    }
}

async fn range_worker(
    ctx: &Ctx,
    shared: &Shared,
    path: &Path,
    first_start: Option<u64>,
    total: u64,
    validator: &str,
    abort: &mut watch::Receiver<bool>,
) -> Result<(), Failure> {
    loop {
        let Some((start, end)) = shared.jobs.lock().unwrap_or_else(|e| e.into_inner()).pop_front() else { return Ok(()) };
        if ctx.stopped() || *abort.borrow() {
            return Err(Failure::stopped());
        }
        let reused = if Some(start) == first_start { shared.first.lock().unwrap_or_else(|e| e.into_inner()).take() } else { None };
        let mut response = match reused {
            Some(r) => r,
            None => ctx.get(&[("range", format!("bytes={start}-{end}")), ("if-range", validator.to_string())]).await?,
        };
        if response_range_total(&response, start, end) != Some(total) || response_validator(&response).as_deref() != Some(validator) {
            if response.status().as_u16() >= 400 {
                return Err(Failure::status(&response));
            }
            return Err(Failure::new(Kind::RangesUnsupported, "The download server stopped serving matching byte ranges"));
        }
        let mut offset = start;
        while let Some(chunk) = next_chunk(ctx, &mut response, abort).await? {
            if offset + chunk.len() as u64 > end + 1 {
                return Err(Failure::new(Kind::RangesUnsupported, "The server sent more data than the requested byte range"));
            }
            ctx.throttle(chunk.len() as u64).await;
            if ctx.stopped() {
                return Err(Failure::stopped());
            }
            shared.write_at(offset, &chunk)?;
            shared.lock_state().mark(offset, offset + chunk.len() as u64);
            offset += chunk.len() as u64;
            ctx.add_bytes(chunk.len() as u64);
            let due = {
                let mut last = shared.last_checkpoint.lock().unwrap_or_else(|e| e.into_inner());
                let due = last.elapsed() >= CHECKPOINT_INTERVAL;
                if due {
                    *last = Instant::now();
                }
                due
            };
            if due {
                shared.persist(ctx, path)?;
            }
        }
        if offset != end + 1 {
            return Err(Failure::new(Kind::Network, "Byte range ended before its expected size"));
        }
    }
}

/// `validateJsDownloadResponse`: a 1-byte GET so a dead or expired link fails before the download
/// is queued. Transient statuses are retried, then let through for the downloader to retry.
pub async fn preflight(url: &str, headers: &[(String, String)]) -> Result<(), String> {
    let (mut attempt, mut bound_attempts) = (0, 0);
    while attempt < PREFLIGHT_ATTEMPTS {
        attempt += 1;
        // A client per attempt, so a retry opens a new connection (see `connection_bound`).
        let client = Client::builder().timeout(PREFLIGHT_TIMEOUT).build().map_err(|e| e.to_string())?;
        let mut request = client.get(url).header("user-agent", USER_AGENT).header("accept-encoding", "identity").header("range", "bytes=0-0");
        for (key, value) in headers {
            request = request.header(key.as_str(), value.as_str());
        }
        let response = request.send().await.map_err(|e| if e.is_timeout() { "Download URL validation timed out".to_string() } else { e.to_string() })?;
        let status = response.status().as_u16();
        if status == 416 {
            return Ok(());
        }
        if RETRYABLE_STATUS.contains(&status) {
            if attempt < PREFLIGHT_ATTEMPTS {
                tokio::time::sleep(Duration::from_secs(u64::from(attempt))).await;
                continue;
            }
            return Ok(());
        }
        if connection_bound(response.url(), status) && bound_attempts < BOUND_PREFLIGHT_ATTEMPTS {
            // Its own budget: the try does not count toward PREFLIGHT_ATTEMPTS.
            bound_attempts += 1;
            attempt -= 1;
            tokio::time::sleep(BOUND_RETRY_DELAY).await;
            continue;
        }
        if status >= 400 {
            return Err(format!("The download link is not available (HTTP {status})."));
        }
        if is_html(&response) {
            return Err("download_error_link_returned_web_page".into());
        }
        return Ok(());
    }
    Ok(())
}

// ---- Batches (the JS batch loop in download-manager.ts) ----

/// Several files downloaded one after another, each link unlocked just before its file starts.
/// A single hoster file is a batch of one.
pub struct Batch {
    shared: Arc<Mutex<BatchState>>,
    stop: watch::Sender<bool>,
    task: tokio::sync::Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

struct BatchState {
    current: Option<Arc<HttpDownload>>,
    done_bytes: u64,
    total: u64,
    /// The folder shown for the download; None shows the current file's name.
    folder: Option<String>,
    last_name: String,
    state: State,
    error: Option<String>,
    limit: Option<u64>,
    /// Multi-part hoster mirrors: each part's size once known, the part running, and the size
    /// shown until a part is seen. None for other batches, whose sizes are known up front.
    parts: Option<PartSizes>,
}

struct PartSizes {
    sizes: Vec<Option<u64>>,
    index: usize,
    fallback: u64,
}

impl Batch {
    /// `parts_fallback`: Some for a multi-part hoster mirror, with the size to show before any
    /// part is seen.
    pub fn start(app: tauri::AppHandle, entries: Vec<Entry>, folder: Option<String>, save_path: PathBuf, ranges_dir: PathBuf, limit: Option<u64>, parts_fallback: Option<u64>) -> Arc<Self> {
        // Unknown sizes (AllDebrid) leave the total to the files themselves.
        let total = if entries.iter().all(|e| e.size.is_some()) { entries.iter().filter_map(|e| e.size).sum() } else { 0 };
        let shared = Arc::new(Mutex::new(BatchState {
            current: None,
            done_bytes: 0,
            total,
            folder,
            last_name: String::new(),
            state: State::Active,
            error: None,
            limit,
            parts: parts_fallback.map(|fallback| PartSizes { sizes: vec![None; entries.len()], index: 0, fallback }),
        }));
        let (stop, stop_rx) = watch::channel(false);
        let task = tauri::async_runtime::spawn(run_batch(app, shared.clone(), entries, save_path, ranges_dir, stop_rx));
        Arc::new(Self { shared, stop, task: tokio::sync::Mutex::new(Some(task)) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BatchState> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn progress(&self) -> Progress {
        let s = self.lock();
        let current = s.current.as_ref().map(|j| j.progress()).unwrap_or_default();
        let bytes = s.done_bytes + current.bytes;
        let name = if current.folder_name.is_empty() { s.last_name.clone() } else { current.folder_name.clone() };
        let file_size = match &s.parts {
            Some(parts) => {
                let mut sizes = parts.sizes.clone();
                if let Some(size) = sizes.get_mut(parts.index).filter(|_| current.file_size > 0) {
                    *size = Some(current.file_size);
                }
                crate::hosters::estimate_parts_total(&sizes, parts.fallback).max(bytes)
            }
            None if s.total > 0 => s.total.max(bytes),
            None => s.done_bytes + current.file_size,
        };
        Progress {
            folder_name: s.folder.clone().unwrap_or(name),
            file_size,
            bytes,
            speed: if s.state == State::Active { current.speed } else { 0.0 },
            state: s.state,
            reconnecting: current.reconnecting,
            error: s.error.clone(),
        }
    }

    pub fn set_limit(&self, limit: Option<u64>) {
        let mut s = self.lock();
        s.limit = limit;
        if let Some(job) = &s.current {
            job.set_limit(limit);
        }
    }

    /// Pauses: stops the current file and waits, keeping what is on disk.
    pub async fn stop(&self) {
        let _ = self.stop.send(true);
        let current = self.lock().current.clone();
        if let Some(job) = current {
            job.stop().await;
        }
        if let Some(task) = self.task.lock().await.take() {
            let _ = task.await;
        }
    }

    /// Stops and deletes the unfinished file; finished files stay, as in Electron.
    pub async fn cancel(&self) {
        self.stop().await;
        let current = self.lock().current.clone();
        if let Some(job) = current {
            job.cancel().await;
        }
    }
}

/// Where a finished hoster part records its file, so a retry skips it without asking the hoster
/// again: its link may have expired by then.
pub(crate) fn receipt_path(dir: &Path, save_path: &Path, uri: &str) -> PathBuf {
    dir.join(format!("part-{}.json", sha256_hex(&format!("{}\n{uri}", save_path.to_string_lossy()))))
}

fn read_receipt(path: &Path) -> Option<(String, u64)> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    Some((v["path"].as_str()?.to_string(), v["size"].as_u64()?))
}

pub(crate) enum PartCheck {
    /// Finished earlier: its path and size.
    Done(String, u64),
    Download,
    Changed(String),
}

/// A part with a receipt is done while its file still has the recorded size. A file that changed
/// is never overwritten; a missing file downloads again.
pub(crate) fn check_part(receipt: &Path, save_path: &Path) -> PartCheck {
    let Some((path, size)) = read_receipt(receipt) else { return PartCheck::Download };
    match std::fs::metadata(save_path.join(&path)).map(|m| m.len()) {
        Ok(on_disk) if on_disk == size => PartCheck::Done(path, size),
        Ok(_) => PartCheck::Changed(path),
        Err(_) => {
            let _ = std::fs::remove_file(receipt);
            PartCheck::Download
        }
    }
}

/// The name a link gives its file in Content-Disposition (PixelDrain's `/u/<id>` links have none
/// in the URL).
pub async fn probe_filename(url: &str, headers: &[(String, String)]) -> Option<String> {
    let client = Client::builder().timeout(PREFLIGHT_TIMEOUT).build().ok()?;
    let mut request = client.get(url).header("user-agent", USER_AGENT).header("accept-encoding", "identity").header("range", "bytes=0-0");
    for (key, value) in headers {
        request = request.header(key.as_str(), value.as_str());
    }
    let response = request.send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    header(&response, "content-disposition").and_then(|v| disposition_filename(&v))
}

/// A hoster part's link, headers and path under the download folder.
async fn resolve_part(app: &tauri::AppHandle, entry: &Entry, used: &mut std::collections::HashSet<String>) -> Result<(String, Vec<(String, String)>, String), String> {
    let Link::Hoster { downloader, uri, folder, prepared } = &entry.link else { unreachable!("only hoster parts") };
    let resolved = match prepared {
        Some(resolved) => resolved.clone(),
        None => crate::hosters::resolve(app, *downloader, uri).await?,
    };
    let path = match &entry.path {
        Some(path) => path.clone(),
        None => {
            let name = match resolved.filename.clone() {
                Some(name) => Some(name),
                None => probe_filename(&resolved.url, &resolved.headers).await,
            };
            let name = name.map(|n| crate::hosters::sanitize(&n)).filter(|n| !n.trim().is_empty()).ok_or(crate::hosters::MULTIPART_NO_NAME)?;
            format!("{folder}/{name}")
        }
    };
    if !used.insert(path.to_lowercase()) {
        return Err("Two download parts have the same file name.".into());
    }
    Ok((resolved.url, resolved.headers, path))
}

async fn run_batch(app: tauri::AppHandle, shared: Arc<Mutex<BatchState>>, entries: Vec<Entry>, save_path: PathBuf, ranges_dir: PathBuf, stop: watch::Receiver<bool>) {
    let lock = || shared.lock().unwrap_or_else(|e| e.into_inner());
    let finish = |state: State, error: Option<String>| {
        let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
        s.state = state;
        if let Some(error) = error {
            eprintln!("[http-download] {error}");
            s.error = Some(error);
        }
    };
    let set_part_size = |index: usize, size: u64| {
        if let Some(parts) = lock().parts.as_mut() {
            parts.sizes[index] = Some(size);
        }
    };
    let mut used = std::collections::HashSet::new();
    for (index, entry) in entries.into_iter().enumerate() {
        if *stop.borrow() {
            return finish(State::Paused, None);
        }
        if let Some(parts) = lock().parts.as_mut() {
            parts.index = index;
        }
        let receipt = match &entry.link {
            Link::Hoster { uri, .. } => Some(receipt_path(&ranges_dir, &save_path, uri)),
            _ => None,
        };
        if let Some(receipt) = &receipt {
            match check_part(receipt, &save_path) {
                PartCheck::Done(path, size) => {
                    used.insert(path.to_lowercase());
                    set_part_size(index, size);
                    let mut s = lock();
                    s.done_bytes += size;
                    s.last_name = path;
                    continue;
                }
                PartCheck::Changed(path) => {
                    return finish(State::Error, Some(format!("The finished download part {path} changed on disk, so it was not downloaded again. Delete it to download it again.")));
                }
                PartCheck::Download => {}
            }
        }
        // A file already complete on disk (and not mid-range) is not fetched again.
        if let (Some(path), Some(size)) = (&entry.path, entry.size) {
            let file = save_path.join(path);
            let on_disk = std::fs::metadata(&file).map(|m| m.len()).ok();
            if size > 0 && on_disk == Some(size) && !state_path(&ranges_dir, &file).exists() {
                let mut s = lock();
                s.done_bytes += size;
                s.last_name = path.clone();
                continue;
            }
        }
        let mut stopped = stop.clone();
        let link = async {
            match &entry.link {
                Link::Hoster { .. } => resolve_part(&app, &entry, &mut used).await.map(|(url, headers, path)| (url, headers, Some(path))),
                link => crate::debrid::unlock(link).await.map(|url| (url, entry.headers.clone(), entry.path.clone())),
            }
        };
        let link = tokio::select! {
            _ = stopped.wait_for(|s| *s) => return finish(State::Paused, None),
            link = link => link,
        };
        let (url, headers, filename) = match link {
            Ok(link) => link,
            Err(e) => return finish(State::Error, Some(e)),
        };
        let options = Options { url, save_path: save_path.clone(), filename: filename.clone(), headers, resource_id: entry.resource_id.clone() };
        let limit = lock().limit;
        let job = match HttpDownload::start(options, ranges_dir.clone(), limit) {
            Ok(job) => job,
            Err(e) => return finish(State::Error, Some(e)),
        };
        lock().current = Some(job.clone());
        // A pause that arrived while the file was starting.
        if *stop.borrow() {
            job.stop().await;
        }
        job.wait().await;
        let p = job.progress();
        match p.state {
            State::Complete => {}
            State::Paused | State::Active => return finish(State::Paused, None),
            State::Error => return finish(State::Error, p.error.or_else(|| Some("Download failed".into()))),
        }
        if entry.exact_size && entry.size.is_some_and(|s| s != p.bytes) {
            return finish(State::Error, Some("A downloaded file returned fewer bytes than expected. Its link may have expired.".into()));
        }
        if let (Some(receipt), Some(path)) = (&receipt, &filename) {
            let written = std::fs::create_dir_all(&ranges_dir).and_then(|_| std::fs::write(receipt, json!({ "path": path, "size": p.bytes }).to_string()));
            if let Err(e) = written {
                eprintln!("[http-download] could not record the finished part {path}: {e}");
            }
            set_part_size(index, p.bytes);
        }
        let mut s = lock();
        s.done_bytes += p.bytes;
        s.last_name = p.folder_name;
        s.current = None;
    }
    finish(State::Complete, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finished_parts_are_skipped_kept_or_downloaded_again() {
        let dir = std::env::temp_dir().join(format!("chimaera-parts-{}", std::process::id()));
        let save = dir.join("save");
        std::fs::create_dir_all(save.join("Game")).unwrap();
        let receipt = receipt_path(&dir, &save, "https://host/f/1");
        assert_ne!(receipt, receipt_path(&dir, &dir.join("other"), "https://host/f/1"));
        assert!(matches!(check_part(&receipt, &save), PartCheck::Download));
        std::fs::write(save.join("Game/Game.part01.rar"), b"12345").unwrap();
        std::fs::write(&receipt, json!({ "path": "Game/Game.part01.rar", "size": 5 }).to_string()).unwrap();
        assert!(matches!(check_part(&receipt, &save), PartCheck::Done(p, 5) if p == "Game/Game.part01.rar"));
        std::fs::write(save.join("Game/Game.part01.rar"), b"123").unwrap();
        assert!(matches!(check_part(&receipt, &save), PartCheck::Changed(_)));
        std::fs::remove_file(save.join("Game/Game.part01.rar")).unwrap();
        assert!(matches!(check_part(&receipt, &save), PartCheck::Download));
        assert!(!receipt.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn marks_and_finds_gaps_like_electron() {
        let mut state = RangeState { total: 100, validator: "\"v\"".into(), resource_id: None, ranges: vec![] };
        state.mark(10, 20);
        state.mark(30, 40);
        state.mark(20, 30);
        assert_eq!(state.ranges, [(10, 40)]);
        state.mark(0, 5);
        assert_eq!(state.missing(), [(5, 10), (40, 100)]);
        assert_eq!(state.saved(), 35);
    }

    #[test]
    fn range_state_round_trips_and_rejects_bad_maps() {
        let state = RangeState { total: 100, validator: "\"abc\"".into(), resource_id: Some(sha256_hex("magnet")), ranges: vec![(0, 10), (20, 30)] };
        let text = state.to_json().to_string();
        assert_eq!(parse_state(&text, 30), Some(state));
        // The file is shorter than a saved range.
        assert_eq!(parse_state(&text, 25), None);
        assert_eq!(parse_state(r#"{"version":1,"total":10,"validator":"x","resourceId":null,"ranges":[[5,3]]}"#, 10), None);
    }

    #[test]
    fn state_file_name_matches_electron() {
        // sha256 of the lowercased absolute path, as range-download-state.ts on Windows.
        let dir = Path::new("ranges");
        let file = std::path::absolute("Some/File.RAR").unwrap();
        let mut identity = file.to_string_lossy().into_owned();
        if cfg!(windows) {
            identity = identity.to_lowercase();
        }
        assert_eq!(state_path(dir, &file), dir.join(format!("{}.json", sha256_hex(&identity))));
    }

    #[test]
    fn validators() {
        assert_eq!(strong_validator(Some("\"abc\""), None, None).as_deref(), Some("\"abc\""));
        assert_eq!(strong_validator(Some("W/\"abc\""), Some("Sun, 06 Nov 1994 08:49:37 GMT"), Some("Mon, 07 Nov 1994 08:49:37 GMT")), None);
        assert!(strong_validator(None, Some("Sun, 06 Nov 1994 08:49:37 GMT"), Some("Sun, 06 Nov 1994 08:50:37 GMT")).is_some());
        assert!(strong_validator(None, Some("Sun, 06 Nov 1994 08:49:37 GMT"), Some("Sun, 06 Nov 1994 08:50:36 GMT")).is_none());
        assert_eq!(http_date("Thu, 01 Jan 1970 00:00:10 GMT"), Some(10));
        assert_eq!(retry_after("5", 0), Some(Duration::from_secs(5)));
    }

    #[test]
    fn range_totals_must_match_the_request() {
        assert_eq!(range_total(206, Some("bytes 0-9/100"), Some("10"), false, 0, 9), Some(100));
        assert_eq!(range_total(206, Some("bytes 0-99/100"), None, false, 0, 199), Some(100));
        assert_eq!(range_total(206, Some("bytes 0-9/100"), Some("11"), false, 0, 9), None);
        assert_eq!(range_total(200, Some("bytes 0-9/100"), None, false, 0, 9), None);
        assert_eq!(range_total(206, Some("bytes 5-9/100"), None, false, 0, 9), None);
    }

    #[test]
    fn disposition_names() {
        assert_eq!(disposition_filename("attachment; filename=\"Game Setup.rar\"").as_deref(), Some("Game Setup.rar"));
        assert_eq!(disposition_filename("attachment; filename*=UTF-8''G%C3%A4me.zip; filename=\"x.zip\"").as_deref(), Some("Gäme.zip"));
        assert_eq!(disposition_filename("attachment; filename=\"../../evil.exe\"").as_deref(), Some("evil.exe"));
        assert_eq!(disposition_filename("inline"), None);
    }
}

