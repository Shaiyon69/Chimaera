//! Self-update through tauri-plugin-updater: port of `UpdateManager` and the autoupdater
//! events. The plugin refuses any package the pubkey in tauri.conf.json did not sign.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::store::Store;

/// The downloaded package, kept until the user restarts to install it.
static PENDING: Mutex<Option<(Update, Vec<u8>)>> = Mutex::new(None);
static CHECKING: AtomicBool = AtomicBool::new(false);

/// Electron: auto-install on Windows (the NSIS build is never portable), opt-in on Linux.
pub fn auto_install_enabled(windows: bool, preferences: &Value) -> bool {
    windows || preferences["enableAutoInstall"].as_bool() == Some(true)
}

/// checkForUpdates: answers whether updates install automatically, then checks in the background.
pub fn check(app: &AppHandle) -> Value {
    let preferences = app.state::<Store>().get("", "userPreferences").unwrap_or(Value::Null);
    let auto = auto_install_enabled(cfg!(windows), &preferences);
    // Like Electron's app.isPackaged: a dev build never updates itself.
    if !cfg!(debug_assertions) && !CHECKING.swap(true, Ordering::SeqCst) {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = run(&app, auto).await {
                eprintln!("[updater] update check failed: {error}");
            }
            CHECKING.store(false, Ordering::SeqCst);
        });
    }
    json!(auto)
}

async fn run(app: &AppHandle, auto: bool) -> Result<(), String> {
    if PENDING.lock().unwrap().is_some() {
        emit_ready(app);
        return Ok(());
    }
    let Some(update) = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())? else {
        return Ok(());
    };
    let _ = app.emit("autoUpdaterEvent", [json!({ "type": "update-available", "info": { "version": update.version } })]);
    if !auto {
        return Ok(());
    }
    let bytes = update.download(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
    let version = update.version.clone();
    *PENDING.lock().unwrap() = Some((update, bytes));
    emit_ready(app);
    notify(app, &version);
    Ok(())
}

fn emit_ready(app: &AppHandle) {
    let _ = app.emit("autoUpdaterEvent", [json!({ "type": "update-downloaded" })]);
}

/// publishNotificationUpdateReadyToInstall: a toast and the in-app entry, once per download.
// shortcut: the toast is not clickable (the notification plugin has no desktop click action); the in-app banner restarts instead.
fn notify(app: &AppHandle, version: &str) {
    let store = app.state::<Store>();
    let title = crate::i18n::t(&store, "notifications", "new_update_available", &[("version", version)]);
    let body = crate::i18n::t(&store, "notifications", "restart_to_install_update", &[]);
    crate::notifications::toast(app, &title, &body);
    if let Err(error) = crate::notifications::create(app, "UPDATE_AVAILABLE", &title, &body, None, None) {
        eprintln!("[updater] could not store the update notification: {error}");
    }
}

/// restartAndInstallUpdate: no-op without a downloaded update, like Electron in dev.
/// Windows: the plugin starts the NSIS installer and exits; it relaunches the app.
pub fn restart_and_install(app: &AppHandle) -> Result<Value, String> {
    let Some((update, bytes)) = PENDING.lock().unwrap().take() else {
        return Ok(Value::Null);
    };
    update.install(bytes).map_err(|e| e.to_string())?;
    app.restart()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_install_follows_platform_and_preference() {
        assert!(auto_install_enabled(true, &Value::Null));
        assert!(!auto_install_enabled(false, &Value::Null));
        assert!(!auto_install_enabled(false, &json!({ "enableAutoInstall": false })));
        assert!(auto_install_enabled(false, &json!({ "enableAutoInstall": true })));
    }
}
