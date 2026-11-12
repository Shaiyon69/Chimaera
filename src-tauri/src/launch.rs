//! `openGame` / `closeGame` and the game-launcher window: the Windows path of
//! src/main/helpers/launch-game.ts, events/library/close-game.ts and
//! WindowManager.createGameLauncherWindow.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

use crate::games::{game_key, with_executable_path};
use crate::store::Store;

const LAUNCH_DELAY_MS: u64 = 2_000;
pub const LAUNCHER_LABEL: &str = "game-launcher";

/// Latest status per game and group ("compatibility" / "achievements"), as game-launcher-status.ts keeps them.
#[derive(Default)]
pub struct LauncherStatuses(Mutex<HashMap<String, Vec<(String, Value)>>>);

impl LauncherStatuses {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<(String, Value)>>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn get(&self, key: &str) -> Value {
        Value::Array(self.lock().get(key).map(|s| s.iter().map(|(_, p)| p.clone()).collect()).unwrap_or_default())
    }

    pub fn clear(&self, key: &str) {
        self.lock().remove(key);
    }

    pub fn send(&self, app: &AppHandle, key: &str, status: &str) {
        let group = if matches!(status, "preparing_compatibility_layer" | "compatibility_layer_ready" | "compatibility_layer_failed" | "game_started") {
            "compatibility"
        } else {
            "achievements"
        };
        let payload = json!({ "gameKey": key, "status": status, "detail": null });
        let mut all = self.lock();
        let statuses = all.entry(key.to_string()).or_default();
        statuses.retain(|(g, _)| g != group);
        statuses.push((group.to_string(), payload.clone()));
        drop(all);
        let _ = app.emit_to(LAUNCHER_LABEL, "game-launcher-status", [payload]);
    }
}

/// parseExecutablePath: a Windows shortcut stands for its target.
pub fn parse_executable_path(path: &str) -> String {
    #[cfg(windows)]
    if path.to_lowercase().ends_with(".lnk") {
        // lnk's link_target panics on malformed LinkInfo; a bad shortcut keeps its own path.
        let target = std::panic::catch_unwind(|| lnk::ShellLink::open(path, encoding_rs::WINDOWS_1252).ok()?.link_target());
        if let Ok(Some(target)) = target {
            return target;
        }
    }
    path.to_string()
}

/// string-argv: whitespace-separated, quotes group words and are dropped around a whole token.
pub fn split_args(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for c in input.chars() {
        match quote {
            Some(q) if c == q => {
                quote = None;
                current.push(c);
            }
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                current.push(c);
            }
            None if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
        .into_iter()
        .map(|t| {
            let quoted = t.len() >= 2 && (t.starts_with('"') && t.ends_with('"') || t.starts_with('\'') && t.ends_with('\''));
            if quoted { t[1..t.len() - 1].to_string() } else { t }
        })
        .collect()
}

#[derive(Debug, PartialEq)]
pub struct LaunchCommand {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// resolveLaunchCommand without wrappers (gamemode / mangohud are Linux-only).
pub fn resolve_launch_command(base: &str, launch_options: Option<&str>) -> LaunchCommand {
    let tokens = split_args(launch_options.unwrap_or_default());
    let plain = |args| LaunchCommand { command: base.to_string(), args, env: Vec::new() };
    if !tokens.iter().any(|t| t == "%command%") {
        return plain(tokens);
    }
    let expanded: Vec<String> = tokens.into_iter().map(|t| if t == "%command%" { base.to_string() } else { t }).collect();
    let env_count = expanded.iter().take_while(|t| t.find('=').is_some_and(|i| i > 0 && is_env_name(&t[..i]))).count();
    let env = expanded[..env_count].iter().map(|t| t.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())).unwrap()).collect();
    let rest = &expanded[env_count..];
    match rest.split_first() {
        Some((command, args)) => LaunchCommand { command: command.clone(), args: args.to_vec(), env },
        None => LaunchCommand { env, ..plain(Vec::new()) },
    }
}

fn is_windows_binary(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with(".exe") || is_batch_file(&lower)
}

/// Steam starts its own games with the Proton build the user picked for them.
fn steam_run_url(shop: &str, object_id: &str) -> Option<String> {
    (shop == "steam" && !object_id.is_empty() && object_id.bytes().all(|b| b.is_ascii_digit())).then(|| format!("steam://rungameid/{object_id}"))
}

/// Runs the resolved command under Wine, keeping its env and arguments.
fn wine_command(launch: LaunchCommand) -> LaunchCommand {
    LaunchCommand { command: "wine".into(), args: std::iter::once(launch.command).chain(launch.args).collect(), env: launch.env }
}

