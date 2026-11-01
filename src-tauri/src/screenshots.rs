//! Achievement souvenirs, kept on disk only: a capture of the game's window when an
//! achievement unlocks. Ported from src/main/services/screenshot.ts and
//! achievement-screenshot-path.ts.
// shortcut: no Hydra upload or sync (no Hydra sign-in in this build); the sync channels report idle.

use std::path::{Path, PathBuf};

use image::RgbaImage;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};

use crate::store::Store;

const QUALITY: u8 = 80;
const MAX_HEIGHT: u32 = 1080;
const MAX_STORED: usize = 50;
/// Paths this app captured, so pruning never touches the user's own files in a custom folder.
const KIND: &str = "souvenirScreenshots";

/// `getScreenshotsPath`: the `achievementScreenshotsPath` preference, else `Screenshots` in the app data.
pub fn dir(app: &AppHandle) -> Result<PathBuf, String> {
    let preferences = app.state::<Store>().get("", "userPreferences")?;
    match preferences["achievementScreenshotsPath"].as_str().filter(|p| !p.trim().is_empty()) {
        Some(path) => Ok(PathBuf::from(path)),
        None => crate::save_backups::data_dir(app, "Screenshots"),
    }
}

/// `isAchievementSouvenirsEnabled`: on by default except on Linux.
pub fn enabled(preferences: &Value) -> bool {
    preferences["enableAchievementSouvenirs"].as_bool().unwrap_or(!cfg!(target_os = "linux"))
}

/// The idle answers of the Electron sync channels when nobody is signed in.
pub fn idle_status() -> Value {
    json!({ "pendingCount": 0, "failedCount": 0, "errorCodes": [] })
}

fn strip_trailing_dots_and_spaces(value: &str) -> &str {
    value.trim_end_matches(['.', ' '])
}

/// sanitizePathSegment: drops control characters, replaces the ones Windows forbids, caps at 120.
fn sanitize(value: &str) -> String {
    let cleaned: String = value.chars().filter(|c| *c as u32 > 31).map(|c| if "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect();
    let capped: String = strip_trailing_dots_and_spaces(&cleaned).chars().take(120).collect();
    let trimmed = capped.trim();
    if trimmed.is_empty() { "unknown".into() } else { trimmed.to_string() }
}

fn stable_suffix(value: &str) -> String {
    Sha256::digest(value.as_bytes()).iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// resolveAchievementScreenshotPath.
pub fn path_for(root: &Path, game_title: &str, achievement_name: &str, game_id: &str, achievement_id: &str) -> PathBuf {
    root.join(format!("{}-{}", sanitize(game_title), stable_suffix(game_id)))
        .join(format!("{}-{}.jpeg", sanitize(achievement_name), stable_suffix(achievement_id)))
}

/// fitScreenshotTo1080p.
fn fit(width: u32, height: u32) -> (u32, u32) {
    if height <= MAX_HEIGHT {
        return (width, height);
    }
    ((width as f64 * MAX_HEIGHT as f64 / height as f64).round() as u32, MAX_HEIGHT)
}

/// isNearlyUniformScreenshot || isMostlyBlackScreenshot, on a 32x32 sample.
fn is_blank(image: &RgbaImage) -> bool {
    let sample = image::imageops::thumbnail(image, 32, 32);
    let pixels: Vec<[u8; 3]> = sample.pixels().map(|p| [p[0], p[1], p[2]]).collect();
    if pixels.is_empty() {
        return true;
    }
    let uniform = (0..3).all(|c| {
        let (min, max) = pixels.iter().fold((255u8, 0u8), |(lo, hi), p| (lo.min(p[c]), hi.max(p[c])));
        max - min <= 8
    });
    let near_black = pixels.iter().filter(|p| p.iter().all(|v| *v <= 16)).count() as f64 / pixels.len() as f64;
    let average = pixels.iter().flat_map(|p| p.iter()).map(|v| *v as f64).sum::<f64>() / (pixels.len() * 3) as f64;
    uniform || (near_black >= 0.98 && average <= 8.0)
}

fn save_jpeg(image: &RgbaImage, path: &Path) -> Result<(), String> {
    let (width, height) = fit(image.width(), image.height());
    let rgb = image::DynamicImage::ImageRgba8(image::imageops::resize(image, width, height, image::imageops::FilterType::Triangle)).to_rgb8();
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).map_err(|e| e.to_string())?;
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), QUALITY).encode_image(&rgb).map_err(|e| e.to_string())
}

