//! Classics (LaunchBox) launch and disc selection. Port of
//! `helpers/open-classics-game.ts`, `launch-classics-game.ts`,
//! `launch-retroarch-game.ts`, `events/library/update-classics-disc.ts`,
//! `services/emulators/bios-detection.ts` and `events/emulators/check-emulator-bios.ts`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::classics_import::{ini_candidates, platform_to_system, rpcs3_config_roots};
use crate::emulators;
use crate::games::game_key;
use crate::library::iso_now;
use crate::rom_scan;
use crate::store::Store;

/// Emulators started for classics games, by store key; the watcher credits playtime while the PID lives.
#[derive(Default)]
pub struct EmulatorPids(Mutex<HashMap<String, u32>>);

impl EmulatorPids {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, u32>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn track(&self, key: &str, pid: u32) {
        self.lock().insert(key.to_string(), pid);
    }

    /// Keys whose emulator still runs; dead entries are dropped.
    pub fn alive(&self, running: &std::collections::HashSet<u32>) -> Vec<String> {
        let mut all = self.lock();
        all.retain(|_, pid| running.contains(pid));
        all.keys().cloned().collect()
    }
}

/// getCloudSaveRetroArchPlatform's platform half: the RetroArch platform a catalogue name maps to.
pub fn retroarch_platform(platform: &str) -> Option<&'static str> {
    static RULES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
        [
            (r"game\s*boy\s*advance|\bgba\b", "gba"),
            (r"game\s*boy\s*color|\bgbc\b", "gbc"),
            (r"game\s*boy|\bgb\b", "gb"),
            (r"nintendo\s*64|\bn64\b", "n64"),
            (r"super\s*nintendo|\bsnes\b", "snes"),
            (r"nintendo\s*entertainment\s*system|\bnes\b|\bfamicom\b", "nes"),
        ]
        .into_iter()
        .map(|(re, p)| (Regex::new(re).unwrap(), p))
        .collect()
    });
    let p = platform.to_lowercase();
    RULES.iter().find(|(re, _)| re.is_match(&p)).map(|(_, platform)| *platform)
}

fn is_ps3_platform(platform: &str) -> bool {
    static PS3: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)playstation\s*3|\bps3\b").unwrap());
    PS3.is_match(platform)
}

/// updateClassicsDisc: add/remove a disc, pick one, or change the platform.
pub fn update_disc(store: &Store, shop: &str, object_id: &str, patch: &Value) -> Result<Value, String> {
    let key = game_key(shop, object_id);
    let mut game = store.get("games", &key)?;
    if !game.is_object() {
        return Err(format!("Game not found: {key}"));
    }
    let mut discs = game["discs"].as_array().cloned().unwrap_or_default();
    if let Some(add) = patch.get("addDisc").filter(|d| d.is_object()) {
        let path = add["path"].as_str().unwrap_or_default();
        if !discs.iter().any(|d| d["path"].as_str() == Some(path)) {
            let mut add = add.clone();
            if add["sku"].as_str().is_none_or(str::is_empty) {
                let hint = patch.get("platform").filter(|p| !p.is_null()).unwrap_or(&game["platform"]);
                if let Some(system) = hint.as_str().and_then(platform_to_system) {
                    add["sku"] = json!(rom_scan::extract_disc_sku(Path::new(path), system));
                }
            }
            discs.push(add);
        }
    }
    if let Some(remove) = patch.get("removeDiscPath").and_then(Value::as_str) {
        if let Some(i) = discs.iter().position(|d| d["path"].as_str() == Some(remove)) {
            discs.remove(i);
        }
    }
    for field in ["selectedDiscPath", "dontAskDiscSelection", "platform"] {
        if let Some(value) = patch.get(field) {
            game[field] = value.clone();
        }
    }
    let selected = game["selectedDiscPath"].as_str().filter(|s| !s.is_empty());
    if selected.is_some_and(|s| !discs.iter().any(|d| d["path"].as_str() == Some(s))) {
        let platform = game["platform"].as_str().unwrap_or_default();
        // RetroArch games may run with no ROM picked; disc games fall back to the first disc.
        let retroarch = shop == "launchbox" && !platform.is_empty() && !is_ps3_platform(platform) && retroarch_platform(platform).is_some();
        game["selectedDiscPath"] = if retroarch { Value::Null } else { discs.first().map(|d| d["path"].clone()).unwrap_or(Value::Null) };
    }
    game["discs"] = Value::Array(discs);
    store.put("games", &key, &game)?;
    Ok(game)
}

