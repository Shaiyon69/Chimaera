//! Emulator configs (port of `services/emulators/emulators-repository.ts`,
//! `known-binaries.ts`, `validate-emulator-executable.ts`, `detect-emulator.ts`,
//! `find-emulator-in-download-directories.ts`, `get-emulator-version.ts`) and the
//! config channels from `events/emulators/`. Configs live in the imported
//! `emulators` sublevel, keyed by system, in Electron's shape.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::store::Store;

pub struct Known {
    pub system: &'static str,
    pub binary: &'static str,
    pub display_name: &'static str,
    pub(crate) linux_names: &'static [&'static str],
    pub(crate) windows_names: &'static [&'static str],
    version_flags: &'static [&'static str],
    version_probe_off_on_windows: bool,
    pub rom_extensions: &'static [&'static str],
}

pub const SYSTEMS: [&str; 5] = ["ps1", "ps2", "ps3", "psp", "dolphin"];

pub const KNOWN: [Known; 5] = [
    Known {
        system: "ps1",
        binary: "duckstation",
        display_name: "DuckStation",
        linux_names: &["duckstation-qt", "duckstation-nogui", "duckstation", "DuckStation"],
        windows_names: &["duckstation-qt-x64-ReleaseLTCG.exe", "duckstation-qt.exe", "duckstation-nogui.exe"],
        version_flags: &["-version"],
        version_probe_off_on_windows: false,
        rom_extensions: &[".cue", ".bin", ".iso", ".chd", ".pbp", ".img", ".sub", ".ccd", ".mds", ".mdf", ".ecm", ".m3u"],
    },
    Known {
        system: "ps2",
        binary: "pcsx2",
        display_name: "PCSX2",
        linux_names: &["pcsx2-qt", "pcsx2", "PCSX2"],
        windows_names: &["pcsx2-qt.exe", "pcsx2-qtx64-avx2.exe", "pcsx2.exe"],
        version_flags: &["-version"],
        version_probe_off_on_windows: false,
        rom_extensions: &[".iso", ".chd", ".cso", ".zso", ".gz", ".nrg", ".cue", ".bin", ".mds", ".mdf", ".m3u"],
    },
    Known {
        system: "ps3",
        binary: "rpcs3",
        display_name: "RPCS3",
        linux_names: &["rpcs3", "RPCS3"],
        windows_names: &["rpcs3.exe"],
        version_flags: &["--version"],
        version_probe_off_on_windows: false,
        rom_extensions: &[".iso", ".pkg", ".elf", ".self"],
    },
    Known {
        system: "psp",
        binary: "ppsspp",
        display_name: "PPSSPP",
        linux_names: &["PPSSPPSDL", "ppsspp"],
        windows_names: &["PPSSPPWindows64.exe", "PPSSPPWindowsARM64.exe"],
        version_flags: &["--version"],
        version_probe_off_on_windows: true,
        rom_extensions: &[".iso", ".cso", ".chd", ".pbp"],
    },
    Known {
        system: "dolphin",
        binary: "dolphin",
        display_name: "Dolphin",
        // `dolphin` alone is KDE's file manager on Linux.
        linux_names: &["dolphin-emu", "dolphin-emu-qt2"],
        windows_names: &["Dolphin.exe"],
        version_flags: &["--version"],
        version_probe_off_on_windows: false,
        rom_extensions: &[".iso", ".gcm", ".bin", ".rvz", ".wia", ".gcz", ".ciso", ".tgc", ".wbfs", ".wad"],
    },
];

/// RETROARCH_DETECTABLE. Kept out of `KNOWN`: RetroArch has its own `retroarch` store, not an `emulators` entry.
pub const RETROARCH: Known = Known {
    system: "retroarch",
    binary: "retroarch",
    display_name: "RetroArch",
    linux_names: &["retroarch", "RetroArch"],
    windows_names: &["retroarch.exe"],
    version_flags: &["--version"],
    version_probe_off_on_windows: false,
    rom_extensions: &[],
};

pub fn known(system: &str) -> Result<&'static Known, String> {
    KNOWN.iter().find(|k| k.system == system).ok_or_else(|| format!("Unknown emulator system: {system}"))
}

fn empty_config(k: &Known) -> Value {
    json!({
        "system": k.system, "binary": k.binary, "executablePath": null, "detectedVersion": null,
        "detectedAt": null, "biosPath": null, "romFolders": [], "lastScanAt": null,
        "totalFiles": 0, "totalSizeBytes": 0,
    })
}

