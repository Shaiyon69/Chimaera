//! Small system channels: temp files, network interfaces, start with Windows, Decky's folder.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

/// Shared with Electron's ipc-guards-core.ts, so its leftovers are cleaned the same way.
const TEMP_PREFIX: &str = "hydra-temp-";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Electron's auto-launch used app.getName(); the same value name replaces its entry.
const RUN_VALUE: &str = "chimaera";
/// What auto-launch adds for a minimized start (Electron's `isHidden`).
pub const HIDDEN_ARG: &str = "--hidden";

/// A Uint8Array reaches us JSON-encoded: an array, or an object keyed by index.
fn bytes(value: &Value) -> Option<Vec<u8>> {
    let byte = |v: &Value| v.as_u64().filter(|n| *n <= 255).map(|n| n as u8);
    match value {
        Value::Array(list) => list.iter().map(byte).collect(),
        Value::Object(map) => {
            let mut indexed: Vec<(usize, u8)> = map.iter().map(|(k, v)| Some((k.parse().ok()?, byte(v)?))).collect::<Option<_>>()?;
            indexed.sort_unstable_by_key(|(i, _)| *i);
            indexed.iter().enumerate().all(|(n, (i, _))| n == *i).then(|| indexed.into_iter().map(|(_, b)| b).collect())
        }
        _ => None,
    }
}

/// `saveTempFile`: the file name is reduced to its last component.
pub fn save_temp_file(name: &str, data: &Value) -> Result<String, String> {
    let data = bytes(data).ok_or("Failed to save temp file: no file data")?;
    let name = Path::new(name).file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let path = std::env::temp_dir().join(format!("{TEMP_PREFIX}{}-{name}", crate::library::now_ms()));
    std::fs::write(&path, data).map_err(|e| format!("Failed to save temp file: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

/// isOwnTempFile: only files this app wrote straight into the temp folder.
fn is_own_temp_file(path: &Path, temp: &Path) -> bool {
    path.is_absolute()
        && !path.components().any(|c| matches!(c, std::path::Component::ParentDir))
        && path.parent() == Some(temp)
        && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(TEMP_PREFIX))
}

/// `deleteTempFile`: refuses anything else; a failed delete is left to the OS.
pub fn delete_temp_file(path: &str) {
    let path = PathBuf::from(path);
    // Path equality compares components, so temp_dir's trailing separator does not matter.
    if !is_own_temp_file(&path, &std::env::temp_dir()) {
        eprintln!("[misc] blocked deleteTempFile outside temp files: {}", path.display());
        return;
    }
    let _ = std::fs::remove_file(path);
}

/// `getNetworkInterfaces`: interfaces with a non-loopback address, IPv4 first.
pub fn network_interfaces() -> Value {
    let networks = sysinfo::Networks::new_with_refreshed_list();
    let mut list: Vec<(String, Vec<std::net::IpAddr>)> = networks
        .iter()
        .map(|(name, data)| (name.clone(), data.ip_networks().iter().map(|n| n.addr).filter(|a| !a.is_loopback()).collect::<Vec<_>>()))
        .filter(|(_, addresses)| !addresses.is_empty())
        .collect();
    list.sort_by(|a, b| a.0.cmp(&b.0));
    Value::Array(
        list.into_iter()
            .map(|(name, mut addresses)| {
                addresses.sort_by_key(|a| a.is_ipv6());
                json!({ "name": name, "addresses": addresses.iter().map(|a| a.to_string()).collect::<Vec<_>>() })
            })
            .collect(),
    )
}

/// resolveNetworkInterfaceBinding: the interface's usable addresses joined by commas, or its
/// name when it has none (libtorrent takes either).
pub fn interface_binding(name: Option<&str>) -> String {
    let Some(name) = name.filter(|n| !n.is_empty()) else { return String::new() };
    let networks = sysinfo::Networks::new_with_refreshed_list();
    let usable: Vec<String> = networks
        .iter()
        .filter(|(n, _)| n.as_str() == name)
        .flat_map(|(_, data)| data.ip_networks().iter().map(|n| n.addr).collect::<Vec<_>>())
        .filter(|a| !a.is_loopback() && !a.to_string().to_lowercase().starts_with("fe80"))
        .map(|a| a.to_string())
        .collect();
    if usable.is_empty() { name.to_string() } else { usable.join(",") }
}

/// `autoLaunch`: the per-user Run entry. Dev builds never register themselves.
pub fn auto_launch(options: &Value) -> Result<(), String> {
    if cfg!(debug_assertions) {
        return Ok(());
    }
    set_run_entry(options["enabled"].as_bool().unwrap_or(false), options["minimized"].as_bool().unwrap_or(false))
}

#[cfg(windows)]
fn set_run_entry(enabled: bool, minimized: bool) -> Result<(), String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    let key = winreg::RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE).map_err(|e| e.to_string())?;
    if !enabled {
        return match key.delete_value(RUN_VALUE) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        };
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let command = format!("\"{}\"{}", exe.display(), if minimized { format!(" {HIDDEN_ARG}") } else { String::new() });
    key.set_value(RUN_VALUE, &command).map_err(|e| e.to_string())
}

// shortcut: start-with-system is Windows only for now (Linux needs an XDG autostart entry).
#[cfg(not(windows))]
fn set_run_entry(_enabled: bool, _minimized: bool) -> Result<(), String> {
    Ok(())
}

/// `checkHomebrewFolderExists`: Decky Loader's ~/homebrew (Steam Deck).
pub fn homebrew_folder_exists(app: &AppHandle) -> bool {
    app.path().home_dir().is_ok_and(|home| home.join("homebrew").exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_uint8array_json_both_ways() {
        assert_eq!(bytes(&json!([1, 2, 255])), Some(vec![1, 2, 255]));
        assert_eq!(bytes(&json!({ "1": 9, "0": 8 })), Some(vec![8, 9]));
        assert_eq!(bytes(&json!({ "0": 8, "2": 9 })), None);
        assert_eq!(bytes(&json!([256])), None);
    }

    /// Prints this machine's list: `cargo test lists_live_interfaces -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn lists_live_interfaces() {
        let list = network_interfaces();
        eprintln!("{list:#}");
        let first = list[0]["name"].as_str().unwrap();
        eprintln!("binding for {first}: {}", interface_binding(Some(first)));
    }

    #[test]
    fn temp_files_round_trip_and_stay_in_temp() {
        // `\` separates paths only on Windows; elsewhere it is part of the file name.
        let name = if cfg!(windows) { r"..\..\evil.png" } else { "../../evil.png" };
        let path = save_temp_file(name, &json!([1, 2, 3])).unwrap();
        let file = PathBuf::from(&path);
        assert_eq!(file.parent().unwrap(), std::env::temp_dir());
        assert!(file.file_name().unwrap().to_str().unwrap().ends_with("-evil.png"));
        let temp = std::env::temp_dir();
        assert!(!is_own_temp_file(&temp.join("other.png"), &temp));
        assert!(!is_own_temp_file(&temp.join("x").join("hydra-temp-1.png"), &temp));
        assert!(!is_own_temp_file(Path::new("hydra-temp-1.png"), &temp));
        delete_temp_file(&path);
        assert!(!file.exists());
    }
}