// ---- BIOS -------------------------------------------------------------------

static SECTION_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\[([^\]]+)\]\s*$").unwrap());
static DUCKSTATION_DIR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\s*SearchDirectory\s*=(.+)$").unwrap());
static PCSX2_DIR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\s*Bios\s*=(.+)$").unwrap());

/// The BIOS folder override in an emulator ini, made absolute against `root`.
fn ini_bios_dir(ini: &Path, section: &str, key: &Regex, root: &Path) -> Option<PathBuf> {
    let content = std::fs::read_to_string(ini).ok()?;
    let mut in_section = false;
    for line in content.lines() {
        if let Some(sec) = SECTION_RE.captures(line) {
            in_section = sec[1].eq_ignore_ascii_case(section);
            continue;
        }
        if let Some(m) = key.captures(line).filter(|_| in_section) {
            let value = m[1].trim();
            if value.is_empty() {
                return None;
            }
            let value = PathBuf::from(value);
            return Some(if value.is_absolute() { value } else { root.join(value) });
        }
    }
    None
}

/// resolvePs1BiosDirs / resolvePs2BiosDirs: manual path, ini override, default, then portable.
fn bios_dirs(system: &str, executable: Option<&str>, manual: Option<&str>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = manual.filter(|m| !m.is_empty()).map(PathBuf::from).into_iter().collect();
    let candidates = ini_candidates(system, executable);
    let parent = |p: &Path| p.parent().map(Path::to_path_buf).unwrap_or_default();
    if system == "ps1" {
        dirs.extend(candidates.iter().filter(|p| p.exists()).filter_map(|ini| ini_bios_dir(ini, "bios", &DUCKSTATION_DIR_RE, &parent(ini))));
        dirs.extend(candidates.iter().map(|c| parent(c).join("bios")));
        dirs.extend(executable.and_then(|e| Path::new(e).parent()).map(|d| d.join("bios")));
    } else {
        dirs.extend(candidates.iter().rev().filter(|p| p.exists()).filter_map(|ini| ini_bios_dir(ini, "folders", &PCSX2_DIR_RE, &parent(&parent(ini)))));
        dirs.extend(candidates.iter().map(|c| parent(&parent(c)).join("bios")));
    }
    let mut unique = Vec::new();
    for dir in dirs {
        if !unique.contains(&dir) && dir.exists() {
            unique.push(dir);
        }
    }
    unique
}

fn looks_like_bios(path: &Path, system: &str) -> bool {
    use std::io::Read;
    let len = if system == "ps2" { 0x80000 } else { 0x10000 };
    let mut head = Vec::new();
    if std::fs::File::open(path).and_then(|f| f.take(len).read_to_end(&mut head)).is_err() {
        return false;
    }
    let has = |needle: &[u8]| head.windows(needle.len()).any(|w| w == needle);
    if system == "ps1" { has(b"Sony Computer Entertainment") } else { has(b"RESET") && has(b"ROMVER") }
}

fn has_plausible_bios(dir: &Path, system: &str) -> bool {
    let (min, max) = if system == "ps1" { (256 * 1024, 16 * 1024 * 1024) } else { (4 * 1024 * 1024, 8 * 1024 * 1024) };
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    entries.flatten().any(|e| {
        let path = e.path();
        std::fs::metadata(&path).is_ok_and(|m| m.is_file() && (min..=max).contains(&m.len())) && looks_like_bios(&path, system)
    })
}

/// resolveInstalledBiosDir: the first folder with a real PS1/PS2 BIOS dump.
pub fn installed_bios_dir(system: &str, executable: Option<&str>, manual: Option<&str>) -> Option<PathBuf> {
    if system != "ps1" && system != "ps2" {
        return None;
    }
    bios_dirs(system, executable, manual).into_iter().find(|d| has_plausible_bios(d, system))
}

