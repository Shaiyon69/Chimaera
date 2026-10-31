//! Launcher import: `detectLauncherGames` and `importLauncherGames`
//! (src/main/services/launcher-import, src/main/events/library/launcher-games.ts).
// Detection rules ported from Playnite's official library plugins (MIT, https://github.com/JosefNemec/PlayniteExtensions).

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde_json::{json, Value};
use unicode_normalization::UnicodeNormalization;

use crate::games::{game_key, update};
use crate::store::Store;

#[derive(Clone, Debug, PartialEq)]
pub struct Detected {
    pub launcher: &'static str,
    pub id: String,
    pub title: String,
    pub install_dir: String,
    /// Candidate main exe; `resolve_exe` verifies it and falls back to a folder scan.
    pub exe_path: Option<String>,
    pub external_launch: Option<Value>,
}

#[derive(Clone, Debug, Default)]
pub struct Program {
    pub key_name: String,
    pub display_name: String,
    pub install_location: String,
    pub publisher: String,
    pub uninstall_string: String,
}

pub struct GogEntry {
    pub id: String,
    pub title: String,
    pub install_dir: String,
}

fn uri(value: String) -> Option<Value> {
    Some(json!({ "kind": "uri", "uri": value }))
}

fn exe(path: &str, args: Vec<String>) -> Option<Value> {
    Some(json!({ "kind": "exe", "path": path, "args": args }))
}

/// `path.win32.join` for the cases launchers produce: forward slashes become backslashes.
pub fn wjoin(base: &str, rel: &str) -> String {
    let rel = rel.replace('/', "\\");
    let base = base.replace('/', "\\");
    match (base.trim_end_matches('\\'), rel.trim_start_matches('\\')) {
        ("", rel) => rel.to_string(),
        (base, "") => base.to_string(),
        (base, rel) => format!("{base}\\{rel}"),
    }
}

fn basename(path: &str) -> String {
    path.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or_default().to_string()
}

fn dirname(path: &str) -> String {
    path.rfind(['\\', '/']).map_or(".".into(), |i| path[..i].to_string())
}

/// JS truthiness for an optional JSON string.
fn text<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value[field].as_str().filter(|s| !s.is_empty())
}

fn strings(value: &Value) -> Vec<&str> {
    value.as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default()
}

pub fn parse_epic(manifests: &[Value], installed: &[Value], dir_exists: &dyn Fn(&str) -> bool) -> Vec<Detected> {
    let mut games: Vec<Detected> = Vec::new();
    for m in manifests {
        let categories = strings(&m["AppCategories"]);
        let is_dlc = categories.contains(&"addons") && !categories.contains(&"addons/launchable");
        let is_engine_plugin = categories.iter().any(|c| *c == "plugins" || *c == "plugins/engine")
            || strings(&m["CompatibleApps"]).iter().any(|a| a.starts_with("UE_"))
            || m["TechnicalType"].as_str().is_some_and(|t| t.contains("plugins/engine"));
        let Some(app_name) = text(m, "AppName") else { continue };
        if is_dlc || is_engine_plugin {
            continue;
        }
        // Manifests can hold a stale location after a game is moved; LauncherInstalled.dat is then right.
        let app = installed.iter().find(|a| a["AppName"].as_str().is_some_and(|n| n.to_lowercase() == app_name.to_lowercase()));
        let app_dir = app.and_then(|a| text(a, "InstallLocation"));
        let Some(dir) = text(m, "InstallLocation").filter(|d| dir_exists(d)).or(app_dir.filter(|d| dir_exists(d))) else { continue };

        let game = Detected {
            launcher: "epic",
            id: app_name.to_string(),
            title: m["DisplayName"].as_str().map_or_else(|| basename(dir), str::to_string),
            install_dir: dir.to_string(),
            exe_path: text(m, "LaunchExecutable").map(|e| wjoin(dir, e)),
            external_launch: match (text(m, "CatalogNamespace"), text(m, "CatalogItemId")) {
                (Some(ns), Some(item)) => uri(format!("com.epicgames.launcher://apps/{ns}%3A{item}%3A{app_name}?action=launch&silent=true")),
                _ => None,
            },
        };
        // Several manifests can exist for one game; the one matching LauncherInstalled.dat wins.
        match games.iter().position(|g| g.id == game.id) {
            None => games.push(game),
            Some(i) if app_dir.is_some_and(|d| d.to_lowercase() == dir.to_lowercase()) => games[i] = game,
            Some(_) => {}
        }
    }
    games
}

/// GOG installs register as `<id>_is1` with publisher GOG.com.
pub fn parse_gog_programs(programs: &[Program], dir_exists: &dyn Fn(&str) -> bool) -> Vec<GogEntry> {
    programs
        .iter()
        .filter_map(|p| {
            let digits = p.key_name.bytes().take_while(u8::is_ascii_digit).count();
            let ok = digits > 0 && p.key_name[digits..].starts_with("_is1") && p.publisher == "GOG.com" && dir_exists(&p.install_location);
            ok.then(|| GogEntry { id: p.key_name[..digits].to_string(), title: p.display_name.clone(), install_dir: p.install_location.clone() })
        })
        .collect()
}

