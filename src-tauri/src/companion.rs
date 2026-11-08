//! `companion:` scheme: game companion web apps (built from the `Chimaera-Brood` repo by
//! `scripts/fetch-companions.mjs` into the `companions` resource folder), shown in an
//! iframe on a game's page. `/terraria/index.html` serves `<resources>/companions/terraria/index.html`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tauri::http::{header, HeaderValue, Response, StatusCode};

use crate::image_cache::{cache_key, respond, status, CLIENT};

const USER_AGENT: &str = concat!("Chimaera/", env!("CARGO_PKG_VERSION"), " companion (Shaiyon ecosystem)");

// A graph asks for ~100 pictures on first open; a few at a time keeps the wiki from rate limiting us.
static IMAGE_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(6);

/// Wiki pages change rarely; a week on disk keeps a companion fast after a restart.
const API_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

const TYPES: &[(&str, &str)] = &[
    ("html", "text/html; charset=utf-8"),
    ("js", "text/javascript"),
    // Must be exact: WebAssembly.instantiateStreaming rejects other types.
    ("wasm", "application/wasm"),
    ("css", "text/css"),
    ("json", "application/json"),
    ("png", "image/png"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("svg", "image/svg+xml"),
    ("ico", "image/x-icon"),
    ("woff2", "font/woff2"),
];

/// The file under `root` a request path names, with its MIME type. Refuses anything
/// that could step outside `root`.
fn companion_file(root: &Path, path: &str) -> Option<(PathBuf, &'static str)> {
    let decoded = percent_encoding::percent_decode_str(path).decode_utf8().ok()?;
    let mut file = root.to_path_buf();
    for part in decoded.split('/').filter(|p| !p.is_empty()) {
        if part == "." || part == ".." || part.contains(['\\', ':']) {
            return None;
        }
        file.push(part);
    }
    let ext = file.extension()?.to_str()?.to_ascii_lowercase();
    let mime = TYPES.iter().find(|(e, _)| *e == ext)?.1;
    Some((file, mime))
}

/// APIs a companion reaches through this scheme: (request path, upstream, allowed `action`s).
/// The webview cannot call them directly: wiki.gg's Cloudflare challenges browser user agents.
/// Only read-only actions pass, so this is not an open proxy.
const PROXIES: &[(&str, &str, &[&str])] =
    &[("/terraria/_wiki/api.php", "https://terraria.wiki.gg/api.php", &["parse", "opensearch"])];

/// Wiki pictures: (request path prefix, upstream prefix). wiki.gg refuses images hotlinked
/// from other sites, so the app fetches them itself. The rest of the path is one file name.
const FILE_PROXIES: &[(&str, &str)] = &[("/terraria/_wiki/images/", "https://terraria.wiki.gg/images/")];

pub struct ProxyTarget {
    pub url: String,
    /// Only an image response is passed on.
    pub image: bool,
}

/// The upstream for a proxied companion request, or None if the request is not one.
pub fn proxy_target(path: &str, query: Option<&str>) -> Option<ProxyTarget> {
    if let Some((prefix, upstream)) = FILE_PROXIES.iter().find(|(p, _)| path.starts_with(p)) {
        let name = &path[prefix.len()..];
        let ext = name.rsplit('.').next()?.to_ascii_lowercase();
        if name.is_empty() || name.contains(['/', '\\']) || query.is_some() || !["png", "gif"].contains(&ext.as_str()) {
            return None;
        }
        return Some(ProxyTarget { url: format!("{upstream}{name}"), image: true });
    }
    let (_, upstream, actions) = PROXIES.iter().find(|(p, _, _)| *p == path)?;
    let query = query?;
    let mut found = url::form_urlencoded::parse(query.as_bytes()).filter(|(k, _)| k == "action");
    let action = found.next()?.1;
    // MediaWiki reads the last duplicate key, so a second `action` could slip past the check.
    if found.next().is_some() || !actions.contains(&action.as_ref()) {
        return None;
    }
    Some(ProxyTarget { url: format!("{upstream}?{query}"), image: false })
}

/// Disk cache file for an API response, under `<app data>/companion-cache`.
fn api_cache_file(app_data: &Path, url: &str) -> PathBuf {
    app_data.join("companion-cache").join(format!("{}.json", cache_key(url)))
}

fn read_api_cache(file: &Path) -> Option<Vec<u8>> {
    let age = std::fs::metadata(file).ok()?.modified().ok()?;
    if SystemTime::now().duration_since(age).ok()? > API_MAX_AGE {
        return None;
    }
    std::fs::read(file).ok()
}

/// A JSON API response kept for the webview; the companion asks again at most daily.
fn cached_json(body: Vec<u8>) -> Response<Vec<u8>> {
    let mut out = respond(StatusCode::OK, "application/json; charset=utf-8", body);
    out.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("max-age=86400"));
    out
}

