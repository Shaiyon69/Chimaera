//! `cached-image:` scheme: catalogue artwork kept on disk so the library renders
//! offline. Same keys, layout and size cap as Electron's services/image-cache.ts.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sha2::{Digest, Sha256};
use tauri::http::{header, Response, StatusCode};

const MAX_CACHE_BYTES: u64 = 1024 * 1024 * 1024;

const EXTENSIONS: &[(&str, &str)] = &[
    ("image/webp", "webp"),
    ("image/png", "png"),
    ("image/jpeg", "jpg"),
    ("image/jpg", "jpg"),
    ("image/gif", "gif"),
    ("image/avif", "avif"),
    ("image/svg+xml", "svg"),
    ("image/x-icon", "ico"),
    ("image/vnd.microsoft.icon", "ico"),
];

pub fn cache_dir(app_data: &Path) -> PathBuf {
    app_data.join("image-cache").join("assets")
}

fn cache_key(url: &str) -> String {
    format!("{:x}", Sha256::digest(url.as_bytes()))
}

/// File extension for an image content type; None for anything that is not an image.
fn extension_for(content_type: &str) -> Option<&'static str> {
    let mime = content_type.split(';').next()?.trim().to_ascii_lowercase();
    EXTENSIONS.iter().find(|(m, _)| *m == mime).map(|(_, ext)| *ext)
}

fn mime_for(ext: &str) -> &'static str {
    EXTENSIONS.iter().find(|(_, e)| *e == ext).map_or("application/octet-stream", |(m, _)| m)
}

/// The original http(s) URL from a request path (`/<encodeURIComponent(url)>`).
fn original_url(path: &str) -> Option<String> {
    let decoded = percent_encoding::percent_decode_str(path.trim_start_matches('/')).decode_utf8().ok()?;
    let parsed = url::Url::parse(&decoded).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| decoded.into_owned())
}

fn find_cached(dir: &Path, key: &str) -> Option<PathBuf> {
    EXTENSIONS.iter().map(|(_, ext)| dir.join(format!("{key}.{ext}"))).find(|file| file.is_file())
}

pub(crate) fn respond(status: StatusCode, content_type: &str, body: Vec<u8>) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
        // Animated covers are drawn to a canvas for their poster frame.
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .body(body)
        .expect("static response parts are valid")
}

pub fn status(status: StatusCode) -> Response<Vec<u8>> {
    respond(status, "text/plain", Vec::new())
}

fn store(dir: &Path, key: &str, ext: &str, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let file = dir.join(format!("{key}.{ext}"));
    // Unique temp name: two requests for one image may download it at once.
    let temp = dir.join(format!("{key}.{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, &file)
}

/// Serves from disk, fetching once on a miss. Non-images pass straight through uncached.
pub async fn handle(dir: &Path, path: &str) -> Response<Vec<u8>> {
    let Some(url) = original_url(path) else { return status(StatusCode::BAD_REQUEST) };
    let key = cache_key(&url);

    if let Some(file) = find_cached(dir, &key) {
        if let Ok(bytes) = std::fs::read(&file) {
            // Touch for the size cap's least-recently-used order.
            let _ = std::fs::File::options().write(true).open(&file).and_then(|f| f.set_modified(SystemTime::now()));
            let ext = file.extension().and_then(|e| e.to_str()).unwrap_or_default();
            return respond(StatusCode::OK, mime_for(ext), bytes);
        }
    }

    let response = match reqwest::get(&url).await {
        Ok(response) if response.status().is_success() => response,
        _ => return status(StatusCode::NOT_FOUND),
    };
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let Ok(bytes) = response.bytes().await else { return status(StatusCode::NOT_FOUND) };
    if let Some(ext) = extension_for(&content_type) {
        if let Err(error) = store(dir, &key, ext, &bytes) {
            eprintln!("[image-cache] write failed for {url}: {error}");
        }
    }
    respond(StatusCode::OK, &content_type, bytes.to_vec())
}

/// Files to delete so the cache fits `max_bytes`, least recently used first.
fn files_to_evict(mut files: Vec<(PathBuf, u64, SystemTime)>, max_bytes: u64) -> Vec<PathBuf> {
    let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
    files.sort_by_key(|(_, _, used)| *used);
    let mut evict = Vec::new();
    for (path, size, _) in files {
        if total <= max_bytes {
            break;
        }
        evict.push(path);
        total -= size;
    }
    evict
}

/// Startup sweep: keeps the cache under 1 GB, dropping the least recently used.
pub fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let files = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let meta = entry.metadata().ok().filter(|m| m.is_file())?;
            Some((entry.path(), meta.len(), meta.modified().ok()?))
        })
        .collect();
    for file in files_to_evict(files, MAX_CACHE_BYTES) {
        let _ = std::fs::remove_file(file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn original_url_accepts_only_encoded_http_urls() {
        assert_eq!(original_url("/https%3A%2F%2Fcdn.example%2Fa.png%3Fx%3D1").as_deref(), Some("https://cdn.example/a.png?x=1"));
        assert_eq!(original_url("/HTTP%3A%2F%2Fexample.com%2F").as_deref(), Some("HTTP://example.com/"));
        assert_eq!(original_url("/file%3A%2F%2F%2FC%3A%2Fx.png"), None);
        assert_eq!(original_url("/not-a-url"), None);
    }

    #[test]
    fn cache_key_matches_electron() {
        // createHash("sha256").update("https://example.com/a.png").digest("hex")
        assert_eq!(cache_key("https://example.com/a.png"), "494a30704d4f32ac0b81739d18a66d3638d440cbc6f5669f6af66f840edee5ab");
    }

    #[test]
    fn extension_ignores_parameters_and_rejects_non_images() {
        assert_eq!(extension_for("image/JPEG; charset=binary"), Some("jpg"));
        assert_eq!(extension_for("video/mp4"), None);
        assert_eq!(mime_for("jpg"), "image/jpeg");
    }

    #[test]
    fn evicts_least_recently_used_until_under_cap() {
        let at = |s| SystemTime::UNIX_EPOCH + Duration::from_secs(s);
        let files = vec![
            (PathBuf::from("new"), 40, at(3)),
            (PathBuf::from("old"), 40, at(1)),
            (PathBuf::from("mid"), 40, at(2)),
        ];
        assert_eq!(files_to_evict(files, 80), vec![PathBuf::from("old")]);
    }
}