/// A game without a primary play task, or whose root game differs, is DLC.
pub fn parse_gog_info(entry: &GogEntry, info: Option<&Value>) -> Option<Detected> {
    let info = info?;
    let tasks = info["playTasks"].as_array()?;
    let task = tasks.iter().find(|t| t["isPrimary"].as_bool() == Some(true) && text(t, "path").is_some() && text(t, "type").is_none_or(|ty| ty == "FileTask"))?;
    if text(info, "rootGameId").is_some_and(|root| root != entry.id) {
        return None;
    }
    let file = wjoin(&entry.install_dir, text(task, "path")?);
    let args = crate::launch::split_args(task["arguments"].as_str().unwrap_or_default());
    Some(Detected {
        launcher: "gog",
        id: entry.id.clone(),
        title: entry.title.clone(),
        install_dir: entry.install_dir.clone(),
        external_launch: exe(&file, args),
        exe_path: Some(file),
    })
}

/// `installs` holds (install id, InstallDir) from `HKLM\SOFTWARE\ubisoft\Launcher\Installs`.
pub fn parse_ubisoft(installs: &[(String, String)], programs: &[Program], dir_exists: &dyn Fn(&str) -> bool) -> Vec<Detected> {
    installs
        .iter()
        .filter_map(|(name, install_dir)| {
            let dir = install_dir.replace('/', "\\");
            let dir = dir.strip_suffix('\\').unwrap_or(&dir);
            if dir.is_empty() || !dir_exists(dir) {
                return None;
            }
            let wanted = format!("Uplay Install {name}");
            let title = programs.iter().find(|p| p.key_name == wanted).map_or_else(|| basename(dir), |p| p.display_name.clone());
            Some(Detected { launcher: "ubisoft", id: name.clone(), title, install_dir: dir.to_string(), exe_path: None, external_launch: uri(format!("uplay://launch/{name}")) })
        })
        .collect()
}

/// `rows` holds (Id, ProductTitle, InstallDirectory); `fuel_command` is `Main.Command` from the game's fuel.json.
pub fn parse_amazon(rows: &[(String, String, String)], fuel_command: &dyn Fn(&str) -> Option<String>, dir_exists: &dyn Fn(&str) -> bool) -> Vec<Detected> {
    rows.iter()
        .filter(|(id, _, dir)| !id.is_empty() && dir_exists(dir))
        .map(|(id, title, dir)| Detected {
            launcher: "amazon",
            id: id.clone(),
            title: title.clone(),
            install_dir: dir.clone(),
            exe_path: fuel_command(dir).filter(|c| !c.is_empty()).map(|c| wjoin(dir, &c)),
            external_launch: uri(format!("amazon-games://play/{id}")),
        })
        .collect()
}

pub fn parse_humble(config: &Value, dir_exists: &dyn Fn(&str) -> bool) -> Vec<Detected> {
    let Some(collection) = config["GameCollection4"].as_array() else { return vec![] };
    collection
        .iter()
        .filter_map(|g| {
            let status = g["status"].as_str().unwrap_or_default();
            if status != "downloaded" && status != "installed" {
                return None;
            }
            let (machine, executable) = (g["machineName"].as_str().unwrap_or_default(), g["executablePath"].as_str().unwrap_or_default());
            // Older Humble App versions stored games under downloadFilePath\machineName.
            let file = match text(g, "filePath") {
                Some(dir) => wjoin(dir, executable),
                None => wjoin(&wjoin(g["downloadFilePath"].as_str().unwrap_or_default(), machine), executable),
            };
            let dir = dirname(&file);
            dir_exists(&dir).then(|| Detected {
                launcher: "humble",
                id: machine.to_string(),
                title: g["gameName"].as_str().unwrap_or_default().to_string(),
                install_dir: dir,
                external_launch: exe(&file, vec![]),
                exe_path: Some(file),
            })
        })
        .collect()
}

fn tables() -> &'static Value {
    static TABLES: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    TABLES.get_or_init(|| serde_json::from_str(include_str!("launcher_tables.json")).expect("launcher_tables.json is valid JSON"))
}

/// `/Battle\.net.*--uid=(.*?)\s/`: the last `--uid=` after "Battle.net" whose value ends in whitespace.
fn battlenet_uid(uninstall: &str) -> Option<&str> {
    let after = &uninstall[uninstall.find("Battle.net")? + "Battle.net".len()..];
    after.rmatch_indices("--uid=").find_map(|(i, m)| {
        let rest = &after[i + m.len()..];
        rest.find(char::is_whitespace).map(|end| &rest[..end])
    })
}

