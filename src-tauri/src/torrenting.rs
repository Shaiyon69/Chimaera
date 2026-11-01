//! Torrent channels, ported from src/main/events/torrenting/.

use serde_json::{json, Value};
use tauri::AppHandle;

use crate::{torrent, trackers};

/// DownloadError values (src/shared/constants.ts) for engine error codes.
fn torrent_files_error(code: &str) -> &'static str {
    match code {
        "invalid_magnet" => "download_error_invalid_magnet",
        "metadata_timeout" | "metadata_busy" => "download_error_torrent_metadata_timeout",
        "metadata_incomplete" => "download_error_torrent_metadata_incomplete",
        "too_many_files" => "download_error_torrent_too_many_files",
        "invalid_trackers" => "download_error_torrent_invalid_trackers",
        _ => "download_error_torrent_files_unavailable",
    }
}

/// `getTorrentFiles(magnet)`: the file list from the torrent's metadata, fetched from peers.
pub async fn get_torrent_files(app: &AppHandle, magnet: &str) -> Value {
    if !magnet.starts_with("magnet:") {
        return json!({ "ok": false, "error": torrent_files_error("invalid_magnet") });
    }
    let result = match trackers::global_trackers(app) {
        Ok(trackers) => torrent::request("torrent_files", json!({ "magnet": magnet, "timeout_ms": 45_000, "trackers": trackers }), 45_000).await,
        Err(e) => Err(e),
    };
    match result {
        Ok(data) => json!({ "ok": true, "data": data }),
        Err(code) => json!({ "ok": false, "error": torrent_files_error(&code) }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_engine_codes() {
        assert_eq!(torrent_files_error("metadata_busy"), "download_error_torrent_metadata_timeout");
        assert_eq!(torrent_files_error("Torrent bridge failed"), "download_error_torrent_files_unavailable");
    }
}
