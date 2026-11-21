//! `chimaera://run` deep links and desktop / Start Menu game shortcuts, ported from
//! src/main/helpers/shortcut-launch.ts, handleDeepLinkPath in src/main/index.ts and
//! src/main/events/library/create-game-shortcut.ts.

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use image::ImageFormat;
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::games::game_key;
use crate::steam_shortcuts::{convert, fetch_asset};
use crate::store::Store;
use crate::data_location::DataDir;

const SCHEME: &str = "chimaera";
// Hydra's scheme; old shortcuts still pass it to our exe.
const LEGACY_SCHEME: &str = "hydralauncher";

/// The first command-line argument that is one of our deep links.
pub fn deep_link_arg(args: &[String]) -> Option<&str> {
    args.iter().map(String::as_str).find(|a| a.starts_with(&format!("{SCHEME}://")) || a.starts_with(&format!("{LEGACY_SCHEME}://")))
}

/// buildRunDeepLink.
pub fn run_link(shop: &str, object_id: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new()).append_pair("shop", shop).append_pair("objectId", object_id).finish();
    format!("{SCHEME}://run?{query}")
}

/// (shop, objectId) of a `run` deep link.
pub fn run_target(uri: &str) -> Option<(String, String)> {
    let url = url::Url::parse(uri).ok()?;
    if url.host_str() != Some("run") {
        return None;
    }
    let get = |key: &str| url.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned()).filter(|v| !v.is_empty());
    Some((get("shop")?, get("objectId")?))
}

fn prefers_hidden_on_game_start(app: &AppHandle) -> bool {
    app.state::<Store>().get("", "userPreferences").ok().is_some_and(|p| p["hideToTrayOnGameStart"].as_bool() == Some(true))
}