pub fn parse_battlenet(programs: &[Program], dir_exists: &dyn Fn(&str) -> bool, file_exists: &dyn Fn(&str) -> bool, client_exe: Option<&str>) -> Vec<Detected> {
    let products = tables()["battlenet"].as_array().map(Vec::as_slice).unwrap_or_default();
    let mut games: Vec<Detected> = Vec::new();
    let mut add = |g: Detected| {
        if !games.iter().any(|e| e.id == g.id) {
            games.push(g);
        }
    };
    for p in programs {
        if p.install_location.is_empty() || !dir_exists(&p.install_location) {
            continue;
        }
        if let Some(uid) = battlenet_uid(&p.uninstall_string) {
            if p.display_name.ends_with("Test") || p.display_name.ends_with("Beta") {
                continue;
            }
            let uid = uid.to_lowercase();
            let Some(product) = products.iter().find(|g| g["type"] == "default" && g["internalId"].as_str().is_some_and(|id| uid.starts_with(&id.to_lowercase()))) else { continue };
            let id = product["productId"].as_str().unwrap_or_default();
            add(Detected {
                launcher: "battlenet",
                id: id.to_string(),
                title: product["name"].as_str().unwrap_or_default().to_string(),
                install_dir: p.install_location.clone(),
                exe_path: None,
                external_launch: client_exe.and_then(|client| exe(client, vec![format!("--exec=launch {id}")])),
            });
        } else if p.publisher == "Blizzard Entertainment" {
            for product in products.iter().filter(|g| g["type"] == "classic" && g["internalId"] == p.display_name.as_str()) {
                let Some(executable) = text(product, "executable") else { continue };
                let file = wjoin(&p.install_location, executable);
                if !file_exists(&file) {
                    continue;
                }
                add(Detected {
                    launcher: "battlenet",
                    id: product["productId"].as_str().unwrap_or_default().to_string(),
                    title: product["name"].as_str().unwrap_or_default().to_string(),
                    install_dir: p.install_location.clone(),
                    external_launch: exe(&file, vec![]),
                    exe_path: Some(file),
                });
            }
        }
    }
    games
}

/// `/(?:Launcher|uninstall)\.exe.+uninstall=(.+)$/i`.
fn rockstar_title_id(uninstall: &str) -> Option<&str> {
    let lower = uninstall.to_ascii_lowercase();
    let marker = ["launcher.exe", "uninstall.exe"].iter().filter_map(|m| lower.find(m).map(|i| i + m.len())).min()?;
    let (i, m) = lower.rmatch_indices("uninstall=").find(|(i, m)| *i > marker && i + m.len() < lower.len())?;
    Some(&uninstall[i + m.len()..])
}

pub fn parse_rockstar(programs: &[Program], dir_exists: &dyn Fn(&str) -> bool, client_exe: Option<&str>) -> Vec<Detected> {
    let products = tables()["rockstar"].as_array().map(Vec::as_slice).unwrap_or_default();
    programs
        .iter()
        .filter_map(|p| {
            let title_id = rockstar_title_id(&p.uninstall_string)?;
            let product = products.iter().find(|g| g["titleId"] == title_id)?;
            if p.install_location.is_empty() || !dir_exists(&p.install_location) {
                return None;
            }
            let dir = &p.install_location;
            Some(Detected {
                launcher: "rockstar",
                id: title_id.to_string(),
                title: product["name"].as_str().unwrap_or_default().to_string(),
                install_dir: dir.clone(),
                exe_path: Some(wjoin(dir, product["executable"].as_str().unwrap_or_default())),
                external_launch: client_exe.and_then(|client| exe(client, vec!["-launchTitleInFolder".into(), dir.clone()])),
            })
        })
        .collect()
}

/// Value of `name="..."` in `xml`, case-insensitive, with `name` starting at a word boundary.
fn xml_attr<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let lower = xml.to_ascii_lowercase();
    let needle = format!("{}=\"", name.to_ascii_lowercase());
    let start = lower.match_indices(&needle).map(|(i, _)| i).find(|&i| i == 0 || !lower.as_bytes()[i - 1].is_ascii_alphanumeric() && lower.as_bytes()[i - 1] != b'_')? + needle.len();
    xml[start..].find('"').map(|end| &xml[start..start + end])
}

/// Store packages are games only when they ship a MicrosoftGame.config (offline stand-in for Playnite's online check).
pub fn parse_xbox(name: &str, family: &str, install_location: &str, manifest_xml: &str, game_config_xml: Option<&str>) -> Option<Detected> {
    let config = game_config_xml?;
    let lower = manifest_xml.to_ascii_lowercase();
    let start = lower.match_indices("<application").map(|(i, _)| i).find(|&i| lower.as_bytes().get(i + 12).is_some_and(|b| !b.is_ascii_alphanumeric() && *b != b'_'))?;
    let app = &manifest_xml[start..start + manifest_xml[start..].find('>')? + 1];
    let app_id = xml_attr(app, "Id")?;
    let display = xml_attr(config, "DefaultDisplayName");
    Some(Detected {
        launcher: "xbox",
        id: family.to_string(),
        // ms-resource: values need the app's resource files; fall back to the package name.
        title: display.filter(|n| !n.is_empty() && !n.starts_with("ms-resource:")).unwrap_or(name).to_string(),
        install_dir: install_location.to_string(),
        exe_path: xml_attr(app, "Executable").map(|e| wjoin(install_location, e)),
        external_launch: exe("explorer.exe", vec![format!("shell:AppsFolder\\{family}!{app_id}")]),
    })
}

const SKIP_EXES: [&str; 10] = ["unins", "crash", "launcher", "redist", "unitycrash", "vc_redist", "dxsetup", "eac", "easyanticheat", "benchmark"];