// Electron shares the executable between systems with the same binary; every
// system has its own binary today, so there are no siblings to sync.
pub fn get_config(store: &Store, system: &str) -> Result<Value, String> {
    let k = known(system)?;
    let stored = store.get("emulators", system)?;
    Ok(if stored.is_object() { stored } else { empty_config(k) })
}

pub fn all_configs(store: &Store) -> Result<Value, String> {
    let mut map = serde_json::Map::new();
    for system in SYSTEMS {
        map.insert(system.into(), get_config(store, system)?);
    }
    Ok(Value::Object(map))
}

pub fn update_config(store: &Store, system: &str, patch: impl FnOnce(&mut Value)) -> Result<Value, String> {
    let previous = get_config(store, system)?;
    let mut next = previous.clone();
    patch(&mut next);
    // clearRpcs3RootOnExecutableChange: the config root belongs to one RPCS3 install.
    if system == "ps3" && previous["executablePath"] != next["executablePath"] {
        next["rpcs3ConfigRoot"] = Value::Null;
    }
    store.put("emulators", system, &next)?;
    Ok(next)
}

fn set_executable(config: &mut Value, path: Option<&Path>, version: Option<String>) {
    config["executablePath"] = json!(path.map(|p| p.to_string_lossy()));
    config["detectedVersion"] = json!(version);
    config["detectedAt"] = if path.is_some() { json!(crate::library::now_ms()) } else { Value::Null };
}

const NON_EXECUTABLE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "bmp", "webp", "svg", "ico", "mp3", "wav", "flac", "ogg", "mp4", "mkv", "avi", "mov",
    "webm", "txt", "md", "pdf", "doc", "docx", "rtf", "zip", "rar", "7z", "tar", "gz", "xz", "iso", "bin", "cue", "chd",
    "pkg", "cso", "json", "xml", "ini", "cfg", "log", "html", "htm",
];

fn read_at(path: &Path, offset: u64, buf: &mut [u8]) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    std::fs::File::open(path).and_then(|mut f| { f.seek(SeekFrom::Start(offset))?; f.read_exact(buf) }).is_ok()
}

fn has_pe_header(path: &Path) -> bool {
    let mut dos = [0u8; 64];
    if !read_at(path, 0, &mut dos) || &dos[..2] != b"MZ" {
        return false;
    }
    let offset = u32::from_le_bytes([dos[0x3c], dos[0x3d], dos[0x3e], dos[0x3f]]) as u64;
    let mut sig = [0u8; 4];
    read_at(path, offset, &mut sig) && sig == *b"PE\0\0"
}

#[cfg(unix)]
fn has_executable_header(path: &Path) -> bool {
    use std::io::Read;
    let mut bytes = Vec::new();
    if std::fs::File::open(path).and_then(|f| f.take(4).read_to_end(&mut bytes)).is_err() {
        return false;
    }
    if bytes.starts_with(b"#!") {
        return true;
    }
    let Ok(prefix) = <[u8; 4]>::try_from(bytes.as_slice()) else { return false };
    const MACH_O: [[u8; 4]; 8] = [
        [0xfe, 0xed, 0xfa, 0xce], [0xce, 0xfa, 0xed, 0xfe], [0xfe, 0xed, 0xfa, 0xcf], [0xcf, 0xfa, 0xed, 0xfe],
        [0xca, 0xfe, 0xba, 0xbe], [0xbe, 0xba, 0xfe, 0xca], [0xca, 0xfe, 0xba, 0xbf], [0xbf, 0xba, 0xfe, 0xca],
    ];
    prefix == *b"\x7fELF" || MACH_O.contains(&prefix) || (&prefix[..2] == b"MZ" && has_pe_header(path))
}

/// isValidEmulatorExecutable. macOS app bundles are not handled (no macOS build).
pub fn is_valid_executable(path: &Path) -> bool {
    if !path.metadata().map(|m| m.is_file()).unwrap_or(false) {
        return false;
    }
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if NON_EXECUTABLE_EXTENSIONS.contains(&ext.as_str()) {
        return false;
    }
    #[cfg(windows)]
    {
        if ext == "exe" { has_pe_header(path) } else { ext == "bat" || ext == "cmd" }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata().map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false) && has_executable_header(path)
    }
}

/// isExecutableNameExpectedForBinary: PPSSPP and Dolphin need their name in the file.
fn name_expected(path: &Path, k: &Known) -> bool {
    if k.binary != "ppsspp" && k.binary != "dolphin" {
        return true;
    }
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    name.contains(k.binary) && !(cfg!(target_os = "linux") && k.binary == "dolphin" && name == "dolphin")
}

