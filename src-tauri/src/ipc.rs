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
use crate::{achievement_popup, achievements, artwork, big_picture, bundled_games, classics_import, classics_launch, common_redist, debrid, decky, download_sources, downloads, dxvk, epic, emulator_install, emulators, extraction, file_saves, file_system, game_installer, games, genres, gog, google_drive, graphics_scan, hydra_api, hydra_backup, installer, launch, launcher_import, library, memcard_saves, messapp_keys, misc, notifications, optiscaler, processed_image, profile, proton, retro_achievements, retroarch, rpcs3_root, save_backups, scan, screenshots, shop_details, shortcuts, steam, steam_shortcuts, themes, torrenting, trackers, transfer, updater};
use crate::data_location::DataDir;

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
        // Colors Noctalia (Linux desktop shell) renders on every theme change; null when absent.
        "getNoctaliaTheme" => {
            let file = app.path().config_dir().map_err(|e| e.to_string())?.join("shaiyon-theme/chimaera.css");
            Ok(std::fs::read_to_string(file).map(Value::String).unwrap_or(Value::Null))
        }
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
            let payload = args.first().unwrap_or(&Value::Null);
            let result = hydra_api::call(&app.state::<Store>(), &version, payload).await;
            if let Err(error) = &result {
                eprintln!("[hydra-api] {} {} failed: {error}", payload["method"].as_str().unwrap_or("get"), payload["url"].as_str().unwrap_or("/"));
            }
            result
        }
        // getUserData: Chimaera's profile is local, so this is LocalApi's `/profile/me`.
        "getMe" => {
            let version = app.package_info().version.to_string();
            hydra_api::call(&app.state::<Store>(), &version, &json!({ "method": "get", "url": "/profile/me" })).await
        }
        "updateProfile" => profile::update_profile(app, args.first().unwrap_or(&Value::Null)).await,
        "getProfileImageMetadata" => Ok(profile::image_metadata(arg_str(args, 0).unwrap_or_default())),
        "processProfileImage" => profile::process_profile_image(arg_str(args, 0).unwrap_or_default()),
        "cropProfileImage" => profile::crop_profile_image(arg_str(args, 0).unwrap_or_default(), args.get(1).unwrap_or(&Value::Null)),
        "saveMessappKey" => messapp_keys::save(arg_str(args, 0).unwrap_or_default(), arg_str(args, 1).unwrap_or_default()).await.map(Value::Bool),
        "loadMessappKey" => messapp_keys::load(arg_str(args, 0).unwrap_or_default()).await.map(|k| json!(k)),
        "clearMessappKey" => messapp_keys::clear(arg_str(args, 0).unwrap_or_default()).await.map(|_| Value::Null),
        // The compare route is another user's Hydra data, which local mode always blocks (same error as Electron).
        "getComparedUnlockedAchievements" => Err("user not logged in".into()),
        "getRetroAchievementsAchievements" => retro_achievements::get_achievements(app, arg_str(args, 0).unwrap_or_default(), arg_str(args, 1).unwrap_or_default(), args.get(2).and_then(Value::as_i64)).await,
        "resetRetroAchievementsAchievements" => retro_achievements::reset(app, args.first().and_then(Value::as_bool).unwrap_or(false)).map(|_| Value::Null),
        "openRetroAchievementsConnectionWindow" => retro_achievements::open_window(app).map(|_| Value::Null),
        "minimizeRetroAchievementsConnectionWindow" => {
            retro_achievements::minimize_window(app);
            Ok(Value::Null)
        }
        "closeRetroAchievementsConnectionWindow" => {
            retro_achievements::close_window(app);
            Ok(Value::Null)
        }
        "completeRetroAchievementsConnectionWindow" => {
            retro_achievements::complete_window(app);
            Ok(Value::Null)
        }
        "getAllCustomThemes" => themes::all(&app.state::<Store>()),
        "getActiveCustomTheme" => themes::active(&app.state::<Store>()),
        "getCustomThemeById" => app.state::<Store>().get("themes", arg_str(args, 0).unwrap_or_default()),
        "addCustomTheme" => themes::add(&app.state::<Store>(), args.first().unwrap_or(&Value::Null)).map(|_| Value::Null),
        "deleteCustomTheme" => app.state::<Store>().del("themes", arg_str(args, 0).unwrap_or_default()).map(|_| Value::Null),
        "deleteAllCustomThemes" => app.state::<Store>().clear("themes").map(|_| Value::Null),
        "toggleCustomTheme" => themes::toggle(app, arg_str(args, 0).unwrap_or_default(), args.get(1).and_then(Value::as_bool).unwrap_or(false)).map(|_| Value::Null),
        "updateCustomTheme" => themes::update_code(app, arg_str(args, 0).unwrap_or_default(), arg_str(args, 1).unwrap_or_default()).map(|_| Value::Null),
        "getThemeSoundPath" => themes::sound_path_of(app, arg_str(args, 0).unwrap_or_default()),
        "getThemeSoundDataUrl" => Ok(themes::sound_data_url(app, arg_str(args, 0).unwrap_or_default())),
        "copyThemeAchievementSound" => themes::copy_sound(app, arg_str(args, 0).unwrap_or_default(), arg_str(args, 1).unwrap_or_default()).map(|_| Value::Null),
        "removeThemeAchievementSound" => themes::remove_sound(app, arg_str(args, 0).unwrap_or_default()).map(|_| Value::Null),
        "importThemeSoundFromStore" => {
            let (id, name, url) = (arg_str(args, 0).unwrap_or_default(), arg_str(args, 1).unwrap_or_default(), arg_str(args, 2).unwrap_or_default());
            themes::import_sound_from_store(app, id, name, url).await.map(|_| Value::Null)
        }
        "openEditorWindow" => themes::open_editor(app, arg_str(args, 0).unwrap_or_default()).map(|_| Value::Null),
        "closeEditorWindow" => {
            themes::close_editor(app, arg_str(args, 0));
            Ok(Value::Null)
        }
        "getGraphicsOverview" => Ok(graphics_scan::overview(app, args.first().unwrap_or(&Value::Null)).await),
        "getGameUpscalers" => graphics_scan::upscalers(app, shop(args), object_id(args)).await,
        "getDxvkStatus" => dxvk::status(app, shop(args), object_id(args)).await,
        "installDxvk" => dxvk::install_for_game(app, shop(args), object_id(args), arg_str(args, 2).unwrap_or_default(), arg_str(args, 3)).await,
        "getGraphicsReleases" => match arg_str(args, 0) {
            Some("optiscaler") => optiscaler::releases().await,
            Some(build @ ("dxvk" | "gplasync")) => dxvk::releases(build).await,
            other => Err(format!("Unknown tool: {other:?}")),
        },
        "getGraphicsCache" => optiscaler::cache_list(app).await,
        "deleteGraphicsCache" => optiscaler::cache_delete(app, arg_str(args, 0).unwrap_or_default()).map(|_| Value::Null),
        "uninstallDxvk" => dxvk::uninstall_for_game(app, shop(args), object_id(args)).map(|_| Value::Null),
        "getGpuVendor" => Ok(optiscaler::vendor().await),
        "getOptiscalerGpu" => Ok(optiscaler::gpu(app).await),
        "getOptiscalerStatus" => optiscaler::status(app, shop(args), object_id(args), arg_str(args, 2)).await,
        "installOptiscaler" => optiscaler::install_for_game(app, shop(args), object_id(args), arg_str(args, 2).unwrap_or_default(), arg_str(args, 3)).await,
        "uninstallOptiscaler" => optiscaler::uninstall_for_game(app, shop(args), object_id(args)).map(|_| Value::Null),
        "setOptiPatcher" => optiscaler::set_optipatcher(app, shop(args), object_id(args), args.get(2).and_then(Value::as_bool).unwrap_or(false)).await.map(|_| Value::Null),
        "getOptiscalerProfiles" => Ok(optiscaler::profiles(app)),
        "saveOptiscalerProfile" => optiscaler::save_profile(app, arg_str(args, 0), arg_str(args, 1).unwrap_or_default(), args.get(2).unwrap_or(&Value::Null)),
        "deleteOptiscalerProfile" => optiscaler::delete_profile(app, arg_str(args, 0).unwrap_or_default()).map(|_| Value::Null),
        "applyOptiscalerProfile" => optiscaler::apply_profile(app, shop(args), object_id(args), arg_str(args, 2).unwrap_or_default()),
        "cleanupOptiscaler" => optiscaler::cleanup(app, shop(args), object_id(args), args.get(2).and_then(Value::as_bool).unwrap_or(false)).await,
        "getOptiscalerConfig" => optiscaler::get_config(app, shop(args), object_id(args)),
        "setOptiscalerConfig" => optiscaler::set_config(app, shop(args), object_id(args), arg_str(args, 2).unwrap_or_default(), arg_str(args, 3).unwrap_or_default()).map(|_| Value::Null),
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
            // Old art shows at once and refreshes behind the page, instead of the page waiting on
            // Hydra's API (seconds when it is slow). Same pattern as the shop details cache.
            if !force_fresh {
                if let Some(stale) = library::stale_assets(&app.state::<Store>(), object_id, shop)? {
                    let (app, object_id, shop) = (app.clone(), object_id.to_string(), shop.to_string());
                    tauri::async_runtime::spawn(async move {
                        if let Err(error) = library::get_game_assets(&app.state::<Store>(), &version, &object_id, &shop, false).await {
                            eprintln!("[assets] background refresh of {shop}:{object_id} failed: {error}");
                        }
                    });
                    return Ok(stale);
                }
            }
            library::get_game_assets(&app.state::<Store>(), &version, object_id, shop, force_fresh).await
        }
        // Electron merges /profile/games from Hydra, which local mode blocks, so it fails there too.
        "refreshLibraryAssets" => Err("library/refresh-failed".into()),
        "getDownloadLayoutState" => downloads::layout_state(&app.state::<Store>()),
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
        "updateGameSteamAppId" => {
            let saved = games::set_steam_app_id(&app.state::<Store>(), shop(args), object_id(args), arg_str(args, 2))?;
            app.emit("on-library-batch-complete", ()).map_err(|e| e.to_string())?;
            Ok(saved)
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
        "startSteamOAuth" => steam::start_login(app).map(|_| Value::Null),
        "getSteamIntegrationStatus" => Ok(steam::integration_status(&app.state::<Store>())),
        "disconnectSteam" => {
            let delete_imported = args.first().and_then(Value::as_bool).unwrap_or(false);
            steam::disconnect(app, delete_imported).await.map(|_| Value::Null)
        }
        "startSteamSync" => Ok(steam::start(app, "manual")),
        "cancelSteamSync" => {
            app.state::<steam::SteamSync>().cancel(app).await;
            Ok(Value::Null)
        }
        "getSteamSyncState" => Ok(app.state::<steam::SteamSync>().state()),
        "syncSteamGameOnGamePage" => Ok(json!(steam::sync_game_page(app, arg_str(args, 0).unwrap_or_default()))),
        "installGameOnSteam" => steam::install_on_steam(app, arg_str(args, 0).unwrap_or_default()).map(|_| Value::Null),
        "startGogOAuth" => gog::start_login(app).map(|_| Value::Null),
        "getGogIntegrationStatus" => Ok(gog::integration_status(&app.state::<Store>())),
        "disconnectGog" => {
            let delete_imported = args.first().and_then(Value::as_bool).unwrap_or(false);
            gog::disconnect(app, delete_imported).await.map(|_| Value::Null)
        }
        "startGogSync" => Ok(gog::start(app, "manual")),
        "cancelGogSync" => {
            app.state::<gog::GogSync>().cancel(app).await;
            Ok(Value::Null)
        }
        "getGogSyncState" => Ok(app.state::<gog::GogSync>().state()),
        "startEpicAuth" => epic::start_login(app).map(|_| Value::Null),
        "getEpicConnection" => Ok(epic::integration_status(&app.state::<Store>())),
        "disconnectEpic" => {
            let delete_imported = args.first().and_then(Value::as_bool).unwrap_or(false);
            epic::disconnect(app, delete_imported).await.map(|_| Value::Null)
        }
        "startEpicSync" => Ok(epic::start(app, "manual")),
        "cancelEpicSync" => {
            app.state::<epic::EpicSync>().cancel(app).await;
            Ok(Value::Null)
        }
        "getEpicSyncState" => Ok(app.state::<epic::EpicSync>().state()),
        "isSteamAppExecutable" => {
            let (app_id, executable) = (arg_str(args, 0).unwrap_or_default().to_string(), arg_str(args, 1).unwrap_or_default().to_string());
            tauri::async_runtime::spawn_blocking(move || {
                let libraries = steam_shortcuts::steam_path().map(|s| scan::library_folders(&s)).unwrap_or_default();
                json!(scan::is_app_executable(&libraries, &app_id, &executable))
            })
            .await
            .map_err(|e| e.to_string())
        }
        "selectGameWinePrefix" => games::set_checked_path(&app.state::<Store>(), shop(args), object_id(args), "winePrefixPath", arg_str(args, 2), games::is_wine_prefix, "Invalid wine prefix path").map(|_| Value::Null),
        "selectGameProtonPath" => games::set_checked_path(&app.state::<Store>(), shop(args), object_id(args), "protonPath", arg_str(args, 2), games::is_proton_dir, "Invalid proton path").map(|_| Value::Null),
        "downloadGameArtwork" => artwork::download_game_artwork(arg_str(args, 0).unwrap_or_default()).await.map(|path| json!(path)),
        "openGameInstaller" => game_installer::open(app, shop(args), object_id(args)).await.map(|ok| json!(ok)),
        "recheckInstalledExecutable" => Ok(json!(game_installer::recheck(app, shop(args), object_id(args)).await)),
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
            let assets_dir = app.path().chimaera_data_dir().map_err(|e| e.to_string())?.join("Assets");
            let launcher = shortcuts::launcher_exe(std::env::var("APPIMAGE").ok()).map_err(|e| e.to_string())?;
            let (app, key) = (app.clone(), games::game_key(&shop, &object_id));
            tauri::async_runtime::spawn_blocking(move || {
                steam_shortcuts::create(&app.state::<Store>(), steam_shortcuts::steam_path().as_deref(), &launcher, &assets_dir, &key, sources, open_vr)
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
        "isGamemodeAvailable" => Ok(json!(cfg!(target_os = "linux") && proton::on_path("gamemoderun"))),
        "isMangohudAvailable" => Ok(json!(cfg!(target_os = "linux") && proton::on_path("mangohud"))),
        "isWinetricksAvailable" => Ok(json!(cfg!(target_os = "linux") && proton::on_path("winetricks"))),
        "getInstalledProtonVersions" => Ok(json!(if cfg!(target_os = "linux") { proton::installed_versions() } else { Vec::new() })),
        "getGameLaunchProtonVersion" => {
            let store = app.state::<Store>();
            let game = store.get("games", &games::game_key(shop(args), object_id(args)))?;
            let path = proton::resolve(&game, &store.get("", "userPreferences")?, &proton::installed_versions());
            Ok(json!(path.and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))))
        }
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
            let assets = app.path().chimaera_data_dir().map_err(|e| e.to_string())?.join("Assets");
            artwork::copy_custom_game_asset(&assets, arg_str(args, 0).unwrap_or_default(), arg_str(args, 1).unwrap_or_default()).map(|url| json!(url))
        }
        "cleanupUnusedAssets" => {
            let assets = app.path().chimaera_data_dir().map_err(|e| e.to_string())?.join("Assets");
            artwork::cleanup_unused_assets(&app.state::<Store>(), &assets)
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
        "detectLauncherGames" => {
            let app = app.clone();
            tauri::async_runtime::spawn_blocking(move || launcher_import::detect_games(&app.state::<Store>())).await.map_err(|e| e.to_string())?
        }
        "scanInstalledGames" => {
            let extra = args.first().and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(std::path::PathBuf::from).collect();
            let flag = |i: usize| args.get(i).and_then(Value::as_bool).unwrap_or(true);
            scan::scan(app, extra, flag(1), flag(2), arg_str(args, 3).map(str::to_string)).await
        }
        "cancelScanInstalledGames" => {
            scan::cancel(arg_str(args, 0).unwrap_or_default());
            Ok(Value::Null)
        }
        "addScannedGames" => scan::add_scanned(app, args.first().unwrap_or(&Value::Null)).await,
        "importLauncherGames" => launcher_import::import_games(app, args.first().unwrap_or(&Value::Null)).await,
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
            if big_picture::focus_if_open(app) {
                return Ok(Value::Null);
            }
            if let Some(main) = app.get_webview_window("main") {
                let _ = main.show();
                let _ = main.unminimize();
                main.set_focus().map_err(|e| e.to_string())?;
            }
            Ok(Value::Null)
        }
        "openBigPictureWindow" => big_picture::open(app).map(|_| Value::Null),
        "executeSystemPowerAction" => big_picture::execute_power_action(window.label(), args.first().unwrap_or(&Value::Null)).await.map(|_| Value::Null),
        "isMainWindowOpen" => Ok(json!(app.get_webview_window("main").is_some_and(|w| w.is_visible().unwrap_or(false)))),
        "getImageDataUrl" => Ok(launch::image_data_url(arg_str(args, 0).unwrap_or_default()).await),
        // null = "no still poster", so the renderer shows the original cover.
        // shortcut: animated covers play instead of a still first frame; build posters and serve them through local: if the motion bothers people.
        "getCoverPoster" => Ok(Value::Null),
        "getClassicsImportStatus" => Ok(json!(app.state::<classics_import::ClassicsImports>().is_importing())),
        "getGameShopDetails" => {
            let (object_id, shop, language) = (arg_str(args, 0).unwrap_or_default(), arg_str(args, 1).unwrap_or_default(), arg_str(args, 2).unwrap_or("en"));
            shop_details::get_game_shop_details(app, object_id, shop, language).await
        }
        "getGameGenres" => {
            let version = app.package_info().version.to_string();
            genres::get(&app.state::<Store>(), &version, args.first().unwrap_or(&Value::Null), arg_str(args, 1).unwrap_or("en")).await
        }
        "getGameStats" => {
            let version = app.package_info().version.to_string();
            shop_details::get_game_stats(&app.state::<Store>(), &version, arg_str(args, 0).unwrap_or_default(), arg_str(args, 1).unwrap_or_default()).await
        }
        "getGameArtwork" => {
            let version = app.package_info().version.to_string();
            let page = args.get(3).and_then(Value::as_u64).unwrap_or(0);
            artwork::get_game_artwork(&version, shop(args), object_id(args), arg_str(args, 2).unwrap_or_default(), page).await
        }
        "getDownloadSources" => download_sources::list(&app.state::<Store>()),
        "addDownloadSource" => {
            let version = app.package_info().version.to_string();
            download_sources::add(&app.state::<Store>(), &version, arg_str(args, 0).unwrap_or_default()).await
        }
        "removeDownloadSource" => {
            let remove_all = args.first().and_then(Value::as_bool).unwrap_or(false);
            download_sources::remove(&app.state::<Store>(), remove_all, arg_str(args, 1)).map(|_| Value::Null)
        }
        "syncDownloadSources" => {
            let version = app.package_info().version.to_string();
            download_sources::sync(&app.state::<Store>(), &version).await.map(|_| Value::Null)
        }
        "getDownloadSourcesCheckBaseline" => download_sources::timestamp(&app.state::<Store>(), "downloadSourcesCheckBaseline"),
        "getDownloadSourcesSinceValue" => download_sources::timestamp(&app.state::<Store>(), "downloadSourcesSinceValue"),
        "getTorrentFiles" => Ok(torrenting::get_torrent_files(app, arg_str(args, 0).unwrap_or_default()).await),
        "saveGlobalTrackers" => {
            let (manual, append_manual, append_url) = (args.first().unwrap_or(&Value::Null), args.get(2).and_then(Value::as_bool).unwrap_or(false), args.get(3).and_then(Value::as_bool).unwrap_or(false));
            trackers::save(&app.state::<Store>(), manual, arg_str(args, 1), append_manual, append_url).map(|_| Value::Null)
        }
        "startGameDownload" => Ok(downloads::start_game_download(app, args.first().unwrap_or(&Value::Null)).await),
        "addGameToQueue" => Ok(downloads::add_game_to_queue(app, args.first().unwrap_or(&Value::Null)).await),
        "clearCompletedDownloads" => downloads::clear_completed(app).map(|n| json!(n)),
        "cancelGameDownload" => downloads::cancel_by_id(app, shop(args), object_id(args)).await.map(|ok| json!(ok)),
        // `confirmed` (args[2]) only matters for TorBox zip transfers, which arrive with debrid.
        "pauseGameDownload" => downloads::pause_by_id(app, shop(args), object_id(args)).await.map(|ok| json!(ok)),
        "getDownloadPauseWarning" => Ok(json!(false)),
        "resumeGameDownload" => {
            let strategy = arg_str(args, 2).unwrap_or("interruptActive");
            downloads::resume(app, shop(args), object_id(args), strategy).await.map(|ok| json!(ok))
        }
        "pauseGameSeed" => downloads::set_seeding(app, shop(args), object_id(args), false).await.map(|_| Value::Null),
        "resumeGameSeed" => downloads::set_seeding(app, shop(args), object_id(args), true).await.map(|_| Value::Null),
        "setDownloadQueuePosition" => {
            let index = args.get(2).and_then(Value::as_i64).unwrap_or(0);
            downloads::set_position(app, shop(args), object_id(args), index, true).map(|ok| json!(ok))
        }
        "setPausedDownloadPosition" => {
            let index = args.get(2).and_then(Value::as_i64).unwrap_or(0);
            downloads::set_position(app, shop(args), object_id(args), index, false).map(|ok| json!(ok))
        }
        "updateDownloadQueuePosition" => downloads::step_position(app, shop(args), object_id(args), arg_str(args, 2).unwrap_or_default()).map(|ok| json!(ok)),
        "moveDownloadPlacement" => {
            let index = args.get(3).and_then(Value::as_i64);
            downloads::move_placement(app, shop(args), object_id(args), arg_str(args, 2).unwrap_or_default(), index).await.map(|ok| json!(ok))
        }
        "getDebridFiles" => {
            let provider = args.get(1).and_then(Value::as_i64).unwrap_or(-1);
            Ok(debrid::files(&app.state::<Store>(), arg_str(args, 0).unwrap_or_default(), provider).await)
        }
        "authenticateRealDebrid" => debrid::authenticate(debrid::REAL_DEBRID, arg_str(args, 0).unwrap_or_default()).await,
        "authenticateTorBox" => debrid::authenticate(debrid::TORBOX, arg_str(args, 0).unwrap_or_default()).await,
        "authenticateAllDebrid" => debrid::authenticate(debrid::ALL_DEBRID, arg_str(args, 0).unwrap_or_default()).await,
        "authenticatePremiumize" => debrid::authenticate(debrid::PREMIUMIZE, arg_str(args, 0).unwrap_or_default()).await,
        "getBundledGameSetup" => bundled_games::setup(app, arg_str(args, 0).unwrap_or_default()),
        "openBundledGameLauncher" => bundled_games::open_launcher(app, arg_str(args, 0).unwrap_or_default()),
        "downloadBundledGame" => bundled_games::download(app, arg_str(args, 0).unwrap_or_default()).await,
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
        "getGameSaveFolder" => Ok(save_backups::save_folder(app, object_id(args)).await),
        "listLocalSaveBackups" => Ok(json!(save_backups::list_in(&save_backups::game_dir(app, shop(args), object_id(args))?))),
        "createLocalSaveBackup" => save_backups::create(app, shop(args), object_id(args), arg_str(args, 2), false).await,
        "deleteLocalSaveBackup" => save_backups::delete_in(&save_backups::game_dir(app, shop(args), object_id(args))?, arg_str(args, 2).unwrap_or_default()).map(|_| Value::Null),
        "restoreLocalSaveBackup" => save_backups::restore(app, shop(args), object_id(args), arg_str(args, 2).unwrap_or_default()).await.map(|_| Value::Null),
        "openLocalSaveBackupsFolder" => {
            let dir = save_backups::game_dir(app, shop(args), object_id(args))?;
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            app.opener().open_path(dir.to_string_lossy(), None::<&str>).map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        "getSaveBackupsDir" => save_backups::backups_dir(app).map(|dir| json!(dir)),
        "selectSaveBackupsDir" => {
            let current = save_backups::backups_dir(app)?;
            let picked = file_system::open_dialog(app, window, &json!({ "properties": ["openDirectory", "createDirectory"], "defaultPath": current })).await?;
            match picked["filePaths"][0].as_str() {
                Some(dir) => {
                    save_backups::set_backups_dir(&app.state::<Store>(), dir)?;
                    Ok(json!(dir))
                }
                None => Ok(Value::Null),
            }
        }
        "connectGoogleDrive" => google_drive::connect(app).await,
        "disconnectGoogleDrive" => {
            google_drive::disconnect().await;
            Ok(Value::Null)
        }
        "getGoogleDriveStatus" => Ok(google_drive::status()),
        "listDriveSaveBackups" => google_drive::list(shop(args), object_id(args)).await.map(|list| json!(list)),
        "uploadSaveBackupToDrive" => google_drive::upload(app, shop(args), object_id(args), arg_str(args, 2).unwrap_or_default()).await.map(|_| Value::Null),
        "downloadDriveSaveBackup" => google_drive::download(app, shop(args), object_id(args), arg_str(args, 2).unwrap_or_default()).await.map(|_| Value::Null),
        "deleteDriveSaveBackup" => google_drive::delete(arg_str(args, 0).unwrap_or_default()).await.map(|_| Value::Null),
        "setAutomaticCloudSync" => {
            let on = args.get(2).and_then(Value::as_bool).unwrap_or(false);
            games::set_field(&app.state::<Store>(), shop(args), object_id(args), "automaticCloudSync", json!(on)).map(|_| Value::Null)
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
            if let Some(limit) = patch.get("maxDownloadSpeedBytesPerSecond") {
                downloads::apply_speed_limit(app, limit).await;
            }
            if let Some(interface) = patch.get("torrentNetworkInterface") {
                downloads::apply_network_interface(interface.as_str()).await;
            }
            // shortcut: Wine prefix pinning reacts to these preferences in Electron; it moves here with Linux support.
            app.emit("on-user-preferences-updated", [&updated]).map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        "getLocalNotificationsCount" => notifications::unread_count(&app.state::<Store>()).map(|n| json!(n)),
        "getLocalNotifications" => notifications::list(&app.state::<Store>()),
        "markLocalNotificationRead" => notifications::set_read(&app.state::<Store>(), arg_str(args, 0).unwrap_or_default(), true).map(|_| Value::Null),
        "markLocalNotificationUnread" => notifications::set_read(&app.state::<Store>(), arg_str(args, 0).unwrap_or_default(), false).map(|_| Value::Null),
        "markAllLocalNotificationsRead" => notifications::mark_all_read(&app.state::<Store>()).map(|_| Value::Null),
        "deleteLocalNotification" => notifications::delete(&app.state::<Store>(), arg_str(args, 0).unwrap_or_default()).map(|_| Value::Null),
        "clearAllLocalNotifications" => notifications::clear_all(&app.state::<Store>()).map(|_| Value::Null),
        "getScreenshotsPath" => screenshots::dir(app).map(|dir| json!(dir)),
        // Souvenirs stay on disk (no Hydra sign-in), so nothing is ever waiting to sync.
        "getAchievementSouvenirSyncStatus" => Ok(screenshots::idle_status()),
        "getAchievementSouvenirSyncDetails" => Ok(json!({ "status": screenshots::idle_status(), "items": [] })),
        "retryAchievementSouvenirSync" => Ok(json!({ "status": screenshots::idle_status(), "attemptedCount": 0, "syncedCount": 0, "missingScreenshotCount": 0 })),
        "cleanupAchievementSouvenirSync" => Ok(json!({ "status": screenshots::idle_status(), "deletedCount": 0, "failedFilePaths": [] })),
        // Souvenir ids exist only on the Hydra server, which this build does not reach.
        "deleteAchievementSouvenir" => Ok(Value::Null),
        "transferGameFiles" => Ok(transfer::transfer(app, shop(args), object_id(args), arg_str(args, 2).unwrap_or_default()).await),
        "cancelGameTransfer" => {
            transfer::cancel(shop(args), object_id(args));
            Ok(Value::Null)
        }
        "pauseGameTransfer" | "resumeGameTransfer" => {
            transfer::set_paused(shop(args), object_id(args), channel == "pauseGameTransfer");
            Ok(Value::Null)
        }
        // Hydra Cloud saves: the Electron main process never registered these (the preload kept
        // them after Hydra Cloud went away) and the renderer does not call them.
        "syncGameCloudSave" | "syncGameCloudSaveFromModal" | "syncCloudSaveAfterCustomPathRebind" | "resolveCloudSaveConflict" => {
            Err("cloud-save/unavailable: Hydra Cloud saves are not part of Chimaera; use save backups".into())
        }
        "importHydraBackup" => {
            let root = save_backups::data_dir(app, "hydra-backup")?;
            let picked = file_system::open_dialog(app, window, &json!({ "properties": ["openDirectory"], "defaultPath": root })).await?;
            match picked["filePaths"][0].as_str() {
                Some(dir) if picked["canceled"] != json!(true) => hydra_backup::import(app, std::path::Path::new(dir)).await,
                _ => Ok(Value::Null),
            }
        }
        "getHydraDeckyPluginInfo" => decky::info(app).await,
        "installHydraDeckyPlugin" => Ok(decky::install(app).await),
        "openHydraBackupsFolder" => {
            let dir = save_backups::data_dir(app, "hydra-backup")?;
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            app.opener().open_path(dir.to_string_lossy(), None::<&str>).map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        "extractGameDownload" => Ok(json!(extraction::start(app, shop(args), object_id(args), false))),
        "autoLaunch" => misc::auto_launch(args.first().unwrap_or(&Value::Null)).map(|_| Value::Null),
        "canInstallCommonRedist" => Ok(json!(common_redist::can_install(app))),
        "installCommonRedist" => common_redist::install(app).map(|_| Value::Null),
        "resetCommonRedistPreflight" => common_redist::reset_preflight(&app.state::<Store>()).map(|_| Value::Null),
        "checkHomebrewFolderExists" => Ok(json!(misc::homebrew_folder_exists(app))),
        "saveTempFile" => misc::save_temp_file(arg_str(args, 0).unwrap_or_default(), args.get(1).unwrap_or(&Value::Null)).map(|p| json!(p)),
        "deleteTempFile" => {
            misc::delete_temp_file(arg_str(args, 0).unwrap_or_default());
            Ok(Value::Null)
        }
        "getNetworkInterfaces" => Ok(misc::network_interfaces()),
        "getProcessedImage" => Ok(processed_image::get(app, arg_str(args, 0), args.get(1).unwrap_or(&Value::Null)).await),
        // shortcut: Wine is not ported (Linux), and Windows has no default prefix.
        "getDefaultWinePrefixSelectionPath" => Ok(if cfg!(target_os = "linux") { json!(proton::default_prefix_root(&app.state::<Store>().get("", "userPreferences")?).to_string_lossy()) } else { Value::Null }),
        "publishNewRepacksNotification" => {
            notifications::new_repacks(app, args.first().and_then(Value::as_i64).unwrap_or(0));
            Ok(Value::Null)
        }
        // No remote library sync exists in this build yet, so it reports its real idle state.
        "getRemoteLibrarySyncState" => Ok(json!(false)),
        "getActiveClassicsImport" => Ok(app.state::<classics_import::ClassicsImports>().active()),
        "getActiveRetroArchImport" => Ok(app.state::<retroarch::RetroArchImports>().active()),
        "getRetroArchConfig" => retroarch::config_checked(&app.state::<Store>()),
        "checkRetroArchExecutable" => {
            let config = retroarch::get_config(&app.state::<Store>())?;
            Ok(json!({ "exists": config["executablePath"].as_str().is_some_and(|p| std::path::Path::new(p).exists()) }))
        }
        "removeRetroArch" => retroarch::remove(&app.state::<Store>()),
        "setRetroArchCoresDir" => {
            let managed = save_backups::data_dir(app, "retroarch")?;
            retroarch::set_cores_dir(&app.state::<Store>(), &managed, arg_str(args, 0))
        }
        "detectRetroArch" | "setRetroArchExecutablePath" | "previewRetroArchExecutable" | "previewRetroArchRomFolder" => {
            // Version probes, folder walks and archive listings block for seconds.
            let (app, channel, args) = (app.clone(), channel.to_string(), args.to_vec());
            tauri::async_runtime::spawn_blocking(move || match channel.as_str() {
                "detectRetroArch" => retroarch::detect_and_save(&app),
                "setRetroArchExecutablePath" => retroarch::set_executable_path(&app, arg_str(&args, 0)),
                "previewRetroArchExecutable" => retroarch::preview_executable(&app, arg_str(&args, 0)),
                _ => Ok(retroarch::preview_folder(&app, arg_str(&args, 0).unwrap_or_default(), args.get(1).and_then(Value::as_bool).unwrap_or(false))),
            })
            .await
            .map_err(|e| e.to_string())?
        }
        "addRetroArchRomFolder" => retroarch::add_folder(&app.state::<Store>(), arg_str(args, 0).unwrap_or_default(), args.get(1).and_then(Value::as_bool).unwrap_or(false)),
        "changeRetroArchRomFolder" => retroarch::change_folder(&app.state::<Store>(), arg_str(args, 0).unwrap_or_default(), arg_str(args, 1).unwrap_or_default()),
        "removeRetroArchRomFolder" => retroarch::remove_folder(&app.state::<Store>(), arg_str(args, 0).unwrap_or_default()),
        "toggleRetroArchSubfolders" => retroarch::toggle_subfolders(&app.state::<Store>(), arg_str(args, 0).unwrap_or_default(), args.get(1).and_then(Value::as_bool).unwrap_or(false)),
        "listRetroArchRoms" => retroarch::list_roms(&app.state::<Store>()),
        "importRetroArchRoms" => {
            let folders = classics_import::folders_arg(args.first().unwrap_or(&Value::Null));
            let (request_id, _) = retroarch::start(app, folders, arg_str(args, 1).unwrap_or("en"));
            Ok(json!({ "requestId": request_id }))
        }
        "cancelRetroArchImport" => {
            app.state::<retroarch::RetroArchImports>().cancel(arg_str(args, 0).unwrap_or_default());
            Ok(Value::Null)
        }
        "rescanRetroArch" => retroarch::rescan(app, arg_str(args, 0).unwrap_or("en")).await,
        "getRetroArchInstallOptions" => Ok(retroarch::install_options().await),
        "installRetroArch" => Ok(retroarch::install(app, arg_str(args, 0).unwrap_or_default()).await),
        "installRetroArchCore" => Ok(retroarch::install_core(app, arg_str(args, 0).unwrap_or_default()).await),
        "installAllRetroArchCores" => Ok(retroarch::install_all_cores(app).await),
        "openGameWinetricks" => emulators::open_winetricks(&app.state::<Store>(), shop(args), object_id(args)).map(Value::from),
        "getEmulatorConfigs" => emulators::all_configs(&app.state::<Store>()),
        "getEmulatorRomExtensions" => {
            let k = emulators::known(arg_str(args, 0).unwrap_or_default())?;
            Ok(json!(k.rom_extensions.iter().map(|e| e.trim_start_matches('.')).collect::<Vec<_>>()))
        }
        "checkEmulatorExecutable" => {
            let config = emulators::get_config(&app.state::<Store>(), arg_str(args, 0).unwrap_or_default())?;
            Ok(json!({ "exists": config["executablePath"].as_str().is_some_and(|p| std::path::Path::new(p).exists()) }))
        }
        "setEmulatorBiosPath" => emulators::set_bios_path(&app.state::<Store>(), arg_str(args, 0).unwrap_or_default(), arg_str(args, 1)),
        "removeEmulator" => emulators::remove(&app.state::<Store>(), arg_str(args, 0).unwrap_or_default()),
        "setEmulatorExecutablePath" | "previewEmulatorExecutable" | "detectEmulator" | "detectEmulators" => {
            // Version probes and folder walks block for seconds.
            let (app, channel, args) = (app.clone(), channel.to_string(), args.to_vec());
            tauri::async_runtime::spawn_blocking(move || -> Result<Value, String> {
                let store = app.state::<Store>();
                let default_downloads = app.path().download_dir().map_err(|e| e.to_string())?;
                let dirs = emulators::download_dirs(&store, &default_downloads);
                if channel == "detectEmulators" {
                    let mut map = serde_json::Map::new();
                    for k in &emulators::KNOWN {
                        map.insert(k.system.into(), emulators::apply_detection(&store, k, emulators::detect(k, &dirs, false))?);
                    }
                    return Ok(Value::Object(map));
                }
                let k = emulators::known(arg_str(&args, 0).unwrap_or_default())?;
                match channel.as_str() {
                    "setEmulatorExecutablePath" => emulators::set_executable_path(&store, k, arg_str(&args, 1)),
                    "previewEmulatorExecutable" => Ok(emulators::preview(k, arg_str(&args, 1), &dirs)),
                    _ => emulators::apply_detection(&store, k, emulators::detect(k, &dirs, true)),
                }
            })
            .await
            .map_err(|e| e.to_string())?
        }
        "updateClassicsDisc" => classics_launch::update_disc(&app.state::<Store>(), shop(args), object_id(args), args.get(2).unwrap_or(&Value::Null)),
        "openClassicsGame" => {
            let force = args.get(3).and_then(Value::as_bool).unwrap_or(false);
            classics_launch::open(app, shop(args), object_id(args), arg_str(args, 2), force).await.map(|_| Value::Null)
        }
        "checkEmulatorBios" => {
            let (app, args) = (app.clone(), args.to_vec());
            tauri::async_runtime::spawn_blocking(move || classics_launch::check_bios(&app.state::<Store>(), arg_str(&args, 0).unwrap_or_default(), arg_str(&args, 1), arg_str(&args, 2)))
                .await
                .map_err(|e| e.to_string())?
        }
        "importLaunchboxRoms" => {
            let folders = classics_import::folders_arg(args.get(1).unwrap_or(&Value::Null));
            Ok(classics_import::start(app, arg_str(args, 0).unwrap_or_default(), folders, arg_str(args, 2).unwrap_or("en")))
        }
        "cancelLaunchboxImport" => {
            app.state::<classics_import::ClassicsImports>().cancel(arg_str(args, 0).unwrap_or_default());
            Ok(Value::Null)
        }
        "addRomFolder" => {
            let (system, path) = (arg_str(args, 0).unwrap_or_default(), arg_str(args, 1).unwrap_or_default());
            let current = emulators::get_config(&app.state::<Store>(), system)?;
            if current["romFolders"].as_array().into_iter().flatten().any(|f| f["path"].as_str() == Some(path)) {
                return Ok(current);
            }
            let folder = classics_import::Folder { path: path.to_string(), scan_subfolders: args.get(2).and_then(Value::as_bool).unwrap_or(false) };
            classics_import::import_and_config(app, system, vec![folder], arg_str(args, 3).unwrap_or("en")).await
        }
        "rescanEmulator" => {
            let system = arg_str(args, 0).unwrap_or_default();
            let store = app.state::<Store>();
            let current = emulators::get_config(&store, system)?;
            let folders: Vec<classics_import::Folder> = current["romFolders"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|f| Some(classics_import::Folder { path: f["path"].as_str()?.to_string(), scan_subfolders: f["scanSubfolders"].as_bool().unwrap_or(false) }))
                .collect();
            if folders.is_empty() {
                return emulators::update_config(&store, system, classics_import::recompute_totals);
            }
            classics_import::import_and_config(app, system, folders, arg_str(args, 1).unwrap_or("en")).await
        }
        "registerRomFolder" => {
            let scan_subfolders = args.get(2).and_then(Value::as_bool).unwrap_or(false);
            classics_import::ensure_registered(&app.state::<Store>(), arg_str(args, 0).unwrap_or_default(), arg_str(args, 1).unwrap_or_default(), scan_subfolders)
        }
        "removeRomFolder" => classics_import::remove_folder(&app.state::<Store>(), arg_str(args, 0).unwrap_or_default(), arg_str(args, 1).unwrap_or_default()),
        "getEmulatorRomPaths" => classics_import::read_recursive_paths(&app.state::<Store>(), arg_str(args, 0).unwrap_or_default()),
        "addEmulatorRomPath" => classics_import::add_recursive_path(&app.state::<Store>(), arg_str(args, 0).unwrap_or_default(), arg_str(args, 1).unwrap_or_default()),
        "getRpcs3DefaultSources" => classics_import::rpcs3_default_sources(&app.state::<Store>()),
        "getRpcs3ConfigRootStatus" => rpcs3_root::status(&app.state::<Store>()),
        "setRpcs3ConfigRoot" => rpcs3_root::set_root(&app.state::<Store>(), arg_str(args, 0).unwrap_or_default()),
        "checkPs3Firmware" => Ok(json!({ "installed": rpcs3_root::firmware_installed(arg_str(args, 0)) })),
        "getEmulatorInstallOptions" => Ok(Value::Array(emulator_install::options(arg_str(args, 0).unwrap_or_default()).await)),
        "installEmulator" => Ok(emulator_install::install(app, arg_str(args, 0).unwrap_or_default(), arg_str(args, 1).unwrap_or_default()).await),
        "scanPs1Memcards" | "scanPs2Memcards" => {
            let platform = if channel == "scanPs1Memcards" { memcard_saves::Platform::Ps1 } else { memcard_saves::Platform::Ps2 };
            Ok(memcard_saves::start_scan(app, platform, args.first().cloned().unwrap_or(Value::Null)))
        }
        "cancelPs1MemcardScan" | "cancelPs2MemcardScan" => {
            memcard_saves::cancel_scan(arg_str(args, 0).unwrap_or_default());
            Ok(Value::Null)
        }
        "listPs1MemcardSaves" => memcard_saves::list(&app.state::<Store>(), memcard_saves::Platform::Ps1),
        "listPs2MemcardSaves" => memcard_saves::list(&app.state::<Store>(), memcard_saves::Platform::Ps2),
        "forgetPs1MemcardSave" | "forgetPs2MemcardSave" => {
            let platform = if channel == "forgetPs1MemcardSave" { memcard_saves::Platform::Ps1 } else { memcard_saves::Platform::Ps2 };
            memcard_saves::forget_save(&app.state::<Store>(), platform, arg_str(args, 0).unwrap_or_default(), arg_str(args, 1).unwrap_or_default()).map(|_| Value::Null)
        }
        "forgetPs1MemcardCard" | "forgetPs2MemcardCard" => {
            let platform = if channel == "forgetPs1MemcardCard" { memcard_saves::Platform::Ps1 } else { memcard_saves::Platform::Ps2 };
            memcard_saves::forget_card(&app.state::<Store>(), platform, arg_str(args, 0).unwrap_or_default()).map(|_| Value::Null)
        }
        "listLocalEmulationSaves" => Ok(file_saves::list(app, arg_str(args, 0).unwrap_or_default()).await),
        "inspectMemcard" => {
            let (platform, card) = (arg_str(args, 0).unwrap_or_default().to_string(), arg_str(args, 1).unwrap_or_default().to_string());
            tauri::async_runtime::spawn_blocking(move || json!(memcard_saves::inspect(&platform, &card))).await.map_err(|e| e.to_string())
        }
        "exportPs1Save" | "exportPs2Save" => {
            // The save dialog blocks until the user answers.
            let platform = if channel == "exportPs1Save" { memcard_saves::Platform::Ps1 } else { memcard_saves::Platform::Ps2 };
            let (app, args) = (app.clone(), args.to_vec());
            tauri::async_runtime::spawn_blocking(move || {
                memcard_saves::export(&app, platform, arg_str(&args, 0).unwrap_or_default(), arg_str(&args, 1).unwrap_or_default(), arg_str(&args, 2).unwrap_or_default())
            })
            .await
            .map_err(|e| e.to_string())
        }
        "listEmulatorRoms" => classics_import::list_roms(&app.state::<Store>(), arg_str(args, 0).unwrap_or_default()),
        "previewRomFolder" | "toggleRomFolderSubfolders" => {
            // Folder walks and disc reads block.
            let (app, channel, args) = (app.clone(), channel.to_string(), args.to_vec());
            tauri::async_runtime::spawn_blocking(move || {
                let (system, target) = (arg_str(&args, 0).unwrap_or_default(), arg_str(&args, 1).unwrap_or_default());
                let scan_subfolders = args.get(2).and_then(Value::as_bool).unwrap_or(false);
                if channel == "previewRomFolder" {
                    emulators::known(system)?;
                    classics_import::preview_folder(system, target, scan_subfolders)
                } else {
                    classics_import::toggle_subfolders(&app.state::<Store>(), system, target, scan_subfolders)
                }
            })
            .await
            .map_err(|e| e.to_string())?
        }
        "checkForUpdates" => Ok(updater::check(app)),
        "restartAndInstallUpdate" => updater::restart_and_install(app),
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
            file_system::assert_not_app_data(&app.path().chimaera_data_dir().map_err(|e| e.to_string())?, &dir)?;
            tauri::async_runtime::spawn_blocking(move || file_system::read_directory(&dir)).await.map_err(|e| e.to_string())?
        }
        "getPathInfo" => {
            let path = arg_str(args, 0).unwrap_or_default();
            file_system::assert_not_app_data(&app.path().chimaera_data_dir().map_err(|e| e.to_string())?, path)?;
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