/// checkEmulatorBios: also remembers a detected folder as the system's BIOS path.
pub fn check_bios(store: &Store, system: &str, executable: Option<&str>, manual: Option<&str>) -> Result<Value, String> {
    if system != "ps1" && system != "ps2" {
        return Ok(json!({ "installed": true, "detectedPath": null }));
    }
    let detected = installed_bios_dir(system, executable, manual).map(|d| d.to_string_lossy().into_owned());
    if let Some(dir) = detected.as_deref().filter(|d| Some(*d) != manual) {
        emulators::update_config(store, system, |c| c["biosPath"] = json!(dir))?;
    }
    Ok(json!({ "installed": detected.is_some(), "detectedPath": detected }))
}

// ---- Launch -----------------------------------------------------------------

fn coded(code: &str, message: String) -> String {
    eprintln!("[classics] launch failed: {code}");
    format!("{code}: {message}")
}

fn emulator_args(binary: &str, target: &str) -> Vec<String> {
    let args: &[&str] = match binary {
        "duckstation" | "pcsx2" => &["-batch", "-fullscreen", "--"],
        "rpcs3" => &["--no-gui"],
        "ppsspp" => &["--pause-menu-exit", "--fullscreen"],
        _ => &["--batch", "--exec"],
    };
    args.iter().map(|a| a.to_string()).chain([target.to_string()]).collect()
}

/// findInstalledPs3GameEboot: an installed PKG's EBOOT under dev_hdd0.
fn installed_ps3_eboot(executable: Option<&str>, title_id: &str) -> Option<PathBuf> {
    let id = rom_scan::lookup_key(title_id);
    rpcs3_config_roots(executable).into_iter().map(|r| r.join("dev_hdd0").join("game").join(&id).join("USRDIR").join("EBOOT.BIN")).find(|p| p.exists())
}

fn spawn_detached(exe: &Path, args: &[String]) -> std::io::Result<u32> {
    let mut command = std::process::Command::new(exe);
    command.args(args).current_dir(exe.parent().unwrap_or(Path::new("."))).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    command.spawn().map(|child| child.id())
}

fn rpcs3_pids(system: &mut sysinfo::System) -> Vec<sysinfo::Pid> {
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    system.processes().iter().filter(|(_, p)| p.name().to_string_lossy().to_lowercase().contains("rpcs3")).map(|(pid, _)| *pid).collect()
}

/// stopRunningRpcs3: refuse while RPCS3 runs, or with `force` kill it and wait up to 10 s.
async fn stop_running_rpcs3(force: bool) -> Result<(), String> {
    let mut system = sysinfo::System::new();
    let running = rpcs3_pids(&mut system);
    if running.is_empty() {
        return Ok(());
    }
    if !force {
        return Err(coded("EMULATOR_ALREADY_RUNNING", "rpcs3 is already running".into()));
    }
    for pid in &running {
        if let Some(p) = system.process(*pid) {
            p.kill();
        }
    }
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if !rpcs3_pids(&mut system).iter().any(|p| running.contains(p)) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(coded("EMULATOR_ALREADY_RUNNING", "rpcs3 did not exit before relaunch".into()))
}

fn mark_played(store: &Store, key: &str, disc: &str) -> Result<(), String> {
    crate::games::update(store, key, |game| {
        game.insert("selectedDiscPath".into(), json!(disc));
        game.insert("lastTimePlayed".into(), json!(iso_now()));
    })
    .map(|_| ())
}