pub fn is_batch_file(command: &str) -> bool {
    let lower = command.to_lowercase();
    lower.ends_with(".bat") || lower.ends_with(".cmd")
}

/// buildWindowsBatchCommand: every part quoted, inner quotes doubled.
pub fn batch_command_line(command: &str, args: &[String]) -> String {
    std::iter::once(command).chain(args.iter().map(String::as_str)).map(|v| format!("\"{}\"", v.replace('"', "\"\""))).collect::<Vec<_>>().join(" ")
}

fn spawn_detached(launch: &LaunchCommand, cwd: &Path) -> std::io::Result<()> {
    let mut command = if cfg!(windows) && is_batch_file(&launch.command) {
        let mut cmd = std::process::Command::new("cmd.exe");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // What Node's spawn(..., { shell: true }) runs.
            cmd.raw_arg(format!("/d /s /c \"{}\"", batch_command_line(&launch.command, &launch.args)));
        }
        cmd
    } else {
        let mut cmd = std::process::Command::new(&launch.command);
        cmd.args(&launch.args);
        cmd
    };
    command.current_dir(cwd).envs(launch.env.iter().cloned()).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    command.spawn().map(|_| ())
}

/// `launchExternal`: a launcher URI, or a launcher client exe with its arguments.
fn launch_external(app: &AppHandle, launch: &Value) -> Result<(), String> {
    match launch["kind"].as_str() {
        Some("uri") => app.opener().open_url(launch["uri"].as_str().unwrap_or_default(), None::<&str>).map_err(|e| e.to_string()),
        Some("exe") => {
            let command = launch["path"].as_str().unwrap_or_default().to_string();
            let args = launch["args"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
            let cwd = Path::new(&command).parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf();
            spawn_detached(&LaunchCommand { command, args, env: vec![] }, &cwd).map_err(|e| e.to_string())
        }
        other => Err(format!("unknown external launch kind {other:?}")),
    }
}

/// The record after launching `exe` for it. The exe is saved only when the record has none
/// that still exists: playing another copy of a grouped game keeps this record's own install.
/// `lastLaunchedAt` tells the playtime watcher which of several records sharing an exe ran it.
pub fn record_launch(game: Value, exe: &str, launch_options: Option<&str>, own_exe_exists: bool) -> Value {
    let mut game = if own_exe_exists { game } else { with_executable_path(game, Some(exe)) };
    game["launchOptions"] = json!(launch_options);
    game["lastLaunchedAt"] = json!(crate::library::iso_now());
    game
}

/// `openGame`.
// shortcut: bundled games (Bloodborne, Horizon) are not ported; they land with their milestone.
pub async fn open_game(app: &AppHandle, shop: &str, object_id: &str, executable_path: &str, launch_options: Option<&str>) -> Result<Value, String> {
    let store = app.state::<Store>();
    let key = game_key(shop, object_id);
    let parsed = parse_executable_path(executable_path);
    // Imported launcher games start through their own launcher; the watcher still tracks playtime.
    if let Some(launch) = store.get("games", &key)?.get("externalLaunch").filter(|l| l.is_object()) {
        match launch_external(app, launch) {
            Ok(()) => return Ok(Value::Null),
            Err(error) => eprintln!("[launch] external launch failed for {key}, falling back to the executable: {error}"),
        }
    }
    if executable_path.is_empty() || !Path::new(executable_path).exists() || !Path::new(&parsed).exists() {
        app.emit("on-game-executable-not-found", [shop, object_id]).map_err(|e| e.to_string())?;
        return Ok(Value::Null);
    }
    let game = store.get("games", &key)?;
    if !game.is_null() {
        let own_exists = game["executablePath"].as_str().is_some_and(|own| !own.is_empty() && Path::new(&parse_executable_path(own)).exists());
        store.put("games", &key, &record_launch(game, &parsed, launch_options, own_exists))?;
    }

    app.state::<LauncherStatuses>().clear(&key);
    open_launcher_window(app, shop, object_id)?;
    crate::common_redist::preflight(app).await;
    tokio::time::sleep(std::time::Duration::from_millis(LAUNCH_DELAY_MS)).await;

    let launch = resolve_launch_command(&parsed, launch_options);
    let cwd = Path::new(&parsed).parent().unwrap_or(Path::new("."));
    if !cfg!(windows) && is_windows_binary(&parsed) {
        // Opening an .exe on Linux hands it to the desktop's default app (a browser downloads it).
        let steam_url = steam_run_url(shop, object_id).filter(|_| crate::proton::is_steam_install(&parsed));
        let result = match steam_url {
            Some(url) => app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string()),
            None if crate::proton::on_path("umu-run") => proton_launch(app, &store, &key, steam_run_url(shop, object_id).map(|_| object_id), &parsed, launch, cwd),
            None => spawn_detached(&wine_command(launch), cwd).map_err(|e| e.to_string()),
        };
        if let Err(error) = result {
            eprintln!("[launch] failed to launch {key}: {error}");
        }
        return Ok(Value::Null);
    }
    if launch.command == parsed && launch.args.is_empty() && launch.env.is_empty() {
        // shell.openPath: lets Windows pick the handler and UAC prompt, as Electron did.
        app.opener().open_path(&parsed, None::<&str>).map_err(|e| e.to_string())?;
    } else if let Err(error) = spawn_detached(&launch, cwd) {
        eprintln!("[launch] failed to launch {key}: {error}");
    }
    Ok(Value::Null)
}