fn focus_main(app: &AppHandle) {
    if crate::big_picture::focus_if_open(app) {
        return;
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// second-instance: bring the window up, unless a run link asks to stay out of the way.
// shortcut: install-source, profile and install-theme links are ignored until themes and profiles are ported.
pub fn handle_second_instance(app: &AppHandle, args: &[String]) {
    let link = deep_link_arg(args).map(String::from);
    let run = link.as_deref().and_then(run_target);
    if run.is_none() || !prefers_hidden_on_game_start(app) {
        focus_main(app);
    }
    if let Some((shop, object_id)) = run {
        let app = app.clone();
        tauri::async_runtime::spawn(async move { run_game(&app, &shop, &object_id).await });
    }
}

/// handleRunGame: classics start through their emulator, shop games through their exe.
pub async fn run_game(app: &AppHandle, shop: &str, object_id: &str) {
    let store = app.state::<Store>();
    let key = game_key(shop, object_id);
    let mut game = store.get("games", &key).unwrap_or(Value::Null);
    if shop == "launchbox" && game.is_object() {
        // lookupCachedPlatform: older imports may only have the platform in the shop cache.
        if game["platform"].as_str().is_none_or(str::is_empty) {
            let prefix = format!("{key}:");
            let cached = store.entries("gameShopCache").unwrap_or_default().into_iter().find_map(|(k, v)| v["platform"].as_str().filter(|_| k.starts_with(&prefix)).map(str::to_string));
            if let Some(platform) = cached {
                game["platform"] = Value::from(platform);
                let _ = store.put("games", &key, &game);
            }
        }
        if let Err(error) = crate::classics_launch::open(app, shop, object_id, None, false).await {
            eprintln!("[deep-link] failed to launch {shop}:{object_id}: {error}");
            focus_main(app);
        }
        return;
    }
    let Some(exe) = game["executablePath"].as_str() else {
        eprintln!("[deep-link] cannot run {shop}:{object_id}: not in the library or no executable");
        return;
    };
    if let Err(error) = crate::launch::open_game(app, shop, object_id, exe, game["launchOptions"].as_str()).await {
        eprintln!("[deep-link] failed to launch {shop}:{object_id}: {error}");
        focus_main(app);
    }
}

/// removeSymbolsFromName, falling back to the objectId.
pub fn shortcut_name(title: &str, object_id: &str) -> String {
    let name: String = title.chars().filter(|c| c.is_ascii_alphanumeric() || *c == ' ').collect();
    let name = name.trim();
    if name.is_empty() { object_id.to_string() } else { name.to_string() }
}

// Used by the Linux writer; kept on every OS so its test runs on the Windows CI too.
#[cfg_attr(windows, allow(dead_code))]
/// create-desktop-shortcuts' Linux file. `%` is the desktop-entry field-code marker, so it is doubled.
pub fn desktop_entry(exe: &str, link: &str, name: &str, icon: Option<&Path>) -> String {
    let mut lines = vec![
        "#!/usr/bin/env xdg-open".to_string(),
        "[Desktop Entry]".into(),
        "Version=1.0".into(),
        "Type=Application".into(),
        "Terminal=false".into(),
        format!("Exec=\"{exe}\" \"{}\"", link.replace('"', "\\\"")).replace('%', "%%"),
        format!("Name={name}"),
    ];
    if let Some(icon) = icon {
        lines.push(format!("Icon={}", icon.display()));
    }
    lines.join("\n")
}

/// downloadIcon: the first icon source that downloads and converts, cached by URL.
async fn shortcut_icon(dir: &Path, urls: &[Option<String>]) -> Option<PathBuf> {
    let valid: Vec<&String> = urls.iter().flatten().filter(|u| u.starts_with("http://") || u.starts_with("https://") || u.starts_with("local:")).collect();
    let first = valid.first()?;
    let hash: String = base64::engine::general_purpose::STANDARD.encode(first).chars().filter(char::is_ascii_alphanumeric).take(16).collect();
    let path = dir.join(format!("icon-{hash}.ico"));
    if path.is_file() {
        return Some(path);
    }
    for url in valid {
        let Some(source) = fetch_asset(url).await else { continue };
        let Ok(ico) = convert(&source, ImageFormat::Ico) else { continue };
        if fs::create_dir_all(dir).and_then(|_| fs::write(&path, ico)).is_ok() {
            return Some(path);
        }
    }
    None
}

#[cfg(windows)]
fn write_shortcut(dir: &Path, name: &str, exe: &str, link: &str, icon: Option<&Path>) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let lnk = dir.join(format!("{name}.lnk"));
    for old in [lnk.clone(), dir.join(format!("{name}.url"))] {
        let _ = fs::remove_file(old);
    }
    // Values travel as environment variables so no title or path is ever parsed as script.
    let script = "$s=(New-Object -ComObject WScript.Shell).CreateShortcut($env:CHIMAERA_LNK);$s.TargetPath=$env:CHIMAERA_TARGET;\
                  $s.Arguments=$env:CHIMAERA_ARGS;$s.WorkingDirectory=$env:CHIMAERA_DIR;$s.IconLocation=$env:CHIMAERA_ICON;$s.Save()";
    let exe_dir = Path::new(exe).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("CHIMAERA_LNK", &lnk)
        .env("CHIMAERA_TARGET", exe)
        .env("CHIMAERA_ARGS", link)
        .env("CHIMAERA_DIR", exe_dir)
        .env("CHIMAERA_ICON", icon.map(|i| i.to_string_lossy().into_owned()).unwrap_or_else(|| exe.to_string()))
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() && lnk.is_file() {
        Ok(())
    } else {
        Err(format!("Failed to create shortcut in {}: {}", dir.display(), String::from_utf8_lossy(&output.stderr).trim()))
    }
}

#[cfg(not(windows))]
fn write_shortcut(dir: &Path, name: &str, exe: &str, link: &str, icon: Option<&Path>) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let file = dir.join(format!("{name}.desktop"));
    fs::write(&file, desktop_entry(exe, link, name, icon)).map_err(|e| e.to_string())?;
    fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())
}

/// The program a shortcut runs. An AppImage runs from a fresh /tmp/.mount_* folder each time,
/// so the shortcut must name the .AppImage file itself (its APPIMAGE variable).
pub fn launcher_exe(appimage: Option<String>) -> std::io::Result<String> {
    match appimage.filter(|p| !p.is_empty()) {
        Some(path) => Ok(path),
        None => Ok(std::env::current_exe()?.to_string_lossy().into_owned()),
    }
}