// shortcut: no emulator souvenirs or emulator achievement logs, and no gamemode/MangoHud
// wrappers (Linux only); playtime comes from the watcher following the emulator's PID.
async fn launch_disc(app: &AppHandle, key: &str, system: &str, disc: &str) -> Result<(), String> {
    let store = app.state::<Store>();
    let config = emulators::get_config(&store, system)?;
    let not_configured = || coded("EMULATOR_NOT_CONFIGURED", format!("Emulator not configured for {system}"));
    let Some(exe) = config["executablePath"].as_str().filter(|p| Path::new(p).exists()).map(str::to_string) else { return Err(not_configured()) };

    // DuckStation and PCSX2 crash silently without a BIOS; block the launch instead.
    if system == "ps1" || system == "ps2" {
        let (exe, manual) = (exe.clone(), config["biosPath"].as_str().map(str::to_string));
        let system_owned = system.to_string();
        let found = tauri::async_runtime::spawn_blocking(move || installed_bios_dir(&system_owned, Some(&exe), manual.as_deref())).await.map_err(|e| e.to_string())?;
        let Some(dir) = found else { return Err(coded("BIOS_NOT_CONFIGURED", format!("BIOS not configured for {system}"))) };
        let dir = dir.to_string_lossy().into_owned();
        if config["biosPath"].as_str() != Some(dir.as_str()) {
            emulators::update_config(&store, system, |c| c["biosPath"] = json!(dir))?;
        }
    }

    let lower = disc.to_lowercase();
    let target = if system == "ps3" && lower.ends_with(".pkg") {
        let title = rom_scan::ps3_pkg_title_id(Path::new(disc)).ok_or_else(|| coded("PKG_UNREADABLE", format!("Could not read PKG title id for {system}")))?;
        match installed_ps3_eboot(Some(&exe), &title) {
            Some(eboot) => eboot.to_string_lossy().into_owned(),
            None => {
                spawn_detached(Path::new(&exe), &["--installpkg".into(), disc.to_string()]).map_err(|_| not_configured())?;
                return Err(format!("PKG_INSTALLING: Installing PKG for {system}"));
            }
        }
    } else if config["binary"].as_str() == Some("pcsx2") && lower.ends_with(".mds") {
        rom_scan::sidecar_with_ext(Path::new(disc), ".mdf").map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| disc.to_string())
    } else {
        disc.to_string()
    };

    mark_played(&store, key, disc)?;
    let binary = config["binary"].as_str().unwrap_or_default();
    let pid = spawn_detached(Path::new(&exe), &emulator_args(binary, &target)).map_err(|_| not_configured())?;
    app.state::<EmulatorPids>().track(key, pid);
    Ok(())
}

fn retroarch_core(platform: &str) -> &'static str {
    match platform {
        "nes" => "fceumm",
        "snes" => "snes9x",
        "n64" => "mupen64plus_next",
        "gba" => "mgba",
        _ => "gambatte",
    }
}

async fn launch_retroarch(app: &AppHandle, key: &str, platform: &str, rom: &str) -> Result<(), String> {
    let store = app.state::<Store>();
    let config = store.get("retroarch", "config")?;
    let not_configured = || coded("RETROARCH_NOT_CONFIGURED", format!("RetroArch not configured for {platform}"));
    let Some(exe) = config["executablePath"].as_str().filter(|p| Path::new(p).exists()).map(str::to_string) else { return Err(not_configured()) };
    let core = &config["cores"][retroarch_core(platform)];
    let Some(core_path) = core["path"].as_str().filter(|p| core["installed"].as_bool() == Some(true) && Path::new(p).exists()) else {
        return Err(coded("CORE_NOT_INSTALLED", format!("Core not installed for {platform}")));
    };
    mark_played(&store, key, rom)?;
    let args = vec!["-L".to_string(), core_path.to_string(), rom.to_string(), "-f".to_string()];
    let pid = spawn_detached(Path::new(&exe), &args).map_err(|_| not_configured())?;
    app.state::<EmulatorPids>().track(key, pid);
    Ok(())
}