/// `findMainExe`: largest non-launcher exe up to 5 levels deep; prefers Unreal `*-Win64-Shipping.exe`.
pub fn find_main_exe(game_dir: &Path) -> Option<String> {
    fn walk(dir: &Path, depth: usize, best: &mut Option<(u64, String)>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let (Ok(kind), name) = (entry.file_type(), entry.file_name().to_string_lossy().to_lowercase()) else { continue };
            if kind.is_file() && name.ends_with(".exe") && !SKIP_EXES.iter().any(|s| name.contains(s)) {
                if let Ok(meta) = entry.metadata() {
                    let size = meta.len() + if name.ends_with("-win64-shipping.exe") { 1 << 40 } else { 0 };
                    if best.as_ref().is_none_or(|(b, _)| size > *b) {
                        *best = Some((size, entry.path().to_string_lossy().into_owned()));
                    }
                }
            }
            if kind.is_dir() && !name.starts_with('.') && depth + 1 < 5 {
                walk(&entry.path(), depth + 1, best);
            }
        }
    }
    let mut best = None;
    walk(game_dir, 0, &mut best);
    best.map(|(_, path)| path)
}

/// Keeps a verified exe, else scans the install folder for the most likely one.
fn resolve_exe(mut game: Detected) -> Detected {
    // WindowsApps is unreadable, so Store exes cannot be verified.
    let keep = game.exe_path.as_deref().is_some_and(|e| game.launcher == "xbox" || Path::new(e).is_file());
    if !keep {
        game.exe_path = find_main_exe(Path::new(&game.install_dir));
    }
    game
}

/// Stable id so a second import finds the existing record instead of duplicating it.
pub fn launcher_object_id(launcher: &str, id: &str) -> String {
    // Per UTF-16 unit, as the JS regex replaced, so ids match records Electron imported.
    format!("{launcher}-{id}").chars().flat_map(|c| if c.is_ascii_alphanumeric() || "_.-".contains(c) { vec![c] } else { vec!['_'; c.len_utf16()] }).collect()
}

/// Lowercase letters and digits only, so "The Witcher® 3: Wild Hunt" equals "the witcher 3 wild hunt".
pub fn normalize_title(title: &str) -> String {
    title.nfkd().filter(|c| !"™®©".contains(*c)).flat_map(char::to_lowercase).filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit()).collect()
}

/// Exact normalized title match only: a wrong cover is worse than the exe icon fallback.
pub fn pick_steam_match(items: &[Value], title: &str) -> Option<u64> {
    let wanted = normalize_title(title);
    if wanted.is_empty() {
        return None;
    }
    items.iter().find(|i| i["type"].as_str().unwrap_or("app") == "app" && i["name"].as_str().is_some_and(|n| normalize_title(n) == wanted))?["id"].as_u64()
}

const STEAM_ASSETS: &str = "https://shared.steamstatic.com/store_item_assets/steam/apps";

/// (iconUrl, logoImageUrl, libraryHeroImageUrl).
pub fn steam_artwork(app_id: u64) -> [Option<Value>; 3] {
    [
        Some(json!(format!("{STEAM_ASSETS}/{app_id}/library_600x900_2x.jpg"))),
        Some(json!(format!("{STEAM_ASSETS}/{app_id}/logo.png"))),
        Some(json!(format!("{STEAM_ASSETS}/{app_id}/library_hero.jpg"))),
    ]
}

async fn find_steam_artwork(title: &str) -> Result<Option<u64>, String> {
    let url = url::Url::parse_with_params("https://store.steampowered.com/api/storesearch/", &[("term", title), ("l", "english"), ("cc", "us")]).map_err(|e| e.to_string())?;
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(8)).build().map_err(|e| e.to_string())?;
    let data: Value = client.get(url).send().await.map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    Ok(pick_steam_match(data["items"].as_array().map(Vec::as_slice).unwrap_or_default(), title))
}

/// `fetchGameArtwork`: Steam store art when the title matches exactly, else the exe's own icon. Never fails.
async fn fetch_artwork(title: &str, exe_path: Option<&str>) -> [Option<Value>; 3] {
    match find_steam_artwork(title).await {
        Ok(Some(id)) => return steam_artwork(id),
        Ok(None) => {}
        Err(error) => eprintln!("[launcher-import] Steam artwork lookup failed for {title}: {error}"),
    }
    let icon = match exe_path {
        Some(exe) => {
            let exe = exe.to_string();
            tauri::async_runtime::spawn_blocking(move || windows::exe_icon(&exe)).await.ok().flatten()
        }
        None => None,
    };
    [icon.map(Value::String), None, None]
}

fn is_in_library(store: &Store, object_id: &str) -> Result<bool, String> {
    let existing = store.get("games", &game_key("custom", object_id))?;
    Ok(!existing.is_null() && existing["isDeleted"].as_bool() != Some(true))
}

/// Installed games from every supported launcher. Empty off Windows.
pub fn detect() -> Vec<Detected> {
    windows::detect().into_iter().map(resolve_exe).collect()
}

