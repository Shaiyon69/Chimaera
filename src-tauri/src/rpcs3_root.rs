//! RPCS3 config root (rpcs3-config-root.ts) and PS3 firmware detection (firmware-detection.ts).

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::achievement_files::real_path;
use crate::classics_import::rpcs3_config_roots;
use crate::emulators;
use crate::store::Store;

pub struct Location {
    pub config_root: PathBuf,
}

fn read_optional(file: &Path) -> Result<Option<String>, ()> {
    match std::fs::read_to_string(file) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    }
}

/// resolveRpcs3VfsHdd0: `/dev_hdd0/` from vfs.yml, `$(EmulatorDir)` expanded.
fn vfs_hdd0(config_root: &Path, vfs: Option<&str>) -> Option<PathBuf> {
    let Some(vfs) = vfs else { return Some(config_root.join("dev_hdd0")) };
    // Empty YAML is null in Electron's parser and rejected there.
    let serde_yaml::Value::Mapping(config) = serde_yaml::from_str(vfs).ok()? else { return None };
    let emulator_dir = match config.get("$(EmulatorDir)") {
        None => None,
        Some(serde_yaml::Value::String(s)) if s.is_empty() => None,
        Some(serde_yaml::Value::String(s)) if Path::new(s).is_absolute() => Some(PathBuf::from(s)),
        Some(_) => return None,
    };
    let base = emulator_dir.unwrap_or_else(|| config_root.to_path_buf());
    let value = match config.get("/dev_hdd0/") {
        None => "$(EmulatorDir)dev_hdd0/",
        Some(serde_yaml::Value::String(s)) => s.as_str(),
        Some(_) => return None,
    };
    if value.is_empty() || value.replace("$(EmulatorDir)", "").contains("$(") {
        return None;
    }
    let expanded = PathBuf::from(value.replace("$(EmulatorDir)", &format!("{}{}", base.display(), std::path::MAIN_SEPARATOR)));
    expanded.is_absolute().then_some(expanded)
}

/// validateRpcs3ConfigRoot: a folder with config.yml, persistent_settings.dat or
/// vfs.yml, a parseable config.yml, and an existing dev_hdd0.
pub fn validate(root: &Path) -> Option<Location> {
    if !root.is_absolute() {
        return None;
    }
    let config_root = real_path(root).filter(|p| p.is_dir())?;
    let config = read_optional(&config_root.join("config.yml")).ok()?;
    let settings = read_optional(&config_root.join("GuiConfigs").join("persistent_settings.dat")).ok()?;
    let vfs = read_optional(&config_root.join("vfs.yml")).ok()?;
    if config.is_none() && settings.is_none() && vfs.is_none() {
        return None;
    }
    if config.as_deref().is_some_and(|c| serde_yaml::from_str::<serde_yaml::Value>(c).is_err()) {
        return None;
    }
    real_path(&vfs_hdd0(&config_root, vfs.as_deref())?).filter(|p| p.is_dir())?;
    Some(Location { config_root })
}

/// resolveRpcs3ConfigRootStatus.
pub fn resolve(executable: Option<&str>, selected: Option<&str>) -> (Value, Option<Location>) {
    let status = |status: &str, candidates: &[String], resolved: Option<&Path>| {
        json!({ "status": status, "selectedRoot": selected, "resolvedRoot": resolved.map(|p| p.to_string_lossy()), "candidates": candidates })
    };
    if !executable.is_some_and(|e| Path::new(e).exists()) {
        return (status("not-configured", &[], None), None);
    }
    let mut locations: Vec<Location> = Vec::new();
    for root in rpcs3_config_roots(executable) {
        let Some(location) = validate(&root) else { continue };
        let key = |p: &Path| if cfg!(windows) { p.to_string_lossy().to_lowercase() } else { p.to_string_lossy().into_owned() };
        if !locations.iter().any(|l| key(&l.config_root) == key(&location.config_root)) {
            locations.push(location);
        }
    }
    let candidates: Vec<String> = locations.iter().map(|l| l.config_root.to_string_lossy().into_owned()).collect();
    if let Some(selected) = selected.filter(|s| !s.is_empty()) {
        return match validate(Path::new(selected)) {
            Some(location) => (status("ready", &candidates, Some(&location.config_root)), Some(location)),
            None => (status("invalid-selection", &candidates, None), None),
        };
    }
    match locations.len() {
        0 => (status("missing", &candidates, None), None),
        1 => {
            let location = locations.remove(0);
            (status("ready", &candidates, Some(&location.config_root)), Some(location))
        }
        _ => (status("ambiguous", &candidates, None), None),
    }
}