/// Runs a Windows game through umu-run with the game's Proton and its own prefix.
fn proton_launch(app: &AppHandle, store: &Store, key: &str, steam_id: Option<&str>, exe: &str, launch: LaunchCommand, cwd: &Path) -> Result<(), String> {
    let game = store.get("games", key)?;
    let prefs = store.get("", "userPreferences")?;
    let statuses = app.state::<LauncherStatuses>();
    let Some(proton) = crate::proton::resolve(&game, &prefs, &crate::proton::installed_versions()) else {
        statuses.send(app, key, "compatibility_layer_failed");
        return Err("no Proton build found; install GE-Proton with ProtonPlus".into());
    };
    statuses.send(app, key, "preparing_compatibility_layer");
    let prefix = crate::proton::prefix_for(&game, &prefs, key);
    std::fs::create_dir_all(&prefix).map_err(|e| format!("cannot create prefix {}: {e}", prefix.display()))?;
    let gamemode = game["autoRunGamemode"].as_bool().unwrap_or(false) && crate::proton::on_path("gamemoderun");
    let mangohud = game["autoRunMangohud"].as_bool().unwrap_or(false);
    let command = crate::proton::umu_command(launch, exe, &proton, &prefix, steam_id, gamemode, mangohud);
    spawn_detached(&command, cwd).map_err(|e| e.to_string())?;
    statuses.send(app, key, "game_started");
    Ok(())
}

fn path_key(path: &str) -> String {
    path.replace('/', "\\").to_lowercase()
}

/// `closeGame`: kills the processes running the game's exe or tracking exes.
// shortcut: no emulator sessions yet.
pub fn close_game(store: &Store, shop: &str, object_id: &str) -> Result<Value, String> {
    let game = store.get("games", &game_key(shop, object_id))?;
    if game.is_null() {
        return Ok(Value::Null);
    }
    let mut targets: Vec<String> = game["trackingExecutablePaths"].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|p| !p.is_empty()).map(path_key).collect();
    if let Some(exe) = game["executablePath"].as_str().filter(|e| !is_batch_file(e)) {
        targets.push(path_key(exe));
    }
    let mut system = sysinfo::System::new();
    system.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, true, sysinfo::ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::Always));
    let folder = game["executablePath"].as_str().and_then(crate::watcher::linux_game_folder);
    if folder.is_some() {
        system.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, true, sysinfo::ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::Always).with_cwd(sysinfo::UpdateKind::Always));
    }
    let in_game_folder = |p: Option<&Path>| p.zip(folder.as_deref()).is_some_and(|(p, f)| crate::watcher::in_folder(&path_key(&p.to_string_lossy()), f));
    for process in system.processes().values() {
        let matched = process.exe().is_some_and(|exe| targets.contains(&path_key(&exe.to_string_lossy()))) || in_game_folder(process.exe()) || in_game_folder(process.cwd());
        if matched && !process.kill() {
            eprintln!("[launch] could not kill pid {}", process.pid());
        }
    }
    Ok(Value::Null)
}

/// The "launching game" splash: hidden until its page calls showGameLauncherWindow.
pub fn open_launcher_window(app: &AppHandle, shop: &str, object_id: &str) -> Result<(), String> {
    if let Some(existing) = app.get_webview_window(LAUNCHER_LABEL) {
        let _ = existing.destroy();
    }
    let query = url::form_urlencoded::Serializer::new(String::new()).append_pair("shop", shop).append_pair("objectId", object_id).finish();
    let mut builder = WebviewWindowBuilder::new(app, LAUNCHER_LABEL, WebviewUrl::App(format!("tauri.html#/game-launcher?{query}").into()));
    // Tiling compositors (niri, Hyprland, Sway) float a child window at its own size instead of tiling it.
    if cfg!(target_os = "linux") {
        if let Some(main) = app.get_webview_window("main") {
            builder = builder.parent(&main).map_err(|e| e.to_string())?;
        }
    }
    builder
        .title("Chimaera")
        .inner_size(550.0, 320.0)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .decorations(false)
        .center()
        .visible(false)
        .background_color(tauri::webview::Color(0x0c, 0x0c, 0x0c, 0xff))
        .initialization_script(crate::process_script())
        .build()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub fn close_launcher_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LAUNCHER_LABEL) {
        let _ = window.close();
    }
}