pub async fn proxy(app_data: Option<PathBuf>, target: &ProxyTarget) -> Response<Vec<u8>> {
    let cache = app_data.map(|dir| api_cache_file(&dir, &target.url));
    if let Some(body) = cache.as_deref().and_then(read_api_cache) {
        return cached_json(body);
    }
    let response = CLIENT
        .get(&target.url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await;
    let Ok(response) = response else { return not_stored(StatusCode::BAD_GATEWAY) };
    let code = StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    match response.bytes().await {
        // MediaWiki reports errors with HTTP 200 and an "error" key; those are not kept.
        Ok(body) if code == StatusCode::OK && !body.starts_with(b"{\"error\"") => {
            if let Some(file) = &cache {
                // A failed write only costs a refetch.
                let _ = std::fs::create_dir_all(file.parent().unwrap()).and_then(|_| std::fs::write(file, &body));
            }
            cached_json(body.to_vec())
        }
        Ok(body) => {
            let mut out = respond(code, &mime, body.to_vec());
            out.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            out
        }
        Err(_) => not_stored(StatusCode::BAD_GATEWAY),
    }
}

/// A wiki picture through Chimaera's image disk cache; anything but an image is refused.
pub async fn proxy_image(cache_dir: Option<PathBuf>, target: &ProxyTarget) -> Response<Vec<u8>> {
    let Some(dir) = cache_dir else { return not_stored(StatusCode::INTERNAL_SERVER_ERROR) };
    let Ok(_slot) = IMAGE_SLOTS.acquire().await else { return not_stored(StatusCode::SERVICE_UNAVAILABLE) };
    // The wiki occasionally resets a connection; two retries with backoff cover it.
    for attempt in 0..3u32 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(500 << attempt)).await;
        }
        let response = crate::image_cache::serve(&dir, &target.url, Some(USER_AGENT)).await;
        let is_image = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t.starts_with("image/"));
        if response.status().is_success() && is_image {
            return response;
        }
    }
    not_stored(StatusCode::NOT_FOUND)
}

/// An error the webview must not cache: `status()` responses are marked immutable for a
/// year, which would keep a picture blank after one failed fetch.
fn not_stored(code: StatusCode) -> Response<Vec<u8>> {
    let mut response = status(code);
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// A bundled copy of a proxied picture (`_wiki/images` in the companion build), if the
/// build shipped one: served from disk with no network.
pub fn bundled(root: &Path, path: &str) -> Option<Response<Vec<u8>>> {
    let (file, mime) = companion_file(root, path)?;
    let bytes = std::fs::read(file).ok()?;
    let mut response = respond(StatusCode::OK, mime, bytes);
    // Pictures never change within a build; a new build gets a new webview cache key anyway.
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("max-age=604800"));
    Some(response)
}

pub fn handle(root: &Path, path: &str) -> Response<Vec<u8>> {
    let Some((file, mime)) = companion_file(root, path) else { return status(StatusCode::FORBIDDEN) };
    match std::fs::read(&file) {
        Ok(bytes) => {
            let mut response = respond(StatusCode::OK, mime, bytes);
            // A new build replaces index.html in place.
            response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            response
        }
        Err(_) => status(StatusCode::NOT_FOUND),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_only_known_files_inside_root() {
        let root = Path::new("res").join("companions");
        let (file, mime) = companion_file(&root, "/terraria%2Findex.html").unwrap();
        assert_eq!(file, root.join("terraria").join("index.html"));
        assert_eq!(mime, "text/html; charset=utf-8");
        assert_eq!(companion_file(&root, "/terraria/assets/app.wasm").unwrap().1, "application/wasm");
        for bad in ["/terraria/../../chimaera.db", "/%2E%2E/x.html", "/terraria/..%5Cx.html", "/C:/x.html", "/terraria/readme", "/x.exe"] {
            assert!(companion_file(&root, bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn serves_bundled_pictures_before_the_proxy() {
        let root = std::env::temp_dir().join("chimaera-companion-bundle-test");
        let dir = root.join("terraria").join("_wiki").join("images");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Night's_Edge.png"), b"png").unwrap();
        let hit = bundled(&root, "/terraria/_wiki/images/Night's_Edge.png").unwrap();
        assert_eq!(hit.body(), b"png");
        assert_eq!(hit.headers()[header::CONTENT_TYPE], "image/png");
        assert!(bundled(&root, "/terraria/_wiki/images/Missing.png").is_none());
        assert!(bundled(&root, "/terraria/_wiki/images/../../../x.png").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn api_cache_is_per_url_and_expires() {
        let dir = std::env::temp_dir().join("chimaera-companion-cache-test");
        let a = api_cache_file(&dir, "https://terraria.wiki.gg/api.php?action=parse&page=A");
        let b = api_cache_file(&dir, "https://terraria.wiki.gg/api.php?action=parse&page=B");
        assert_ne!(a, b);
        std::fs::create_dir_all(a.parent().unwrap()).unwrap();
        std::fs::write(&a, b"{}").unwrap();
        assert_eq!(read_api_cache(&a).as_deref(), Some(&b"{}"[..]));
        let old = SystemTime::now() - API_MAX_AGE - Duration::from_secs(60);
        std::fs::File::options().write(true).open(&a).unwrap().set_modified(old).unwrap();
        assert!(read_api_cache(&a).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn proxies_only_read_actions_on_known_paths() {
        let path = "/terraria/_wiki/api.php";
        assert_eq!(
            proxy_target(path, Some("action=parse&page=Zenith")).map(|t| t.url).as_deref(),
            Some("https://terraria.wiki.gg/api.php?action=parse&page=Zenith")
        );
        assert!(proxy_target(path, Some("action=opensearch&search=x")).is_some());
        assert!(proxy_target(path, Some("action=edit&title=x")).is_none());
        assert!(proxy_target(path, Some("action=parse&action=edit")).is_none());
        assert!(proxy_target(path, Some("page=x")).is_none());
        assert!(proxy_target(path, None).is_none());
        assert!(proxy_target("/other/_wiki/api.php", Some("action=parse")).is_none());

        let file = proxy_target("/terraria/_wiki/images/Night's_Edge.png", None).unwrap();
        assert!(file.image);
        assert_eq!(file.url, "https://terraria.wiki.gg/images/Night's_Edge.png");
        assert!(proxy_target("/terraria/_wiki/images/a/b.png", None).is_none());
        assert!(proxy_target("/terraria/_wiki/images/x.php", None).is_none());
        assert!(proxy_target("/terraria/_wiki/images/x.png", Some("a=b")).is_none());
    }
}