pub fn is_valid_for(path: &Path, k: &Known) -> bool {
    is_valid_executable(path) && name_expected(path, k)
}

pub(crate) fn no_window(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Runs a command for at most `timeout` and returns stdout + stderr.
fn run_capture(command: &mut Command, timeout: Duration) -> Option<(bool, String)> {
    let mut child = no_window(command).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                break;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    let text = format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    Some((output.status.success(), text))
}

/// First match of /\d+\.\d+(?:\.\d+)?[a-zA-Z0-9.-]*/.
fn find_version(text: &str) -> Option<String> {
    let b = text.as_bytes();
    let digits = |mut i: usize| { while i < b.len() && b[i].is_ascii_digit() { i += 1 } i };
    for start in 0..b.len() {
        if !b[start].is_ascii_digit() || (start > 0 && b[start - 1].is_ascii_digit()) {
            continue;
        }
        let major = digits(start);
        if major >= b.len() || b[major] != b'.' || digits(major + 1) == major + 1 {
            continue;
        }
        let mut end = digits(major + 1);
        while end < b.len() && (b[end].is_ascii_alphanumeric() || b[end] == b'.' || b[end] == b'-') {
            end += 1;
        }
        return Some(text[start..end].to_string());
    }
    None
}

pub fn version_of(path: &Path, k: &Known) -> Option<String> {
    if k.version_flags.is_empty() || (cfg!(windows) && k.version_probe_off_on_windows) {
        return None;
    }
    k.version_flags.iter().find_map(|flag| {
        run_capture(Command::new(path).arg(flag), Duration::from_secs(5)).and_then(|(_, out)| find_version(&out))
    })
}

fn names(k: &Known) -> &'static [&'static str] {
    if cfg!(windows) { k.windows_names } else { k.linux_names }
}

fn on_path(name: &str) -> Option<PathBuf> {
    let (ok, out) = run_capture(Command::new(if cfg!(windows) { "where" } else { "which" }).arg(name), Duration::from_secs(3))?;
    let first = out.lines().map(str::trim).find(|l| !l.is_empty())?;
    (ok && Path::new(first).exists()).then(|| PathBuf::from(first))
}

fn env_dir(name: &str, fallback: &str) -> PathBuf {
    std::env::var_os(name).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(fallback))
}

fn home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default()
}

fn install_dirs() -> Vec<PathBuf> {
    if cfg!(windows) {
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| home().join("AppData").join("Local"));
        [env_dir("ProgramFiles", r"C:\Program Files"), env_dir("ProgramFiles(x86)", r"C:\Program Files (x86)"), local]
            .into_iter()
            .flat_map(|root| { let programs = root.join("Programs"); [root, programs] })
            .collect()
    } else {
        let h = home();
        ["/usr/bin", "/usr/local/bin", "/var/lib/flatpak/exports/bin"].iter().map(PathBuf::from)
            .chain([h.join(".local/bin"), h.join(".local/share/flatpak/exports/bin"), h.join("Applications"), PathBuf::from("/opt")])
            .collect()
    }
}

/// `duckstation-qt.exe` -> `duckstation`: installs often sit in a folder named after the binary.
fn subdir_name(name: &str) -> &str {
    let base = name.strip_suffix(".exe").or_else(|| name.strip_suffix(".EXE")).unwrap_or(name);
    base.split(['-', '_']).next().unwrap_or(base)
}

fn search_dirs(names: &[&str], dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter().filter(|d| d.exists()).find_map(|dir| {
        names.iter().find_map(|name| {
            [dir.join(name), dir.join(subdir_name(name)).join(name)].into_iter().find(|p| p.exists())
        })
    })
}

fn child_dirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).map(|it| it.flatten().filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false)).map(|e| e.path()).collect()).unwrap_or_default()
}

fn find_direct(dir: &Path, names: &[&str], k: &Known) -> Option<PathBuf> {
    names.iter().map(|n| dir.join(n)).find(|p| is_valid_for(p, k))
}

/// Breadth-first, 4 levels deep and 2,000 folders at most, like Electron.
fn find_nested(root: &Path, names: &[&str], k: &Known) -> Option<PathBuf> {
    let mut pending = vec![(root.to_path_buf(), 0)];
    let mut i = 0;
    while i < pending.len() && i < 2_000 {
        let (dir, depth) = pending[i].clone();
        i += 1;
        if let Some(hit) = find_direct(&dir, names, k) {
            return Some(hit);
        }
        if depth < 4 {
            pending.extend(child_dirs(&dir).into_iter().map(|d| (d, depth + 1)));
        }
    }
    None
}