/// `detectLauncherGames`.
pub fn detect_games(store: &Store) -> Result<Value, String> {
    let mut list = Vec::new();
    for g in detect() {
        let object_id = launcher_object_id(g.launcher, &g.id);
        list.push(json!({
            "launcher": g.launcher,
            "id": g.id,
            "objectId": object_id,
            "title": g.title,
            "installDir": g.install_dir,
            "exePath": g.exe_path,
            "externalLaunch": g.external_launch,
            "imported": is_in_library(store, &object_id)?,
        }));
    }
    Ok(Value::Array(list))
}

/// `importLauncherGames`. Re-detects instead of trusting the renderer, so paths and URIs always come from this machine.
pub async fn import_games(app: &tauri::AppHandle, selection: &Value) -> Result<Value, String> {
    use tauri::Manager;
    let wanted: HashSet<String> = selection.as_array().into_iter().flatten().map(|s| format!("{}:{}", s["launcher"].as_str().unwrap_or_default(), s["id"].as_str().unwrap_or_default())).collect();
    let games: Vec<Detected> = tauri::async_runtime::spawn_blocking(detect).await.map_err(|e| e.to_string())?.into_iter().filter(|g| wanted.contains(&format!("{}:{}", g.launcher, g.id))).collect();

    let mut imported = 0;
    for game in games {
        let object_id = launcher_object_id(game.launcher, &game.id);
        if is_in_library(&app.state::<Store>(), &object_id)? {
            continue;
        }
        let [icon, logo, hero] = fetch_artwork(&game.title, game.exe_path.as_deref()).await;
        let store = app.state::<Store>();
        crate::artwork::create_custom_game(&store, &object_id, &game.title, game.exe_path.as_deref(), icon.as_ref(), logo.as_ref(), hero.as_ref())?;
        update(&store, &game_key("custom", &object_id), |record| {
            record.insert("launcher".into(), json!(game.launcher));
            record.insert("launcherGameId".into(), json!(game.id));
            record.insert("externalLaunch".into(), game.external_launch.clone().unwrap_or(Value::Null));
        })?;
        imported += 1;
    }
    if imported > 0 {
        // Same Steam twins the startup backfill makes, without waiting for a restart.
        crate::games::backfill_steam_records(&app.state::<Store>())?;
    }
    Ok(json!({ "imported": imported }))
}

#[cfg(not(windows))]
mod windows {
    use super::Detected;

    pub fn detect() -> Vec<Detected> {
        vec![]
    }

    pub fn exe_icon(_exe: &str) -> Option<String> {
        None
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::os::windows::process::CommandExt;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};
    use winreg::RegKey;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";

    fn dir_exists(dir: &str) -> bool {
        Path::new(dir).is_dir()
    }

    fn file_exists(file: &str) -> bool {
        Path::new(file).is_file()
    }

    fn read_json(path: &Path) -> Option<Value> {
        let text = fs::read_to_string(path).ok()?;
        // Epic and Humble write a UTF-8 BOM.
        serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
    }

    fn env_dir(name: &str) -> String {
        std::env::var(name).unwrap_or_default()
    }

    fn reg_str(key: &RegKey, name: &str) -> String {
        key.get_value::<String, _>(name).map(|v| v.trim().to_string()).unwrap_or_default()
    }

    /// Direct sub-keys of `key` with their opened handles.
    fn sub_keys(hive: winreg::HKEY, key: &str, view: u32) -> Vec<(String, RegKey)> {
        let Ok(root) = RegKey::predef(hive).open_subkey_with_flags(key, KEY_READ | view) else { return vec![] };
        root.enum_keys().flatten().filter_map(|name| root.open_subkey_with_flags(&name, KEY_READ | view).ok().map(|k| (name, k))).collect()
    }

    /// Installed-programs list, as Windows shows it: HKLM (both views) plus HKCU.
    fn uninstall_programs() -> Vec<Program> {
        let mut seen = HashSet::new();
        [(HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY), (HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY), (HKEY_CURRENT_USER, KEY_WOW64_64KEY)]
            .into_iter()
            .flat_map(|(hive, view)| sub_keys(hive, UNINSTALL, view))
            .filter_map(|(name, key)| {
                let display_name = reg_str(&key, "DisplayName");
                (!display_name.is_empty()).then(|| Program {
                    key_name: name,
                    display_name,
                    install_location: reg_str(&key, "InstallLocation").trim_matches('"').to_string(),
                    publisher: reg_str(&key, "Publisher"),
                    uninstall_string: reg_str(&key, "UninstallString"),
                })
            })
            .filter(|p| seen.insert(format!("{}|{}", p.key_name, p.install_location).to_lowercase()))
            .collect()
    }