/// openClassicsGame.
pub async fn open(app: &AppHandle, shop: &str, object_id: &str, disc_path: Option<&str>, force: bool) -> Result<(), String> {
    if shop != "launchbox" {
        return Err("openClassicsGame called for non-launchbox shop".into());
    }
    let key = game_key(shop, object_id);
    let game = app.state::<Store>().get("games", &key)?;
    if !game.is_object() {
        return Err(format!("Game not found: {key}"));
    }
    let platform = game["platform"].as_str().unwrap_or_default();
    let first_disc = game["discs"][0]["path"].as_str();
    let exists = |p: &Option<&str>| p.is_some_and(|p| Path::new(p).exists());

    if let Some(ra) = retroarch_platform(platform) {
        let rom = disc_path.or(match &game["selectedDiscPath"] {
            Value::Null if game.get("selectedDiscPath").is_some() => None,
            Value::String(s) => Some(s.as_str()),
            _ => first_disc,
        });
        if !exists(&rom) {
            return Err(coded("NO_DISC", format!("No ROM available for game {object_id}")));
        }
        return launch_retroarch(app, &key, ra, rom.unwrap()).await;
    }

    let Some(system) = platform_to_system(platform) else { return Err(coded("PLATFORM_UNKNOWN", format!("Unknown platform for game {object_id}"))) };
    let disc = disc_path.or(game["selectedDiscPath"].as_str()).or(first_disc);
    if !exists(&disc) {
        return Err(coded("NO_DISC", format!("No disc available for game {object_id}")));
    }
    if system == "ps3" {
        stop_running_rpcs3(force).await?;
    }
    launch_disc(app, &key, system, disc.unwrap()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_retroarch_platforms() {
        assert_eq!(retroarch_platform("Nintendo Game Boy Advance"), Some("gba"));
        assert_eq!(retroarch_platform("Nintendo Game Boy Color"), Some("gbc"));
        assert_eq!(retroarch_platform("Super Nintendo Entertainment System"), Some("snes"));
        assert_eq!(retroarch_platform("Nintendo Entertainment System"), Some("nes"));
        assert_eq!(retroarch_platform("Nintendo 64"), Some("n64"));
        assert_eq!(retroarch_platform("Sony Playstation"), None);
    }

    #[test]
    fn builds_emulator_arguments() {
        assert_eq!(emulator_args("pcsx2", "g.iso"), ["-batch", "-fullscreen", "--", "g.iso"]);
        assert_eq!(emulator_args("rpcs3", "EBOOT.BIN"), ["--no-gui", "EBOOT.BIN"]);
        assert_eq!(emulator_args("dolphin", "g.rvz"), ["--batch", "--exec", "g.rvz"]);
    }

    #[test]
    fn updates_discs_and_falls_back_on_removal() {
        let store = Store::memory().unwrap();
        store.put("games", "launchbox:1", &json!({ "platform": "Sony Playstation", "discs": [{ "path": "a.cue" }, { "path": "b.cue" }], "selectedDiscPath": "b.cue" })).unwrap();
        let game = update_disc(&store, "launchbox", "1", &json!({ "removeDiscPath": "b.cue" })).unwrap();
        assert_eq!(game["selectedDiscPath"], "a.cue");
        let game = update_disc(&store, "launchbox", "1", &json!({ "addDisc": { "path": "c.cue", "sku": "SLUS-1" }, "selectedDiscPath": "c.cue", "dontAskDiscSelection": true })).unwrap();
        assert_eq!((game["discs"].as_array().unwrap().len(), game["selectedDiscPath"].as_str(), game["dontAskDiscSelection"].as_bool()), (2, Some("c.cue"), Some(true)));

        store.put("games", "launchbox:2", &json!({ "platform": "Nintendo 64", "discs": [{ "path": "x.z64" }, { "path": "y.z64" }], "selectedDiscPath": "y.z64" })).unwrap();
        let game = update_disc(&store, "launchbox", "2", &json!({ "removeDiscPath": "y.z64" })).unwrap();
        assert_eq!(game["selectedDiscPath"], Value::Null);
        assert!(update_disc(&store, "launchbox", "404", &json!({})).is_err());
    }

    #[test]
    fn finds_a_bios_by_signature_and_size() {
        let dir = std::env::temp_dir().join(format!("chimaera-bios-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut bios = vec![0u8; 512 * 1024];
        bios[0x100..0x11b].copy_from_slice(b"Sony Computer Entertainment");
        std::fs::write(dir.join("small.bin"), b"Sony Computer Entertainment").unwrap();
        assert!(!has_plausible_bios(&dir, "ps1"));
        std::fs::write(dir.join("scph1001.bin"), &bios).unwrap();
        assert!(has_plausible_bios(&dir, "ps1"));
        assert!(!has_plausible_bios(&dir, "ps2"));
        assert_eq!(installed_bios_dir("ps1", None, Some(&dir.to_string_lossy())), Some(dir.clone()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