/// findManagedEmulatorExecutable: any known name for this emulator, any depth.
fn find_managed(root: &Path, k: &Known) -> Option<PathBuf> {
    let wanted: Vec<String> = k.linux_names.iter().chain(k.windows_names).map(|n| n.to_lowercase()).collect();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() && wanted.contains(&entry.file_name().to_string_lossy().to_lowercase()) {
                return Some(entry.path());
            }
        }
    }
    None
}

fn find_in_downloads(k: &Known, download_dirs: &[PathBuf]) -> Option<PathBuf> {
    let names = names(k);
    download_dirs.iter().find_map(|dir| {
        find_direct(dir, names, k)
            .or_else(|| {
                [dir.join(k.display_name), dir.join(k.binary)].iter().find_map(|emu_dir| {
                    find_managed(emu_dir, k).filter(|p| is_valid_for(p, k)).or_else(|| find_nested(emu_dir, names, k))
                })
            })
            .or_else(|| find_nested(dir, names, k))
    })
}

fn portable_dirs() -> Vec<PathBuf> {
    let h = home();
    vec![h.join("Downloads"), h.join("Desktop"), h.join("Emulators"), PathBuf::from(r"D:\Emulators"), PathBuf::from(r"C:\Emulators"), h.join("emudeck").join("EmulationStation-DE").join("Emulators")]
}

/// detectEmulator: PATH, install folders, the downloads folders, then (Windows)
/// one level into the usual portable folders.
pub fn detect(k: &Known, download_dirs: &[PathBuf], resolve_version: bool) -> Option<(PathBuf, Option<String>)> {
    let names = names(k);
    let found = names.iter().find_map(|n| on_path(n))
        .or_else(|| search_dirs(names, &install_dirs()))
        .or_else(|| find_in_downloads(k, download_dirs))
        .or_else(|| {
            if !cfg!(windows) {
                return None;
            }
            portable_dirs().iter().flat_map(|d| child_dirs(d)).find_map(|sub| names.iter().map(|n| sub.join(n)).find(|p| p.exists()))
        })?;
    let version = if resolve_version { version_of(&found, k) } else { None };
    Some((found, version))
}

/// The configured downloads folder plus the OS one, like detectEmulatorWithDownloads.
pub fn download_dirs(store: &Store, default_downloads: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![crate::installer::downloads_path(store, default_downloads), default_downloads.to_path_buf()];
    dirs.dedup();
    dirs
}

/// detectEmulator / detectEmulators: keep the old path only while it is still valid.
pub fn apply_detection(store: &Store, k: &Known, result: Option<(PathBuf, Option<String>)>) -> Result<Value, String> {
    update_config(store, k.system, |config| match result {
        Some((path, version)) => {
            let version = version.map(Value::from).unwrap_or_else(|| config["detectedVersion"].clone());
            set_executable(config, Some(&path), None);
            config["detectedVersion"] = version;
        }
        None => {
            let still_valid = config["executablePath"].as_str().is_some_and(|p| is_valid_for(Path::new(p), k));
            if !still_valid {
                set_executable(config, None, None);
            }
        }
    })
}

/// setEmulatorExecutablePath: null clears it; an invalid file answers null.
pub fn set_executable_path(store: &Store, k: &Known, path: Option<&str>) -> Result<Value, String> {
    let Some(path) = path.filter(|p| !p.is_empty()).map(PathBuf::from) else {
        return update_config(store, k.system, |c| set_executable(c, None, None));
    };
    if !is_valid_for(&path, k) {
        return Ok(Value::Null);
    }
    let version = version_of(&path, k);
    // The probe can take seconds; the file may have moved meanwhile.
    if !is_valid_for(&path, k) {
        return Ok(Value::Null);
    }
    update_config(store, k.system, |c| set_executable(c, Some(&path), version))
}

pub fn set_bios_path(store: &Store, system: &str, path: Option<&str>) -> Result<Value, String> {
    let dir = path.filter(|p| !p.is_empty()).map(PathBuf::from).filter(|p| p.is_dir());
    update_config(store, system, |c| c["biosPath"] = json!(dir.map(|d| d.to_string_lossy().into_owned())))
}

pub fn remove(store: &Store, system: &str) -> Result<Value, String> {
    update_config(store, system, |c| {
        set_executable(c, None, None);
        for (key, value) in [("biosPath", Value::Null), ("romFolders", json!([])), ("lastScanAt", Value::Null), ("totalFiles", json!(0)), ("totalSizeBytes", json!(0))] {
            c[key] = value;
        }
    })
}

