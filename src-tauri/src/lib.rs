//! Tauri shell for Chimaera. The React renderer and the Electron preload run
//! unchanged (src/tauri/electron-shim.ts); every preload IPC call arrives at the
//! single `ipc` command and is routed by channel name in `ipc.rs`.

mod electron_import;
mod hydra_api;
mod ipc;
mod library;
mod store;

use serde_json::{json, Value};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

/// Node's `process.platform` name for this OS, which the renderer branches on.
fn node_platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        std::env::consts::OS
    }
}

/// What the preload reads from `process` at load time. Only these variables,
/// never the whole environment: the webview loads remote content.
fn process_script() -> String {
    let env: serde_json::Map<String, Value> = ["PORTABLE_EXECUTABLE_FILE", "XDG_SESSION_TYPE", "WAYLAND_DISPLAY"]
        .iter()
        .filter_map(|key| std::env::var(key).ok().map(|value| (key.to_string(), json!(value))))
        .collect();
    format!(
        "window.__CHIMAERA_PROCESS__ = {};",
        json!({ "platform": node_platform(), "env": env })
    )
}

#[tauri::command]
async fn ipc(app: tauri::AppHandle, window: tauri::WebviewWindow, channel: String, args: Vec<Value>) -> Result<Value, String> {
    ipc::dispatch(&app, &window, &channel, &args).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![ipc])
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let store = store::Store::open(&data_dir.join("chimaera.db"))?;
            let user_data = electron_import::electron_user_data(&app.path().config_dir()?);
            let scratch = app.path().app_cache_dir()?.join("electron-import");
            match electron_import::run_once(&store, &user_data, &scratch) {
                Ok(Some(result)) => eprintln!("[import] Electron data: {result}"),
                Ok(None) => {}
                // Not fatal: the app starts empty and the import retries next launch.
                Err(error) => eprintln!("[import] Electron data import failed: {error}"),
            }
            app.manage(store);
            app.manage(ipc::AppSession(uuid::Uuid::new_v4().to_string()));
            // Same size and chrome as WindowManager.createMainWindow: the renderer
            // draws its own title bar and window buttons.
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("tauri.html".into()))
                .title("Chimaera")
                .inner_size(1200.0, 860.0)
                .min_inner_size(1024.0, 600.0)
                .decorations(false)
                .initialization_script(&process_script())
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Chimaera");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_only_the_platform_and_allowed_env() {
        let script = process_script();
        assert!(script.starts_with("window.__CHIMAERA_PROCESS__ = {"));
        assert!(script.contains(&format!("\"platform\":\"{}\"", node_platform())));
        assert!(!script.contains("PATH"));
    }
}