/// The game's window: one owned by a process running the game's executable or tracking paths.
/// The focused one wins, then the largest.
#[cfg(windows)]
fn grab_game_window(game: &Value) -> Result<RgbaImage, String> {
    use std::collections::HashSet;
    let mut system = sysinfo::System::new();
    system.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, true, sysinfo::ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::Always));
    let pids: HashSet<u32> = system
        .processes()
        .iter()
        .filter(|(_, p)| p.exe().is_some_and(|exe| crate::watcher::game_matches(game, &HashSet::from([crate::watcher::path_key(&exe.to_string_lossy())]))))
        .map(|(pid, _)| pid.as_u32())
        .collect();
    if pids.is_empty() {
        return Err("the game is not running".into());
    }
    let window = xcap::Window::all()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|w| w.pid().is_ok_and(|pid| pids.contains(&pid)) && !w.is_minimized().unwrap_or(true))
        .filter(|w| w.width().unwrap_or(0) > 0 && w.height().unwrap_or(0) > 0)
        .max_by_key(|w| (w.is_focused().unwrap_or(false), w.width().unwrap_or(0) as u64 * w.height().unwrap_or(0) as u64))
        .ok_or("the game has no visible window")?;
    window.capture_image().map_err(|e| e.to_string())
}

// shortcut: Linux (X11/Wayland) and macOS capture are not ported; souvenirs are off there.
#[cfg(not(windows))]
fn grab_game_window(_game: &Value) -> Result<RgbaImage, String> {
    Err("souvenir capture is only ported on Windows".into())
}

/// captureGameScreenshot for the first new unlock of `game`, then keeps the newest 50 captures.
pub fn capture(app: &AppHandle, game: &Value, achievement_name: &str) -> Result<PathBuf, String> {
    let image = grab_game_window(game)?;
    if is_blank(&image) {
        return Err("the captured frame is blank".into());
    }
    let game_id = format!("{}:{}", game["shop"].as_str().unwrap_or_default(), game["objectId"].as_str().unwrap_or_default());
    let path = path_for(&dir(app)?, game["title"].as_str().unwrap_or_default(), achievement_name, &game_id, &uuid::Uuid::new_v4().to_string());
    save_jpeg(&image, &path)?;
    let store = app.state::<Store>();
    store.put(KIND, &path.to_string_lossy(), &json!({ "capturedAt": crate::library::now_ms() }))?;
    prune(&store)?;
    Ok(path)
}

/// cleanupOldScreenshots, over the captures this app recorded.
fn prune(store: &Store) -> Result<(), String> {
    let mut all = store.entries(KIND)?;
    all.sort_by_key(|(_, v)| std::cmp::Reverse(v["capturedAt"].as_i64().unwrap_or(0)));
    for (path, _) in all.into_iter().skip(MAX_STORED) {
        if std::fs::remove_file(&path).is_ok() || !Path::new(&path).exists() {
            store.del(KIND, &path)?;
            if let Some(parent) = Path::new(&path).parent() {
                // Only succeeds when the game's folder is now empty.
                let _ = std::fs::remove_dir(parent);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_match_electron() {
        assert_eq!(sanitize("Dark: Souls?. "), "Dark_ Souls_");
        assert_eq!(sanitize(" \u{1}.. "), "unknown");
        assert_eq!(sanitize(&"a".repeat(200)).len(), 120);
        assert_eq!(stable_suffix("abc"), "ba7816bf8f01");
        let path = path_for(Path::new("S"), "Game", "Win", "g", "a");
        assert_eq!(path.parent().unwrap().file_name().unwrap().to_str().unwrap(), format!("Game-{}", stable_suffix("g")));
        assert!(path.to_string_lossy().ends_with(".jpeg"));
    }

    #[test]
    fn fits_to_1080p_and_spots_blank_frames() {
        assert_eq!(fit(3840, 2160), (1920, 1080));
        assert_eq!(fit(1280, 720), (1280, 720));
        assert!(is_blank(&RgbaImage::from_pixel(64, 64, image::Rgba([3, 3, 3, 255]))));
        let mut frame = RgbaImage::from_pixel(64, 64, image::Rgba([10, 10, 10, 255]));
        for x in 0..32 {
            for y in 0..64 {
                frame.put_pixel(x, y, image::Rgba([200, 120, 40, 255]));
            }
        }
        assert!(!is_blank(&frame));
    }

    /// Grabs a live Explorer window: `cargo test captures_a_live_window -- --ignored`.
    #[test]
    #[ignore]
    #[cfg(windows)]
    fn captures_a_live_window() {
        let image = grab_game_window(&json!({ "executablePath": r"C:\Windows\explorer.exe" })).unwrap();
        assert!(image.width() > 0 && !is_blank(&image));
        let out = std::env::temp_dir().join("chimaera-souvenir-test.jpeg");
        save_jpeg(&image, &out).unwrap();
        eprintln!("saved {}", out.display());
    }

    #[test]
    fn prune_keeps_the_newest() {
        let store = Store::memory().unwrap();
        for i in 0..(MAX_STORED + 2) {
            store.put(KIND, &format!("missing-{i}.jpeg"), &json!({ "capturedAt": i })).unwrap();
        }
        prune(&store).unwrap();
        let kept = store.entries(KIND).unwrap();
        assert_eq!(kept.len(), MAX_STORED);
        assert!(kept.iter().all(|(p, _)| p != "missing-0.jpeg" && p != "missing-1.jpeg"));
    }
}
