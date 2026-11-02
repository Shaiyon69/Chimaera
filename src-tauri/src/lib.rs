//! Tauri shell for Chimaera. The React renderer and the Electron preload run
//! unchanged (src/tauri/electron-shim.ts); every preload IPC call arrives at the
//! single `ipc` command and is routed by channel name in `ipc.rs`.

mod achievement_files;
mod achievement_popup;
mod achievements;
mod artwork;
mod big_picture;
mod bundled_games;
mod classics_import;
mod classics_launch;
mod common_redist;
mod debrid;
mod decky;
mod download_sources;
mod downloads;
mod dxvk;
mod electron_import;
mod emulator_install;
mod emulators;
mod extraction;
mod file_saves;
mod file_system;
mod game_installer;
mod games;
mod genres;
mod google_drive;
mod graphics_scan;
mod hosters;
mod http_download;
mod hydra_api;
mod hydra_backup;
mod i18n;
mod image_cache;
mod installer;
mod ipc;
mod launch;
mod launcher_import;
mod library;
mod memcard_saves;
mod memory_card;
mod messapp_keys;
mod profile;
mod save_backups;
mod scan;
mod screenshots;
mod local_files;
mod misc;
mod notifications;
mod optiscaler;
mod processed_image;
mod retro_achievements;
mod retroarch;
mod rom_scan;
mod rpcs3_root;
mod shop_details;
mod shortcuts;
mod steam;
mod steam_shortcuts;
mod store;
mod themes;
mod torrent;
mod torrenting;
mod trackers;
mod transfer;
mod updater;
mod watcher;
mod webview_memory;

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
pub(crate) fn process_script() -> String {
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
        // First plugin, so a second launch exits before anything else starts; its
        // arguments (a chimaera:// link from a shortcut) go to the running app.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| shortcuts::handle_second_instance(app, &args)))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![ipc])
        .register_asynchronous_uri_scheme_protocol("local", |_ctx, request, responder| {
            let path = request.uri().path().to_string();
            // Off the webview thread: a large hero image would stall the UI.
            std::thread::spawn(move || responder.respond(local_files::handle(&path)));
        })
        .register_asynchronous_uri_scheme_protocol("cached-image", |ctx, request, responder| {
            let dir = ctx.app_handle().path().app_data_dir().map(|dir| image_cache::cache_dir(&dir));
            let path = request.uri().path().to_string();
            tauri::async_runtime::spawn(async move {
                let response = match dir {
                    Ok(dir) => image_cache::handle(&dir, &path).await,
                    Err(_) => image_cache::status(tauri::http::StatusCode::INTERNAL_SERVER_ERROR),
                };
                responder.respond(response);
            });
        })
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let image_dir = image_cache::cache_dir(&data_dir);
            std::thread::spawn(move || image_cache::prune(&image_dir));
            let store = store::Store::open(&data_dir.join("chimaera.db"))?;
            let user_data = electron_import::electron_user_data(&app.path().config_dir()?);
            http_download::adopt_electron_ranges(&user_data.join("DownloadRanges"), &data_dir.join("DownloadRanges"));
            let scratch = app.path().app_cache_dir()?.join("electron-import");
            match electron_import::run_once(&store, &user_data, &scratch) {
                Ok(Some(result)) => eprintln!("[import] Electron data: {result}"),
                Ok(None) => {}
                // Not fatal: the app starts empty and the import retries next launch.
                Err(error) => eprintln!("[import] Electron data import failed: {error}"),
            }
            // After the import, so imported launcher games get their Steam record on the first run too.
            match games::backfill_steam_records(&store) {
                Ok(0) => {}
                Ok(count) => eprintln!("[library] added {count} steam records for launcher games"),
                Err(error) => eprintln!("[library] steam backfill failed: {error}"),
            }
            app.manage(store);
            app.manage(ipc::AppSession(uuid::Uuid::new_v4().to_string()));
            app.manage(launch::LauncherStatuses::default());
            app.manage(watcher::Sessions::default());
            watcher::spawn(app.handle().clone());
            app.manage(achievements::AchievementWatcher::default());
            app.manage(achievement_popup::Popup::default());
            achievements::spawn(app.handle().clone());
            app.manage(downloads::Downloads::default());
            downloads::spawn(app.handle().clone());
            app.manage(steam::SteamSync::default());
            app.manage(classics_import::ClassicsImports::default());
            app.manage(classics_launch::EmulatorPids::default());
            app.manage(retroarch::RetroArchImports::default());
            // Same size and chrome as WindowManager.createMainWindow: the renderer
            // draws its own title bar and window buttons.
            let args: Vec<String> = std::env::args().collect();
            let launch_big_picture = big_picture::should_launch_in(&app.state::<store::Store>(), &args);
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("tauri.html".into()))
                .title("Chimaera")
                .visible(!launch_big_picture)
                .inner_size(1200.0, 860.0)
                .min_inner_size(1024.0, 600.0)
                .decorations(false)
                .initialization_script(&process_script())
                .build()?
                .on_window_event({
                    let app = app.handle().clone();
                    // Minimize, restore and hide all land here as Resized/Focused.
                    move |event| {
                        if matches!(event, tauri::WindowEvent::Resized(_) | tauri::WindowEvent::Focused(_)) {
                            webview_memory::refresh(&app);
                        }
                    }
                });
            if launch_big_picture {
                big_picture::open(app.handle())?;
            }
            // Started with Windows and asked to stay out of the way.
            // shortcut: minimized, not hidden to the tray; the tray icon is not ported yet.
            if args.iter().any(|a| a == misc::HIDDEN_ARG) {
                if let Some(main) = app.get_webview_window("main") {
                    let _ = main.minimize();
                }
            }
            if cfg!(windows) {
                let handle = app.handle().clone();
                let app = handle.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = common_redist::download(&app).await {
                        eprintln!("[common-redist] download failed: {error}");
                    }
                });
                let app = handle;
                tauri::async_runtime::spawn(async move { optiscaler::auto_update(&app).await });
            }
            {
                // After the window is up, so the renderer hears the sync progress.
                let app = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    steam::start_on_launch(&app);
                });
            }
            if let Some((shop, object_id)) = shortcuts::deep_link_arg(&args).and_then(shortcuts::run_target) {
                let app = app.handle().clone();
                tauri::async_runtime::spawn(async move { shortcuts::run_game(&app, &shop, &object_id).await });
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while running Chimaera")
        .run(|_, event| {
            // Close the libtorrent session cleanly so resume data and the port are released.
            if let tauri::RunEvent::Exit = event {
                tauri::async_runtime::block_on(torrent::shutdown());
            }
        });
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
