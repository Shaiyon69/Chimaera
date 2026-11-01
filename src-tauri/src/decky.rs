//! Hydra's Decky Loader plugin for Steam Deck (services/decky-plugin.ts,
//! events/misc/get-hydra-decky-plugin-info.ts and install-hydra-decky-plugin.ts).
//! The plugin lives in ~/homebrew/plugins/Hydra; Hydra's API names the current release.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

fn plugins_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app.path().home_dir().map_err(|e| e.to_string())?.join("homebrew").join("plugins"))
}

fn installed_version(plugin: &Path) -> Option<String> {
    let text = fs::read_to_string(plugin.join("package.json")).ok()?;
    serde_json::from_str::<Value>(&text).ok()?["version"].as_str().map(str::to_string)
}

/// GET /decky/release: { version, downloadUrl }.
async fn release(app: &AppHandle) -> Result<(String, String), String> {
    let (client, url) = crate::hydra_api::public_request(&app.package_info().version.to_string(), "/decky/release")?;
    let body: Value = client.get(url).send().await.and_then(|r| r.error_for_status()).map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    match (body["version"].as_str(), body["downloadUrl"].as_str()) {
        (Some(v), Some(u)) => Ok((v.to_string(), u.to_string())),
        _ => Err("Unexpected Decky release response".into()),
    }
}

fn info_of(plugin: &Path, expected: Option<String>) -> Value {
    let version = installed_version(plugin);
    let outdated = match (&version, &expected) {
        (None, _) => true,
        (Some(v), Some(e)) => v != e,
        (Some(_), None) => false,
    };
    json!({ "installed": version.is_some(), "version": version, "path": plugin, "outdated": outdated, "expectedVersion": expected })
}

pub async fn info(app: &AppHandle) -> Result<Value, String> {
    let plugin = plugins_dir(app)?.join("Hydra");
    let expected = release(app).await.map_err(|e| eprintln!("[decky] release info failed: {e}")).ok().map(|(v, _)| v);
    Ok(info_of(&plugin, expected))
}

/// Downloads the release zip and replaces the plugin folder with its `Hydra` folder.
// shortcut: no sudo prompt; a root-owned ~/homebrew/plugins fails with the OS error. Add pkexec if Deck users hit it.
async fn update(app: &AppHandle, download_url: &str) -> Result<(), String> {
    let bytes = reqwest::get(download_url).await.and_then(|r| r.error_for_status()).map_err(|e| e.to_string())?.bytes().await.map_err(|e| e.to_string())?;
    let plugins = plugins_dir(app)?;
    let temp = std::env::temp_dir().join("hydra-decky-plugin");
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let _ = fs::remove_dir_all(&temp);
        // extract() refuses entries whose paths escape `temp`.
        let result = zip::ZipArchive::new(std::io::Cursor::new(bytes)).and_then(|mut z| z.extract(&temp)).map_err(|e| e.to_string()).and_then(|_| {
            let source = temp.join("Hydra");
            if !source.is_dir() {
                return Err("Failed to extract plugin".into());
            }
            let target = plugins.join("Hydra");
            fs::create_dir_all(&plugins).map_err(|e| e.to_string())?;
            if target.exists() {
                fs::remove_dir_all(&target).map_err(|e| e.to_string())?;
            }
            crate::electron_import::copy_dir(&source, &target).map_err(|e| e.to_string())
        });
        let _ = fs::remove_dir_all(&temp);
        result
    })
    .await
    .map_err(|e| e.to_string())?
}

/// installHydraDeckyPlugin: installs or updates, then reports what is on disk.
pub async fn install(app: &AppHandle) -> Value {
    let plugin = match plugins_dir(app) {
        Ok(dir) => dir.join("Hydra"),
        Err(error) => return json!({ "success": false, "path": "", "currentVersion": null, "expectedVersion": "unknown", "error": error }),
    };
    let (expected, url) = match release(app).await {
        Ok(r) => r,
        Err(error) => return json!({ "success": false, "path": plugin, "currentVersion": null, "expectedVersion": "unknown", "error": error }),
    };
    let mut error = None;
    if installed_version(&plugin).as_deref() != Some(expected.as_str()) {
        if let Err(e) = update(app, &url).await {
            eprintln!("[decky] install failed: {e}");
            error = Some(e);
        }
    }
    let current = installed_version(&plugin);
    let success = error.is_none() && current.is_some();
    let mut result = json!({ "success": success, "path": plugin, "currentVersion": current, "expectedVersion": expected });
    if !success {
        result["error"] = json!(error.unwrap_or_else(|| "Plugin installation failed".into()));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_missing_and_outdated_plugins() {
        let dir = std::env::temp_dir().join(format!("chimaera-decky-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(info_of(&dir, Some("1.0.0".into()))["installed"], json!(false));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("package.json"), r#"{"version":"0.9.0"}"#).unwrap();
        let info = info_of(&dir, Some("1.0.0".into()));
        assert_eq!((info["installed"].clone(), info["outdated"].clone(), info["version"].clone()), (json!(true), json!(true), json!("0.9.0")));
        assert_eq!(info_of(&dir, None)["outdated"], json!(false));
        let _ = fs::remove_dir_all(&dir);
    }
}
