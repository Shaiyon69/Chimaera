//! Preload channel name -> Rust handler. One match arm per ported
//! `ipcRenderer.invoke` channel; `scripts/tauri-parity.mjs` reads these arms
//! to list what is still missing, so keep each arm as `"channelName" =>`.

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use tauri_plugin_opener::OpenerExt;

use crate::store::{assert_renderer_access, Store};
use crate::{hydra_api, library};

/// Changes on every launch; the renderer clears session-scoped localStorage when it does.
pub struct AppSession(pub String);

fn arg_str(args: &[Value], index: usize) -> Option<&str> {
    args.get(index).and_then(Value::as_str)
}

pub async fn dispatch(app: &AppHandle, window: &WebviewWindow, channel: &str, args: &[Value]) -> Result<Value, String> {
    match channel {
        "ping" => Ok(json!("pong")),
        "getVersion" => Ok(json!(app.package_info().version.to_string())),
        "getDefaultDownloadsPath" => {
            let dir = app.path().download_dir().map_err(|e| e.to_string())?;
            Ok(json!(dir.to_string_lossy()))
        }
        "openExternal" => {
            let src = arg_str(args, 0).unwrap_or_default();
            if !is_safe_external_url(src) {
                return Ok(Value::Null);
            }
            app.opener().open_url(src, None::<&str>).map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        "isStaging" => Ok(json!(false)),
        "getAppSessionId" => Ok(json!(app.state::<AppSession>().0)),
        "leveldbGet" => {
            let (key, sublevel) = (arg_str(args, 0).unwrap_or_default(), arg_str(args, 1));
            assert_renderer_access(false, sublevel, Some(key))?;
            app.state::<Store>().get(sublevel.unwrap_or_default(), key)
        }
        "leveldbPut" => {
            let (key, sublevel) = (arg_str(args, 0).unwrap_or_default(), arg_str(args, 2));
            assert_renderer_access(true, sublevel, Some(key))?;
            let value = args.get(1).cloned().unwrap_or(Value::Null);
            app.state::<Store>().put(sublevel.unwrap_or_default(), key, &value).map(|_| Value::Null)
        }
        "leveldbDel" => {
            let (key, sublevel) = (arg_str(args, 0).unwrap_or_default(), arg_str(args, 1));
            assert_renderer_access(true, sublevel, Some(key))?;
            app.state::<Store>().del(sublevel.unwrap_or_default(), key).map(|_| Value::Null)
        }
        "leveldbClear" => {
            let sublevel = arg_str(args, 0);
            assert_renderer_access(true, sublevel, None)?;
            app.state::<Store>().clear(sublevel.unwrap_or_default()).map(|_| Value::Null)
        }
        "leveldbValues" => {
            let sublevel = arg_str(args, 0);
            assert_renderer_access(false, sublevel, None)?;
            let entries = app.state::<Store>().entries(sublevel.unwrap_or_default())?;
            Ok(Value::Array(entries.into_iter().map(|(_, v)| v).collect()))
        }
        "leveldbIterator" => {
            let sublevel = arg_str(args, 0);
            assert_renderer_access(false, sublevel, None)?;
            let entries = app.state::<Store>().entries(sublevel.unwrap_or_default())?;
            Ok(Value::Array(entries.into_iter().map(|(k, v)| json!([k, v])).collect()))
        }
        "hydraApiCall" => {
            let version = app.package_info().version.to_string();
            hydra_api::call(&app.state::<Store>(), &version, args.first().unwrap_or(&Value::Null)).await
        }
        // getUserData: Chimaera's profile is local, so this is LocalApi's `/profile/me`.
        "getMe" => {
            let version = app.package_info().version.to_string();
            hydra_api::call(&app.state::<Store>(), &version, &json!({ "method": "get", "url": "/profile/me" })).await
        }
        // Electron tells the download orchestrator and souvenir worker; neither exists here yet (F4, F6).
        "updateNetworkStatus" => Ok(Value::Null),
        "getLibrary" => {
            let include_concealed = args.first().and_then(Value::as_bool).unwrap_or(false);
            library::get_library(&app.state::<Store>(), include_concealed)
        }
        // HydraApi.isLoggedIn() is always false in Chimaera, so Electron returns [] too.
        "getHiddenLibrary" => Ok(json!([])),
        "getGameAssets" => {
            let version = app.package_info().version.to_string();
            let (object_id, shop) = (arg_str(args, 0).unwrap_or_default(), arg_str(args, 1).unwrap_or_default());
            let force_fresh = args.get(2).and_then(|o| o["forceFresh"].as_bool()).unwrap_or(false);
            library::get_game_assets(&app.state::<Store>(), &version, object_id, shop, force_fresh).await
        }
        // Electron merges /profile/games from Hydra, which local mode blocks, so it fails there too.
        "refreshLibraryAssets" => Err("library/refresh-failed".into()),
        "getDownloadLayoutState" => library::download_layout_state(&app.state::<Store>()),
        // No process watcher yet (F2): nothing is tracked as running.
        "getGamesRunning" => Ok(json!([])),
        // null = "no still poster", so the renderer shows the original cover.
        // shortcut: animated covers play instead of a still first frame until the local: protocol (F1) can serve built posters.
        "getCoverPoster" => Ok(Value::Null),
        "getClassicsImportStatus" => Ok(json!(false)),
        "getUserPreferences" => app.state::<Store>().get("", "userPreferences"),
        "updateUserPreferences" => {
            let store = app.state::<Store>();
            let patch = args.first().cloned().unwrap_or(Value::Null);
            let updated = library::merge_preferences(store.get("", "userPreferences")?, &patch);
            if let Some(language) = patch["language"].as_str() {
                store.put("", "language", &json!(language))?;
            }
            store.put("", "userPreferences", &updated)?;
            // shortcut: Wine prefix pinning, souvenir capture and download speed limits react to these
            // preferences in Electron; each moves here with its milestone (F4, F6, F8).
            app.emit("on-user-preferences-updated", [&updated]).map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        "getLocalNotificationsCount" => {
            let entries = app.state::<Store>().entries("localNotifications")?;
            Ok(json!(entries.iter().filter(|(_, n)| !n["isRead"].as_bool().unwrap_or(false)).count()))
        }
        // No remote library sync, classics import or RetroArch import exists in this
        // build yet, so these report their real idle state.
        "getRemoteLibrarySyncState" => Ok(json!(false)),
        "getActiveClassicsImport" => Ok(Value::Null),
        "getActiveRetroArchImport" => Ok(Value::Null),
        // Returns whether auto-install is on; the Tauri updater arrives with milestone F1.
        "checkForUpdates" => Ok(json!(false)),
        "getRandomGame" => library::random_steam250_game().await,
        "minimizeMainWindow" => window.minimize().map(|_| Value::Null).map_err(|e| e.to_string()),
        "toggleMaximizeMainWindow" => {
            let maximized = window.is_maximized().map_err(|e| e.to_string())?;
            let result = if maximized { window.unmaximize() } else { window.maximize() };
            result.map(|_| Value::Null).map_err(|e| e.to_string())
        }
        // shortcut: closes outright; the Electron build can hide to the tray instead, which arrives with the tray in milestone 1.
        "closeMainWindow" => window.close().map(|_| Value::Null).map_err(|e| e.to_string()),
        "isMainWindowMaximized" => window.is_maximized().map(|m| json!(m)).map_err(|e| e.to_string()),
        _ => {
            // Dev aid: the terminal running `yarn tauri:dev` lists what the UI still needs.
            #[cfg(debug_assertions)]
            eprintln!("[ipc] not ported: {channel}");
            Err(format!("not ported to Tauri yet: {channel}"))
        }
    }
}

/// Same rule as `isSafeExternalUrl` (src/main/helpers/ipc-guards-core.ts): only
/// schemes that open a browser or mail client, never file: or custom handlers.
pub fn is_safe_external_url(raw: &str) -> bool {
    url::Url::parse(raw).is_ok_and(|u| matches!(u.scheme(), "https" | "http" | "mailto"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_urls_match_the_electron_guard() {
        assert!(is_safe_external_url("https://example.com"));
        assert!(is_safe_external_url("mailto:a@b.c"));
        assert!(!is_safe_external_url("file:///C:/Windows/System32/calc.exe"));
        assert!(!is_safe_external_url("javascript:alert(1)"));
        assert!(!is_safe_external_url("steam://run/620"));
        assert!(!is_safe_external_url("not a url"));
    }
}