pub fn preview(k: &Known, path: Option<&str>, download_dirs: &[PathBuf]) -> Value {
    let found = match path.filter(|p| !p.is_empty()) {
        Some(p) => Some(PathBuf::from(p)).filter(|p| is_valid_for(p, k)).map(|p| { let v = version_of(&p, k); (p, v) }),
        None => detect(k, download_dirs, true),
    };
    found.map(|(p, v)| json!({ "executablePath": p.to_string_lossy(), "detectedVersion": v })).unwrap_or(Value::Null)
}

/// openGameWinetricks: Linux only, in the game's effective prefix (the one Play uses).
pub fn open_winetricks(store: &Store, shop: &str, object_id: &str) -> Result<bool, String> {
    if !cfg!(target_os = "linux") {
        return Ok(false);
    }
    let key = crate::games::game_key(shop, object_id);
    let game = store.get("games", &key)?;
    if !game.is_object() {
        return Ok(false);
    }
    let prefix = crate::proton::prefix_for(&game, &store.get("", "userPreferences")?, &key);
    let mut command = Command::new("winetricks");
    crate::proton::use_host_env(&mut command).env("WINEPREFIX", prefix).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    match command.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
            Ok(true)
        }
        Err(e) => {
            eprintln!("[winetricks] failed to launch: {e}");
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_versions_like_the_electron_regex() {
        assert_eq!(find_version("DuckStation v0.1-7465-g1ab2c3d"), Some("0.1-7465-g1ab2c3d".into()));
        assert_eq!(find_version("PCSX2 Nightly - v2.3.12 (Qt)"), Some("2.3.12".into()));
        assert_eq!(find_version("RPCS3 0.0.34-17358-abc Alpha"), Some("0.0.34-17358-abc".into()));
        assert_eq!(find_version("build 42, no dots"), None);
        assert_eq!(find_version("10."), None);
    }

    #[test]
    fn checks_executable_names_for_ppsspp_and_dolphin() {
        let psp = known("psp").unwrap();
        assert!(name_expected(Path::new("C:/emu/PPSSPPWindows64.exe"), psp));
        assert!(!name_expected(Path::new("C:/emu/game.exe"), psp));
        assert!(name_expected(Path::new("C:/emu/anything.exe"), known("ps2").unwrap()));
    }

    #[test]
    fn rejects_non_executables_and_accepts_pe_files() {
        let dir = std::env::temp_dir().join(format!("chimaera-emu-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = dir.join("readme.txt");
        std::fs::write(&text, "MZ").unwrap();
        assert!(!is_valid_executable(&text));
        let mut pe = vec![0u8; 0x48];
        pe[..2].copy_from_slice(b"MZ");
        pe[0x3c] = 0x40;
        pe[0x40..0x44].copy_from_slice(b"PE\0\0");
        let exe = dir.join("pcsx2-qt.exe");
        std::fs::write(&exe, &pe).unwrap();
        assert!(has_pe_header(&exe));
        let fake = dir.join("fake.exe");
        std::fs::write(&fake, b"MZ not a pe file at all, just text padding here........................").unwrap();
        assert!(!has_pe_header(&fake));
        #[cfg(windows)]
        {
            assert!(is_valid_executable(&exe));
            assert!(!is_valid_executable(&fake));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_an_emulator_in_a_named_downloads_folder() {
        let dir = std::env::temp_dir().join(format!("chimaera-emu-{}", uuid::Uuid::new_v4()));
        let nested = dir.join("PCSX2").join("pcsx2-v2.3");
        std::fs::create_dir_all(&nested).unwrap();
        let mut pe = vec![0u8; 0x48];
        pe[..2].copy_from_slice(b"MZ");
        pe[0x3c] = 0x40;
        pe[0x40..0x44].copy_from_slice(b"PE\0\0");
        let name = if cfg!(windows) { "pcsx2-qt.exe" } else { "pcsx2-qt" };
        std::fs::write(nested.join(name), &pe).unwrap();
        #[cfg(windows)]
        assert_eq!(find_in_downloads(known("ps2").unwrap(), &[dir.clone()]), Some(nested.join(name)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn subdir_names_drop_suffixes() {
        assert_eq!(subdir_name("duckstation-qt-x64-ReleaseLTCG.exe"), "duckstation");
        assert_eq!(subdir_name("rpcs3.exe"), "rpcs3");
        assert_eq!(subdir_name("Dolphin.exe"), "Dolphin");
    }
}