/// `getImageDataUrl`: a remote cover as a data: URL, so the launcher can read its pixels for the
/// accent colour without a tainted canvas. Null for non-http(s) URLs or a failed fetch.
pub async fn image_data_url(url: &str) -> Value {
    use base64::Engine;
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Value::Null;
    }
    let Ok(response) = reqwest::get(url).await else { return Value::Null };
    if !response.status().is_success() {
        return Value::Null;
    }
    let content_type = response.headers().get("content-type").and_then(|v| v.to_str().ok()).and_then(|v| v.split(';').next()).map(str::trim).map(String::from);
    let Ok(bytes) = response.bytes().await else { return Value::Null };
    let extension = url::Url::parse(url).ok().and_then(|u| u.path().rsplit_once('.').map(|(_, e)| e.to_lowercase())).unwrap_or_default();
    let by_extension = match extension.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        _ => "image/png",
    };
    let mime = content_type.filter(|t| t.starts_with("image/")).unwrap_or_else(|| by_extension.into());
    json!(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(&bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_launch_routes() {
        assert!(is_windows_binary("/g/Terraria.EXE") && is_windows_binary("/g/run.bat") && !is_windows_binary("/g/terraria.sh"));
        assert_eq!(steam_run_url("steam", "105600").as_deref(), Some("steam://rungameid/105600"));
        assert_eq!(steam_run_url("steam", "1/../x"), None);
        assert_eq!(steam_run_url("custom", "105600"), None);
        let wine = wine_command(LaunchCommand { command: "/g/a.exe".into(), args: vec!["-x".into()], env: vec![("A".into(), "1".into())] });
        assert_eq!((wine.command.as_str(), wine.args), ("wine", vec!["/g/a.exe".to_string(), "-x".to_string()]));
    }

    #[test]
    fn launching_another_copy_keeps_the_records_own_exe() {
        let game = json!({ "executablePath": r"C:\Steam\ln2.exe" });
        let after = record_launch(game, r"C:\GOG\ln2.exe", Some("-x"), true);
        assert_eq!(after["executablePath"], r"C:\Steam\ln2.exe");
        assert_eq!(after["launchOptions"], "-x");
        assert!(after["lastLaunchedAt"].is_string());
    }

    #[test]
    fn launching_saves_the_exe_when_the_record_has_none_or_it_is_gone() {
        assert_eq!(record_launch(json!({}), r"C:\GOG\ln2.exe", None, false)["executablePath"], r"C:\GOG\ln2.exe");
        let moved = json!({ "executablePath": r"C:\Old\ln2.exe" });
        assert_eq!(record_launch(moved, r"C:\GOG\ln2.exe", None, false)["executablePath"], r"C:\GOG\ln2.exe");
    }

    #[test]
    fn splits_like_string_argv() {
        assert_eq!(split_args(r#"-w  "two words" --x="a b" 'q'"#), ["-w", "two words", r#"--x="a b""#, "q"]);
        assert!(split_args("").is_empty());
    }

    #[test]
    fn resolves_launch_commands() {
        let cmd = |c: &str, a: &[&str], e: &[(&str, &str)]| LaunchCommand {
            command: c.into(),
            args: a.iter().map(|s| s.to_string()).collect(),
            env: e.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        };
        assert_eq!(resolve_launch_command("g.exe", None), cmd("g.exe", &[], &[]));
        assert_eq!(resolve_launch_command("g.exe", Some("-dx12 -w")), cmd("g.exe", &["-dx12", "-w"], &[]));
        assert_eq!(resolve_launch_command("g.exe", Some("A=1 B_2=x %command% -q")), cmd("g.exe", &["-q"], &[("A", "1"), ("B_2", "x")]));
        assert_eq!(resolve_launch_command("g.exe", Some("run.exe %command%")), cmd("run.exe", &["g.exe"], &[]));
        assert_eq!(resolve_launch_command("g.exe", Some("A=1 %command%")), cmd("g.exe", &[], &[("A", "1")]));
    }

    #[test]
    fn quotes_batch_commands() {
        assert!(is_batch_file("C:\\x\\Start.CMD"));
        assert_eq!(batch_command_line("C:\\a b\\s.bat", &["say \"hi\"".into()]), r#""C:\a b\s.bat" "say ""hi""""#);
    }
}
