//! `local:` scheme: files on disk the renderer shows (profile pictures, custom
//! and bundled game art), like Electron's protocol.handle("local").

use std::path::PathBuf;

use tauri::http::{header, HeaderValue, Response, StatusCode};

use crate::image_cache::{respond, status};

/// Only media is served: the webview also loads remote content, which must not
/// read arbitrary files (the store, keys) through this scheme.
const MEDIA_TYPES: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("webp", "image/webp"),
    ("gif", "image/gif"),
    ("avif", "image/avif"),
    ("svg", "image/svg+xml"),
    ("ico", "image/x-icon"),
    ("bmp", "image/bmp"),
    ("mp4", "video/mp4"),
    ("webm", "video/webm"),
];

/// The absolute media file a request path (`/<encodeURIComponent(path)>`) names, with its MIME type.
fn media_file(path: &str) -> Option<(PathBuf, &'static str)> {
    let decoded = percent_encoding::percent_decode_str(path.trim_start_matches('/')).decode_utf8().ok()?;
    let file = PathBuf::from(decoded.as_ref());
    if !file.is_absolute() {
        return None;
    }
    let ext = file.extension()?.to_str()?.to_ascii_lowercase();
    let mime = MEDIA_TYPES.iter().find(|(e, _)| *e == ext)?.1;
    Some((file, mime))
}

pub fn handle(path: &str) -> Response<Vec<u8>> {
    let Some((file, mime)) = media_file(path) else { return status(StatusCode::FORBIDDEN) };
    match std::fs::read(&file) {
        Ok(bytes) => {
            let mut response = respond(StatusCode::OK, mime, bytes);
            // Unlike catalogue images, these can be replaced in place (e.g. dev resources).
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
    fn serves_only_absolute_media_paths() {
        let (file, mime) = media_file(if cfg!(windows) { "/C%3A%5CUsers%5Cme%5Cavatar.WEBP" } else { "/%2Fhome%2Fme%2Favatar.WEBP" }).unwrap();
        assert!(file.ends_with("avatar.WEBP"));
        assert_eq!(mime, "image/webp");
        assert!(media_file("/relative%2Fa.png").is_none());
        let db = if cfg!(windows) { "/C%3A%5Cdata%5Cchimaera.db" } else { "/%2Fdata%2Fchimaera.db" };
        assert!(media_file(db).is_none());
    }
}