/// createGameShortcut: a desktop or Start Menu shortcut that runs the game through Chimaera.
pub async fn create_game_shortcut(app: &AppHandle, shop: &str, object_id: &str, location: &str) -> Result<bool, String> {
    let store = app.state::<Store>();
    let game = store.get("games", &game_key(shop, object_id))?;
    if game.is_null() {
        return Err("Could not find this game in your library.".into());
    }
    if shop == "launchbox" {
        return Err("Classic games cannot get a shortcut yet.".into());
    }
    if location == "start_menu" && !cfg!(windows) {
        return Err("Start Menu shortcuts are only available on Windows.".into());
    }
    if cfg!(target_os = "macos") {
        return Err("Shortcuts are not available on macOS yet.".into());
    }
    let paths = app.path();
    let output = if location == "start_menu" {
        paths.data_dir().map(|d| d.join("Microsoft").join("Windows").join("Start Menu").join("Programs"))
    } else {
        paths.desktop_dir()
    }
    .map_err(|_| "Could not resolve the shortcut output folder.".to_string())?;
    fs::create_dir_all(&output).map_err(|e| e.to_string())?;

    let assets = if shop == "custom" {
        Value::Null
    } else {
        let version = app.package_info().version.to_string();
        crate::library::get_game_assets(&store, &version, object_id, shop, false).await.unwrap_or(Value::Null)
    };
    let s = |v: &Value| v.as_str().map(String::from);
    let urls = [s(&game["customIconUrl"]), s(&assets["iconUrl"]), s(&game["iconUrl"]), s(&assets["coverImageUrl"])];
    let icon_dir = paths.chimaera_data_dir().map_err(|e| e.to_string())?.join("Assets").join(format!("{shop}-{object_id}"));
    let icon = shortcut_icon(&icon_dir, &urls).await;

    let exe = launcher_exe(std::env::var("APPIMAGE").ok()).map_err(|e| e.to_string())?;
    let name = shortcut_name(game["title"].as_str().unwrap_or_default(), object_id);
    let link = run_link(shop, object_id);
    tauri::async_runtime::spawn_blocking(move || write_shortcut(&output, &name, &exe, &link, icon.as_deref())).await.map_err(|e| e.to_string())??;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appimage_shortcuts_name_the_appimage() {
        assert_eq!(launcher_exe(Some("/home/u/Applications/chimaera.appimage".into())).unwrap(), "/home/u/Applications/chimaera.appimage");
        assert_eq!(launcher_exe(Some(String::new())).unwrap(), std::env::current_exe().unwrap().to_string_lossy());
    }

    #[test]
    fn run_links_round_trip_and_accept_the_legacy_scheme() {
        let link = run_link("steam", "a b&c");
        assert_eq!(link, "chimaera://run?shop=steam&objectId=a+b%26c");
        assert_eq!(run_target(&link), Some(("steam".into(), "a b&c".into())));
        assert_eq!(run_target("hydralauncher://run?shop=gog&objectId=1"), Some(("gog".into(), "1".into())));
        assert_eq!(run_target("chimaera://profile?userId=1"), None);
        assert_eq!(run_target("chimaera://run?shop=steam"), None);
        let args = vec!["chimaera.exe".to_string(), "--hidden".into(), link.clone()];
        assert_eq!(deep_link_arg(&args), Some(link.as_str()));
    }

    #[test]
    fn names_and_desktop_entries() {
        assert_eq!(shortcut_name("Half-Life: 2 ", "x"), "HalfLife 2");
        assert_eq!(shortcut_name("???", "x"), "x");
        let entry = desktop_entry("/opt/chimaera", "chimaera://run?shop=steam&objectId=a%26b", "Game", None);
        assert!(entry.contains("Exec=\"/opt/chimaera\" \"chimaera://run?shop=steam&objectId=a%%26b\""));
        assert!(entry.contains("Name=Game") && !entry.contains("Icon="));
    }

    #[cfg(windows)]
    #[test]
    fn writes_a_windows_shortcut_with_hostile_text_intact() {
        let dir = std::env::temp_dir().join(format!("chimaera-lnk-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let exe = std::env::current_exe().unwrap().to_string_lossy().into_owned();
        let link = "chimaera://run?shop=steam&objectId=1'$(x)\"";
        write_shortcut(&dir, "Game", &exe, link, None).unwrap();
        let parsed = lnk::ShellLink::open(dir.join("Game.lnk"), encoding_rs::WINDOWS_1252).unwrap();
        assert_eq!(parsed.string_data().command_line_arguments().as_deref(), Some(link));
        fs::remove_dir_all(&dir).unwrap();
    }
}