    fn epic() -> Result<Vec<Detected>, String> {
        let data = Path::new(&std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into())).join("Epic");
        let Ok(entries) = fs::read_dir(data.join(r"EpicGamesLauncher\Data\Manifests")) else { return Ok(vec![]) };
        // Epic sometimes leaves empty or hand-broken manifests behind.
        let manifests: Vec<Value> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "item")).filter_map(|p| read_json(&p)).collect();
        let installed = read_json(&data.join(r"UnrealEngineLauncher\LauncherInstalled.dat")).and_then(|v| v["InstallationList"].as_array().cloned()).unwrap_or_default();
        Ok(parse_epic(&manifests, &installed, &dir_exists))
    }

    fn gog(programs: &[Program]) -> Result<Vec<Detected>, String> {
        Ok(parse_gog_programs(programs, &dir_exists)
            .iter()
            .filter_map(|entry| parse_gog_info(entry, read_json(&Path::new(&entry.install_dir).join(format!("goggame-{}.info", entry.id))).as_ref()))
            .collect())
    }

    fn ubisoft(programs: &[Program]) -> Result<Vec<Detected>, String> {
        let installs = |view| -> Vec<(String, String)> {
            sub_keys(HKEY_LOCAL_MACHINE, r"SOFTWARE\ubisoft\Launcher\Installs", view).into_iter().map(|(name, key)| (name, reg_str(&key, "InstallDir"))).filter(|(_, dir)| !dir.is_empty()).collect()
        };
        // Playnite checks the 32-bit view first, then 64-bit.
        let mut list = installs(KEY_WOW64_32KEY);
        if list.is_empty() {
            list = installs(KEY_WOW64_64KEY);
        }
        Ok(parse_ubisoft(&list, programs, &dir_exists))
    }

    fn amazon() -> Result<Vec<Detected>, String> {
        let db = Path::new(&env_dir("LOCALAPPDATA")).join(r"Amazon Games\Data\Games\Sql\GameInstallInfo.sqlite");
        if !db.is_file() {
            return Ok(vec![]);
        }
        let conn = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
        let mut statement = conn.prepare("SELECT Id, ProductTitle, InstallDirectory FROM DbSet WHERE Installed = 1").map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |r| Ok((r.get::<_, Option<String>>(0)?.unwrap_or_default(), r.get::<_, Option<String>>(1)?.unwrap_or_default(), r.get::<_, Option<String>>(2)?.unwrap_or_default())))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let fuel = |dir: &str| read_json(&Path::new(dir).join("fuel.json")).and_then(|f| f["Main"]["Command"].as_str().map(str::to_string));
        Ok(parse_amazon(&rows, &fuel, &dir_exists))
    }

    fn humble() -> Result<Vec<Detected>, String> {
        Ok(read_json(&Path::new(&env_dir("APPDATA")).join(r"Humble App\config.json")).map(|c| parse_humble(&c, &dir_exists)).unwrap_or_default())
    }

    fn client(programs: &[Program], matches: impl Fn(&Program) -> bool, exe: &str) -> Option<String> {
        programs.iter().find(|p| matches(p)).map(|p| wjoin(&p.install_location, exe)).filter(|f| file_exists(f))
    }

    fn battlenet(programs: &[Program]) -> Result<Vec<Detected>, String> {
        let client_exe = client(programs, |p| p.uninstall_string.contains("--uid=battle.net"), "Battle.net.exe");
        Ok(parse_battlenet(programs, &dir_exists, &file_exists, client_exe.as_deref()))
    }

    fn rockstar(programs: &[Program]) -> Result<Vec<Detected>, String> {
        let client_exe = client(programs, |p| p.display_name == "Rockstar Games Launcher", "Launcher.exe");
        Ok(parse_rockstar(programs, &dir_exists, client_exe.as_deref()))
    }

    // shortcut: no timeout on PowerShell (Electron gave it 30 s); add one if Get-AppxPackage ever hangs.
    fn powershell(script: &str, envs: &[(&str, &str)]) -> Result<String, String> {
        let out = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &format!("[Console]::OutputEncoding=[Text.Encoding]::UTF8; {script}")])
            .envs(envs.iter().copied())
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).into_owned());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn xbox() -> Result<Vec<Detected>, String> {
        let out = powershell(
            "Get-AppxPackage | Where-Object { $_.SignatureKind -eq 'Store' -and -not $_.IsFramework } | Select-Object Name,PackageFamilyName,InstallLocation | ConvertTo-Json -Compress",
            &[],
        )?;
        if out.trim().is_empty() {
            return Ok(vec![]);
        }
        let parsed: Value = serde_json::from_str(out.trim()).map_err(|e| e.to_string())?;
        let packages = if parsed.is_array() { parsed.as_array().cloned().unwrap_or_default() } else { vec![parsed] };
        let read = |path: std::path::PathBuf| fs::read(path).ok().map(|b| String::from_utf8_lossy(&b).into_owned());
        Ok(packages
            .iter()
            .filter_map(|pkg| {
                let location = text(pkg, "InstallLocation")?;
                let manifest = read(Path::new(location).join("AppxManifest.xml"))?;
                let config = read(Path::new(location).join("MicrosoftGame.config"));
                parse_xbox(pkg["Name"].as_str().unwrap_or_default(), pkg["PackageFamilyName"].as_str().unwrap_or_default(), location, &manifest, config.as_deref())
            })
            .collect())
    }

    /// The exe's 32 px icon as a PNG data URL, like Electron's `app.getFileIcon(..., "large")`.
    pub fn exe_icon(exe: &str) -> Option<String> {
        // The path goes in through the environment so it is never parsed as PowerShell.
        let script = "Add-Type -AssemblyName System.Drawing; $i=[System.Drawing.Icon]::ExtractAssociatedIcon($env:CHIMAERA_EXE); $m=New-Object IO.MemoryStream; $i.ToBitmap().Save($m,[System.Drawing.Imaging.ImageFormat]::Png); [Convert]::ToBase64String($m.ToArray())";
        let out = powershell(script, &[("CHIMAERA_EXE", exe)]).map_err(|e| eprintln!("[launcher-import] exe icon lookup failed for {exe}: {e}")).ok()?;
        let b64 = out.trim();
        (!b64.is_empty()).then(|| format!("data:image/png;base64,{b64}"))
    }

    /// A launcher that fails to read is logged and skipped.
    pub fn detect() -> Vec<Detected> {
        let programs = uninstall_programs();
        let detectors: [(&str, &dyn Fn() -> Result<Vec<Detected>, String>); 8] = [
            ("epic", &epic),
            ("gog", &|| gog(&programs)),
            ("ubisoft", &|| ubisoft(&programs)),
            ("amazon", &amazon),
            ("humble", &humble),
            ("battlenet", &|| battlenet(&programs)),
            ("rockstar", &|| rockstar(&programs)),
            ("xbox", &xbox),
        ];
        detectors
            .iter()
            .flat_map(|(name, run)| {
                run().unwrap_or_else(|error| {
                    eprintln!("[launcher-import] {name} detection failed: {error}");
                    vec![]
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yes(_: &str) -> bool {
        true
    }

    fn program(key_name: &str, display_name: &str, install_location: &str, publisher: &str, uninstall_string: &str) -> Program {
        Program { key_name: key_name.into(), display_name: display_name.into(), install_location: install_location.into(), publisher: publisher.into(), uninstall_string: uninstall_string.into() }
    }

    #[test]
    fn epic_skips_dlc_and_engine_plugins_and_prefers_launcher_installed() {
        let manifests = [
            json!({ "AppName": "Fortnite", "DisplayName": "Game One", "InstallLocation": r"C:\stale", "LaunchExecutable": r"Bin\game.exe", "CatalogNamespace": "ns", "CatalogItemId": "item" }),
            json!({ "AppName": "Dlc", "InstallLocation": r"C:\d", "AppCategories": ["addons"] }),
            json!({ "AppName": "Plug", "InstallLocation": r"C:\p", "AppCategories": ["plugins"] }),
        ];
        let games = parse_epic(&manifests, &[json!({ "AppName": "fortnite", "InstallLocation": r"C:\Epic\One" })], &|d| d != r"C:\stale");
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].install_dir, r"C:\Epic\One");
        assert_eq!(games[0].exe_path.as_deref(), Some(r"C:\Epic\One\Bin\game.exe"));
        assert_eq!(games[0].external_launch, uri("com.epicgames.launcher://apps/ns%3Aitem%3AFortnite?action=launch&silent=true".into()));
    }

    #[test]
    fn gog_filters_programs_and_rejects_dlc() {
        let entries = parse_gog_programs(
            &[program("123_is1", "G", r"C:\GOG\G", "GOG.com", ""), program("456_is1", "n", r"C:\x", "Other", ""), program("GOGPACK123_is1", "n", r"C:\x", "GOG.com", "")],
            &yes,
        );
        assert_eq!(entries.len(), 1);
        let game = parse_gog_info(&entries[0], Some(&json!({ "rootGameId": "123", "playTasks": [{ "isPrimary": true, "type": "FileTask", "path": "g.exe", "arguments": "-a \"b c\"" }] }))).unwrap();
        assert_eq!(game.exe_path.as_deref(), Some(r"C:\GOG\G\g.exe"));
        assert_eq!(game.external_launch, exe(r"C:\GOG\G\g.exe", vec!["-a".into(), "b c".into()]));
        assert_eq!(parse_gog_info(&entries[0], Some(&json!({ "rootGameId": "999", "playTasks": [] }))), None);
        assert_eq!(parse_gog_info(&entries[0], Some(&json!({ "rootGameId": "999", "playTasks": [{ "isPrimary": true, "path": "x.exe" }] }))), None);
    }

    #[test]
    fn ubisoft_titles_come_from_the_uninstall_entry_else_the_folder() {
        let games = parse_ubisoft(
            &[("720".into(), "C:/Ubi/Game One/".into()), ("99".into(), "C:/Ubi/Other".into())],
            &[program("Uplay Install 720", "Game One Deluxe", r"C:\x", "", "")],
            &yes,
        );
        let got: Vec<_> = games.iter().map(|g| (g.id.as_str(), g.title.as_str(), g.install_dir.as_str())).collect();
        assert_eq!(got, [("720", "Game One Deluxe", r"C:\Ubi\Game One"), ("99", "Other", r"C:\Ubi\Other")]);
        assert_eq!(games[0].external_launch, uri("uplay://launch/720".into()));
    }

    #[test]
    fn amazon_uses_the_uri_and_the_fuel_exe() {
        let games = parse_amazon(&[("amzn1".into(), "A".into(), r"C:\Amazon\A".into())], &|_| Some("a.exe".into()), &yes);
        assert_eq!(games[0].exe_path.as_deref(), Some(r"C:\Amazon\A\a.exe"));
        assert_eq!(games[0].external_launch, uri("amazon-games://play/amzn1".into()));
    }

    #[test]
    fn humble_keeps_installed_entries_and_the_legacy_layout() {
        let config = json!({ "GameCollection4": [
            { "machineName": "a", "gameName": "A", "status": "installed", "filePath": r"C:\H\a", "executablePath": "a.exe" },
            { "machineName": "b", "gameName": "B", "status": "downloaded", "downloadFilePath": r"C:\H", "executablePath": "b.exe" },
            { "machineName": "c", "gameName": "C", "status": "available", "executablePath": "c.exe" },
        ]});
        let exes: Vec<_> = parse_humble(&config, &yes).into_iter().map(|g| g.exe_path.unwrap()).collect();
        assert_eq!(exes, [r"C:\H\a\a.exe", r"C:\H\b\b.exe"]);
    }

    #[test]
    fn battlenet_maps_uids_and_launches_through_the_client() {
        let games = parse_battlenet(
            &[
                program("k", "World of Warcraft", r"C:\Blizzard\WoW", "", r#""C:\Program Files\Battle.net\Battle.net-Setup.exe" --uid=wow_classic --lang=enUS "#),
                program("k", "Unknown", r"C:\Games\x", "", "Battle.net --uid=nothing_known "),
            ],
            &yes,
            &yes,
            Some(r"C:\Battle.net\Battle.net.exe"),
        );
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].id, "WoW");
        assert_eq!(games[0].external_launch, exe(r"C:\Battle.net\Battle.net.exe", vec!["--exec=launch WoW".into()]));
    }

    #[test]
    fn rockstar_reads_the_title_id_from_the_uninstall_string() {
        let games = parse_rockstar(&[program("k", "n", r"C:\Rockstar\GTAV", "", r#""C:\Rockstar\GTAV\uninstall.exe" -uninstall=gta5"#)], &yes, Some(r"C:\RS\Launcher.exe"));
        assert_eq!(games[0].title, "Grand Theft Auto V");
        assert_eq!(games[0].exe_path.as_deref(), Some(r"C:\Rockstar\GTAV\PlayGTAV.exe"));
        assert_eq!(games[0].external_launch, exe(r"C:\RS\Launcher.exe", vec!["-launchTitleInFolder".into(), r"C:\Rockstar\GTAV".into()]));
    }

    #[test]
    fn xbox_needs_a_game_config() {
        let manifest = r#"<Package><Applications><Application Id="Game" Executable="game.exe" /></Applications></Package>"#;
        let location = r"C:\XboxGames\Game\Content";
        assert_eq!(parse_xbox("Contoso.Game", "Contoso.Game_abc", location, manifest, None), None);
        let game = parse_xbox("Contoso.Game", "Contoso.Game_abc", location, manifest, Some(r#"<Game><ShellVisuals DefaultDisplayName="ms-resource:Title" /></Game>"#)).unwrap();
        assert_eq!(game.title, "Contoso.Game");
        assert_eq!(game.exe_path.as_deref(), Some(r"C:\XboxGames\Game\Content\game.exe"));
        assert_eq!(game.external_launch, exe("explorer.exe", vec![r"shell:AppsFolder\Contoso.Game_abc!Game".into()]));
    }

    #[test]
    fn steam_match_needs_an_exact_normalized_app_title() {
        let items = [json!({ "type": "bundle", "id": 1, "name": "Portal" }), json!({ "type": "app", "id": 2, "name": "Portal 2" }), json!({ "type": "app", "id": 3, "name": "The Witcher® 3: Wild Hunt" })];
        assert_eq!(pick_steam_match(&items, "Portal"), None);
        assert_eq!(pick_steam_match(&items, "portal 2"), Some(2));
        assert_eq!(pick_steam_match(&items, "The Witcher 3 - Wild Hunt"), Some(3));
        assert_eq!(pick_steam_match(&items, "   "), None);
        assert_eq!(normalize_title("Pokémon"), "pokemon");
        assert!(steam_artwork(2)[0].as_ref().unwrap().as_str().unwrap().ends_with("/apps/2/library_600x900_2x.jpg"));
    }

    #[test]
    fn object_ids_match_electron() {
        assert_eq!(launcher_object_id("epic", "Fortnite:x y"), "epic-Fortnite_x_y");
        assert_eq!(launcher_object_id("xbox", "a😀b"), "xbox-a__b");
    }

    #[test]
    fn main_exe_skips_launchers_and_prefers_shipping() {
        let dir = std::env::temp_dir().join(format!("chimaera-main-exe-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(r"Game\Binaries\Win64")).unwrap();
        fs::write(dir.join("Launcher.exe"), vec![0; 4096]).unwrap();
        fs::write(dir.join("big.exe"), vec![0; 2048]).unwrap();
        assert_eq!(find_main_exe(&dir), Some(dir.join("big.exe").to_string_lossy().into_owned()));
        let shipping = dir.join(r"Game\Binaries\Win64\Game-Win64-Shipping.exe");
        fs::write(&shipping, [0; 16]).unwrap();
        assert_eq!(find_main_exe(&dir), Some(shipping.to_string_lossy().into_owned()));
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Reads this machine's launchers: `cargo test detects_this_machine -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn detects_this_machine() {
        for game in detect() {
            println!("{} {} {:?} {:?} {:?}", game.launcher, game.title, game.install_dir, game.exe_path, game.external_launch);
        }
    }
}