pub fn status(store: &Store) -> Result<Value, String> {
    let config = emulators::get_config(store, "ps3")?;
    Ok(resolve(config["executablePath"].as_str(), config["rpcs3ConfigRoot"].as_str()).0)
}

pub fn set_root(store: &Store, root: &str) -> Result<Value, String> {
    let config = emulators::get_config(store, "ps3")?;
    let executable = config["executablePath"].clone();
    if executable.is_null() {
        return Err("cloud_save_rpcs3_not_configured".into());
    }
    let location = validate(Path::new(root)).ok_or("cloud_save_rpcs3_config_invalid_selection")?;
    let current = emulators::get_config(store, "ps3")?;
    if current["executablePath"] != executable {
        return Err("cloud_save_rpcs3_not_configured".into());
    }
    emulators::update_config(store, "ps3", |c| c["rpcs3ConfigRoot"] = json!(location.config_root.to_string_lossy()))
}

/// isPs3FirmwareInstalled: a non-empty dev_flash/sys/external in any RPCS3 root.
pub fn firmware_installed(executable: Option<&str>) -> bool {
    rpcs3_config_roots(executable)
        .into_iter()
        .any(|root| std::fs::read_dir(root.join("dev_flash").join("sys").join("external")).is_ok_and(|mut d| d.next().is_some()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chimaera-rpcs3-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn validates_roots_and_vfs_hdd0() {
        let root = temp("root");
        assert!(validate(&root).is_none(), "no config files");
        std::fs::write(root.join("config.yml"), "Core: {}\n").unwrap();
        assert!(validate(&root).is_none(), "no dev_hdd0");
        std::fs::create_dir(root.join("dev_hdd0")).unwrap();
        assert_eq!(validate(&root).unwrap().config_root, real_path(&root).unwrap());

        let elsewhere = temp("hdd0");
        std::fs::write(root.join("vfs.yml"), format!("$(EmulatorDir): \"\"\n/dev_hdd0/: {}\n", elsewhere.display())).unwrap();
        let vfs = std::fs::read_to_string(root.join("vfs.yml")).unwrap();
        assert_eq!(vfs_hdd0(&root, Some(&vfs)), Some(elsewhere.clone()));
        assert!(validate(&root).is_some());
        std::fs::write(root.join("vfs.yml"), "/dev_hdd0/: $(Other)hdd0/\n").unwrap();
        assert!(validate(&root).is_none(), "unknown variable");
        std::fs::write(root.join("vfs.yml"), "").unwrap();
        assert!(validate(&root).is_none(), "empty vfs.yml is invalid");
        std::fs::write(root.join("vfs.yml"), "{}\n").unwrap();
        std::fs::write(root.join("config.yml"), "a: [\n").unwrap();
        assert!(validate(&root).is_none(), "broken config.yml");
        assert!(validate(Path::new("relative")).is_none());
    }

    #[test]
    fn firmware_needs_files_in_sys_external() {
        let dir = temp("fw");
        let exe = dir.join("rpcs3.exe");
        let exe = exe.to_str();
        let external = dir.join("dev_flash").join("sys").join("external");
        std::fs::create_dir_all(&external).unwrap();
        // The user's own RPCS3 install may count, so only the positive case is asserted.
        std::fs::write(external.join("libaudio.sprx"), b"x").unwrap();
        assert!(firmware_installed(exe));
    }
}
