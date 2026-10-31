//! Preload channel name -> Rust handler. One match arm per ported
//! `ipcRenderer.invoke` channel; `scripts/tauri-parity.mjs` reads these arms
//! to list what is still missing, so keep each arm as `"channelName" =>`.

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_opener::OpenerExt;

use crate::launch::LauncherStatuses;
use crate::store::{assert_renderer_access, Store};
use crate::watcher::Sessions;
use crate::{achievement_popup, achievements, artwork, bundled_games, file_system, games, hydra_api, installer, launch, library, shop_details, shortcuts, steam_shortcuts};

/// Changes on every launch; the renderer clears session-scoped localStorage when it does.
pub struct AppSession(pub String);

fn arg_str(args: &[Value], index: usize) -> Option<&str> {
    args.get(index).and_then(Value::as_str)
}

/// Most library channels start with (shop, objectId).
fn shop(args: &[Value]) -> &str {
    arg_str(args, 0).unwrap_or_default()
}

fn object_id(args: &[Value]) -> &str {
    arg_str(args, 1).unwrap_or_default()
}

/// The (shop, objectId) download record and the downloads folder it defaults to.
fn download_of(app: &AppHandle, args: &[Value]) -> Result<(Value, std::path::PathBuf), String> {
    let store = app.state::<Store>();
    let download = store.get("downloads", &games::game_key(shop(args), object_id(args)))?;
    let default = app.path().download_dir().map_err(|e| e.to_string())?;
    Ok((download, installer::downloads_path(&store, &default)))
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
        "getGamesRunning" => Ok(app.state::<Sessions>().running()),
        "getGameByObjectId" => {
            let store = app.state::<Store>();
            match library::linked_view(&store, &games::game_key(shop(args), object_id(args)))? {
                Some(grouped) => Ok(grouped),
                None => games::get_by_object_id(&store, shop(args), object_id(args)),
            }
        }
        "addGameToLibrary" => {
            let (title, platform) = (arg_str(args, 2).unwrap_or_default(), arg_str(args, 3));
            games::add_to_library(&app.state::<Store>(), shop(args), object_id(args), title, platform).map(|_| Value::Null)
        }
        "removeGameFromLibrary" => games::remove_from_library(&app.state::<Store>(), shop(args), object_id(args)).map(|_| Value::Null),
        "removeGame" => app.state::<Store>().del("downloads", &games::game_key(shop(args), object_id(args))).map(|_| Value::Null),
        "addGameToFavorites" => games::set_favorite(&app.state::<Store>(), shop(args), object_id(args), true).map(|_| Value::Null),
        "removeGameFromFavorites" => games::set_favorite(&app.state::<Store>(), shop(args), object_id(args), false).map(|_| Value::Null),
        "toggleGamePin" => {
            let pin = args.get(2).and_then(Value::as_bool).unwrap_or(false);
            games::set_pin(&app.state::<Store>(), shop(args), object_id(args), pin).map(|_| Value::Null)
        }
        "updateLaunchOptions" => games::set_launch_options(&app.state::<Store>(), shop(args), object_id(args), arg_str(args, 2)).map(|_| Value::Null),
        "setGameVisibility" => {
            let (field, value) = (arg_str(args, 2).unwrap_or_default(), args.get(3).and_then(Value::as_bool).unwrap_or(false));
            games::set_visibility(&app.state::<Store>(), shop(args), object_id(args), field, value)
        }
        "changeGamePlayTime" => {
            let seconds = args.get(2).and_then(Value::as_f64).unwrap_or(0.0);
            games::change_playtime(&app.state::<Store>(), shop(args), object_id(args), seconds).map(|_| Value::Null)
        }
        "resetGamePlayTime" => games::reset_playtime(&app.state::<Store>(), shop(args), object_id(args)).map(|_| Value::Null),
        "updateExecutablePath" => {
            let path = arg_str(args, 2).map(launch::parse_executable_path);
            games::set_executable_path(&app.state::<Store>(), shop(args), object_id(args), path.as_deref()).map(|_| Value::Null)
        }
        "updateTrackingExecutablePaths" => {
            let paths = args.get(2).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(launch::parse_executable_path).collect();
            games::set_tracking_paths(&app.state::<Store>(), shop(args), object_id(args), paths).map(|_| Value::Null)
        }
        "verifyExecutablePathInUse" => games::game_using_executable(&app.state::<Store>(), arg_str(args, 0).unwrap_or_default()),
        "openGameExecutablePath" => {
            let game = app.state::<Store>().get("games", &games::game_key(shop(args), object_id(args)))?;
            if let Some(path) = game["executablePath"].as_str() {
                app.opener().reveal_item_in_dir(path).map_err(|e| e.to_string())?;
            }
            Ok(Value::Null)
        }
        "hasDownloadFolder" => {
            let (download, root) = download_of(app, args)?;
            Ok(json!(installer::folder_path(&download, &root).is_some_and(|f| f.exists())))
        }
        "getExecutablePickerPath" => {
            let (download, root) = download_of(app, args)?;
            let launched = download["installerLaunched"].as_bool().unwrap_or(false);
            let folder = installer::folder_path(&download, &root).filter(|f| !launched && f.is_dir());
            Ok(json!(folder.map(|f| f.to_string_lossy().into_owned())))
        }
        "hasSourceArchive" => {
            let (download, root) = download_of(app, args)?;
            Ok(json!(installer::source_archive(&download, &root).is_some()))
        }
        "hasGameInstaller" => {
            if shop(args) == "launchbox" {
                return Ok(json!(false));
            }
            let (download, root) = download_of(app, args)?;
            tauri::async_runtime::spawn_blocking(move || json!(installer::download_installer(&download, &root).is_some())).await.map_err(|e| e.to_string())
        }
        "openGameInstallerPath" => {
            let (download, _) = download_of(app, args)?;
            // Like Electron, only a download with its own recorded path is revealed.
            if let (Some(root), Some(folder)) = (download["downloadPath"].as_str(), download["folderName"].as_str()) {
                app.opener().reveal_item_in_dir(std::path::Path::new(root).join(folder)).map_err(|e| e.to_string())?;
            }
            Ok(Value::Null)
        }
        "deleteGameFolder" => {
            let (app, key) = (app.clone(), games::game_key(shop(args), object_id(args)));
            let delete_archive = args.get(2).and_then(|o| o["deleteSourceArchive"].as_bool()).unwrap_or(false);
            let default = app.path().download_dir().map_err(|e| e.to_string())?;
            tauri::async_runtime::spawn_blocking(move || installer::delete_game_folder(&app.state::<Store>(), &default, &key, delete_archive))
                .await
                .map_err(|e| e.to_string())?
                .map(|_| Value::Null)
        }
        "deleteArchive" => {
            let default = app.path().download_dir().map_err(|e| e.to_string())?;
            installer::delete_archive(&app.state::<Store>(), &default, arg_str(args, 0).unwrap_or_default()).map(|ok| json!(ok))
        }
        "createGameShortcut" => shortcuts::create_game_shortcut(app, shop(args), object_id(args), arg_str(args, 2).unwrap_or("desktop")).await.map(|ok| json!(ok)),
        "checkSteamShortcut" => {
            let (app, key) = (app.clone(), games::game_key(shop(args), object_id(args)));
            tauri::async_runtime::spawn_blocking(move || steam_shortcuts::check(&app.state::<Store>(), steam_shortcuts::steam_path().as_deref(), &key))
                .await
                .map_err(|e| e.to_string())?
                .map(|found| json!(found))
        }
        "createSteamShortcut" => {
            let (shop, object_id) = (shop(args).to_string(), object_id(args).to_string());
            let open_vr = args.get(2).and_then(|o| o["openVr"].as_bool()).unwrap_or(false);
            let game = app.state::<Store>().get("games", &games::game_key(&shop, &object_id))?;
            let version = app.package_info().version.to_string();
            let assets = library::get_game_assets(&app.state::<Store>(), &version, &object_id, &shop, false).await.unwrap_or(Value::Null);
            let mut sources: [Option<Vec<u8>>; 5] = Default::default();
            for (source, url) in sources.iter_mut().zip(steam_shortcuts::asset_urls(&game, &assets)) {
                if let Some(url) = url {
                    *source = steam_shortcuts::fetch_asset(&url).await;
                }
            }
            let assets_dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("Assets");
            let (app, key) = (app.clone(), games::game_key(&shop, &object_id));
            tauri::async_runtime::spawn_blocking(move || {
                steam_shortcuts::create(&app.state::<Store>(), steam_shortcuts::steam_path().as_deref(), &assets_dir, &key, sources, open_vr)
            })
            .await
            .map_err(|e| e.to_string())?
            .map(|_| Value::Null)
        }
        "deleteSteamShortcut" => {
            let (app, key) = (app.clone(), games::game_key(shop(args), object_id(args)));
            tauri::async_runtime::spawn_blocking(move || steam_shortcuts::delete(&app.state::<Store>(), steam_shortcuts::steam_path().as_deref(), &key))
                .await
                .map_err(|e| e.to_string())?
                .map(|done| json!(done))
        }
        "getAvailableDrives" => tauri::async_runtime::spawn_blocking(file_system::available_drives).await.map_err(|e| e.to_string()),
        "clearNewDownloadOptions" => games::clear_new_download_options(&app.state::<Store>(), shop(args), object_id(args)).map(|_| Value::Null),
        "assignGameToCollection" => {
            let ids = args.get(2).cloned().unwrap_or(json!([]));
            games::assign_to_collection(&app.state::<Store>(), shop(args), object_id(args), ids).map(|_| Value::Null)
        }
        // The watcher always counts playtime here, so a running session needs no extra switch.
        "setGameHydraPlaytimeEnabled" => {
            let enabled = args.get(2).and_then(Value::as_bool).unwrap_or(false);
            games::set_field(&app.state::<Store>(), shop(args), object_id(args), "enableHydraPlaytimeTracking", json!(enabled)).map(|_| Value::Null)
        }
        "setGameDedupeExcluded" => {
            let excluded = args.get(2).and_then(Value::as_bool).unwrap_or(false);
            games::set_field(&app.state::<Store>(), shop(args), object_id(args), "dedupeExcluded", json!(excluded)).map(|_| Value::Null)
        }
        "toggleGameGamemode" => {
            let on = args.get(2).and_then(Value::as_bool).unwrap_or(false);
            games::set_field(&app.state::<Store>(), shop(args), object_id(args), "autoRunGamemode", json!(on)).map(|_| Value::Null)
        }
        "toggleGameMangohud" => {
            let on = args.get(2).and_then(Value::as_bool).unwrap_or(false);
            games::set_field(&app.state::<Store>(), shop(args), object_id(args), "autoRunMangohud", json!(on)).map(|_| Value::Null)
        }
        // shortcut: gamemode, MangoHud, winetricks and Proton are Linux-only and report their Windows answer;
        // the Linux launch path (Wine/umu/Proton) ports them for real.
        "isGamemodeAvailable" => Ok(json!(false)),
        "isMangohudAvailable" => Ok(json!(false)),
        "isWinetricksAvailable" => Ok(json!(false)),
        "getInstalledProtonVersions" => Ok(json!([])),
        "getGameLaunchProtonVersion" => Ok(Value::Null),
        "getGameArtworkSelection" => app.state::<Store>().get("artworkSelection", &games::game_key(shop(args), object_id(args))),
        "setGameArtworkSelection" => {
            let record = artwork::set_artwork_selection(&app.state::<Store>(), args.first().unwrap_or(&Value::Null))?;
            app.emit("on-library-batch-complete", ()).map_err(|e| e.to_string())?;
            Ok(record)
        }
        "addCustomGameToLibrary" => {
            let (title, exe) = (arg_str(args, 0).unwrap_or_default(), arg_str(args, 1).unwrap_or_default());
            artwork::add_custom_game(&app.state::<Store>(), title, exe, args.get(2), args.get(3), args.get(4))
        }
        "copyCustomGameAsset" => {
            let assets = app.path().app_data_dir().map_err(|e| e.to_string())?.join("Assets");
            artwork::copy_custom_game_asset(&assets, arg_str(args, 0).unwrap_or_default(), arg_str(args, 1).unwrap_or_default()).map(|url| json!(url))
        }
        "updateCustomGame" => {
            let game = artwork::update_custom_game(&app.state::<Store>(), args.first().unwrap_or(&Value::Null))?;
            app.emit("on-library-batch-complete", ()).map_err(|e| e.to_string())?;
            Ok(game)
        }
        "updateGameCustomAssets" => {
            let game = artwork::update_game_custom_assets(&app.state::<Store>(), args.first().unwrap_or(&Value::Null))?;
            app.emit("on-library-batch-complete", ()).map_err(|e| e.to_string())?;
            Ok(game)
        }
        "openGameSaveFolder" => {
            let folder = arg_str(args, 2).unwrap_or_default();
            if folder.is_empty() || !std::path::Path::new(folder).exists() {
                return Ok(json!(false));
            }
            Ok(json!(app.opener().open_path(games::save_folder_shell_path(folder), None::<&str>).is_ok()))
        }
        "getPlaySessions" =>games::play_sessions(&app.state::<Store>(), shop(args), object_id(args)),
        "getPlaytimeSummary" => games::playtime_summary(&app.state::<Store>()),
        "openGame" => {
            let (executable_path, launch_options) = (arg_str(args, 2).unwrap_or_default(), arg_str(args, 3));
            launch::open_game(app, shop(args), object_id(args), executable_path, launch_options).await
        }
        "closeGame" => launch::close_game(&app.state::<Store>(), shop(args), object_id(args)),
        "getGameLauncherStatuses" => Ok(app.state::<LauncherStatuses>().get(&games::game_key(shop(args), object_id(args)))),
        "showGameLauncherWindow" => {
            if let Some(launcher) = app.get_webview_window(launch::LAUNCHER_LABEL) {
                launcher.show().map_err(|e| e.to_string())?;
            }
            Ok(Value::Null)
        }
        "closeGameLauncherWindow" => {
            launch::close_launcher_window(app);
            Ok(Value::Null)
        }
        "openMainWindow" => {
            if let Some(main) = app.get_webview_window("main") {
                let _ = main.show();
                let _ = main.unminimize();
                main.set_focus().map_err(|e| e.to_string())?;
            }
            Ok(Value::Null)
        }
        "isMainWindowOpen" => Ok(json!(app.get_webview_window("main").is_some_and(|w| w.is_visible().unwrap_or(false)))),
        "getImageDataUrl" => Ok(launch::image_data_url(arg_str(args, 0).unwrap_or_default()).await),
        // null = "no still poster", so the renderer shows the original cover.
        // shortcut: animated covers play instead of a still first frame until the local: protocol (F1) can serve built posters.
        "getCoverPoster" => Ok(Value::Null),
        "getClassicsImportStatus" => Ok(json!(false)),
        "getGameShopDetails" => {
            let (object_id, shop, language) = (arg_str(args, 0).unwrap_or_default(), arg_str(args, 1).unwrap_or_default(), arg_str(args, 2).unwrap_or("en"));
            shop_details::get_game_shop_details(app, object_id, shop, language).await
        }
        "getGameStats" => {
            let version = app.package_info().version.to_string();
            shop_details::get_game_stats(&app.state::<Store>(), &version, arg_str(args, 0).unwrap_or_default(), arg_str(args, 1).unwrap_or_default()).await
        }
        "getBundledGameSetup" => bundled_games::setup(app, arg_str(args, 0).unwrap_or_default()),
        "getUnlockedAchievements" => achievements::get_unlocked_achievements(app, arg_str(args, 0).unwrap_or_default(), arg_str(args, 1).unwrap_or_default()).await,
        "resetGameAchievements" => achievements::reset(app, shop(args), object_id(args)).await.map(|_| Value::Null),
        "achievementNotificationHostReady" | "achievementNotificationContentReady" | "achievementNotificationFinished" | "achievementNotificationFailed" => {
            // Only the pop-up window takes part in its handshake.
            if window.label() == achievement_popup::LABEL {
                achievement_popup::handle(app, channel, arg_str(args, 0), arg_str(args, 1));
            }
            Ok(Value::Null)
        }
        "showAchievementTestNotification" => {
            achievement_popup::show_test(app, &app.state::<Store>().get("", "userPreferences")?);
            Ok(Value::Null)
        }
        "updateAchievementCustomNotificationWindow" => {
            achievement_popup::dispose(app);
            Ok(Value::Null)
        }
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
        "showOpenDialog" => file_system::open_dialog(app, window, args.first().unwrap_or(&Value::Null)).await,
        "showItemInFolder" => {
            app.opener().reveal_item_in_dir(arg_str(args, 0).unwrap_or_default()).map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        "openFolder" => {
            let folder = arg_str(args, 0).unwrap_or_default();
            file_system::prepare_folder(folder)?;
            // shell.openPath resolves to "" on success.
            app.opener().open_path(folder, None::<&str>).map_err(|e| e.to_string())?;
            Ok(json!(""))
        }
        "clipboardWriteText" => app.clipboard().write_text(arg_str(args, 0).unwrap_or_default()).map(|_| Value::Null).map_err(|e| e.to_string()),
        "readDirectory" => {
            let dir = arg_str(args, 0).unwrap_or_default().to_string();
            file_system::assert_not_app_data(&app.path().app_data_dir().map_err(|e| e.to_string())?, &dir)?;
            tauri::async_runtime::spawn_blocking(move || file_system::read_directory(&dir)).await.map_err(|e| e.to_string())?
        }
        "getPathInfo" => {
            let path = arg_str(args, 0).unwrap_or_default();
            file_system::assert_not_app_data(&app.path().app_data_dir().map_err(|e| e.to_string())?, path)?;
            Ok(file_system::path_info(path))
        }
        "listDrives" => Ok(file_system::list_drives()),
        "getDiskFreeSpace" => {
            let path = arg_str(args, 0).unwrap_or_default().to_string();
            tauri::async_runtime::spawn_blocking(move || file_system::disk_usage(&path)).await.map_err(|e| e.to_string())
        }
        "checkFolderWritePermission" => Ok(json!(file_system::can_write(arg_str(args, 0).unwrap_or_default()))),
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
