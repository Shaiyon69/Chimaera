//! Linux Proton through umu-run: the port of src/main/services/umu.ts and wine.ts.
//! Proton builds come from Steam (`steamapps/common/Proton*`) and from compatibility tools
//! (`compatibilitytools.d`, where ProtonPlus installs GE-Proton).

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::classics_import::home_dir;
use crate::games::is_proton_dir;
use crate::launch::LaunchCommand;

fn steam_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = [".local/share/Steam", ".steam/root", ".steam/steam", ".var/app/com.valvesoftware.Steam/data/Steam", ".var/app/com.valvesoftware.Steam/.local/share/Steam", "snap/steam/common/.local/share/Steam"]
        .iter()
        .filter_map(|r| std::fs::canonicalize(home.join(r)).ok())
        .collect();
    roots.sort();
    roots.dedup();
    roots
}

fn proton_dirs_in(dir: &Path, filter: impl Fn(&str) -> bool) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .filter_map(Result::ok)
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .filter(|(name, path)| filter(name) && is_proton_dir(path))
        .collect()
}

/// `getInstalledProtonVersions`: `ProtonVersion[]` with the newest version first.
pub fn installed_versions() -> Vec<Value> {
    let mut found: Vec<(String, PathBuf, &str)> = Vec::new();
    let tool_dirs = steam_roots(&home_dir()).into_iter().map(|r| r.join("compatibilitytools.d")).chain([PathBuf::from("/usr/share/steam/compatibilitytools.d")]);
    for dir in tool_dirs {
        found.extend(proton_dirs_in(&dir, |_| true).into_iter().map(|(n, p)| (n, p, "compatibility_tools")));
    }
    // Steam can install Proton into any of its libraries, not only its own folder.
    let mut libraries: Vec<PathBuf> = steam_roots(&home_dir()).iter().flat_map(|root| crate::scan::library_folders(root)).filter_map(|p| std::fs::canonicalize(p).ok()).collect();
    libraries.sort();
    libraries.dedup();
    for library in libraries {
        found.extend(proton_dirs_in(&library.join("steamapps/common"), |n| n.starts_with("Proton")).into_iter().map(|(n, p)| (n, p, "steam")));
    }
    found.sort_by(|a, b| natural_cmp(&b.0, &a.0));
    found.dedup_by(|a, b| a.1 == b.1);
    found.into_iter().map(|(name, path, source)| json!({ "name": name, "path": path.to_string_lossy(), "source": source })).collect()
}

/// Compares digit runs as numbers, so GE-Proton10-4 sorts above GE-Proton9-27.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    fn parts(s: &str) -> Vec<Result<u64, String>> {
        let mut out = Vec::new();
        let mut chars = s.chars().peekable();
        while let Some(&c) = chars.peek() {
            let digit = c.is_ascii_digit();
            let mut run = String::new();
            while let Some(&c) = chars.peek().filter(|c| c.is_ascii_digit() == digit) {
                run.push(c);
                chars.next();
            }
            out.push(if digit { Ok(run.parse().unwrap_or(u64::MAX)) } else { Err(run.to_lowercase()) });
        }
        out
    }
    parts(a).cmp(&parts(b))
}

/// The newest GE-Proton, else Proton Experimental, else the newest of any other build.
fn default_version(versions: &[Value]) -> Option<&Value> {
    let named = |pred: &dyn Fn(&str) -> bool| versions.iter().find(|v| v["name"].as_str().is_some_and(pred));
    named(&|n| n.starts_with("GE-Proton")).or_else(|| named(&|n| n == "Proton - Experimental")).or(versions.first())
}

fn valid_dir(value: &Value) -> Option<PathBuf> {
    value.as_str().filter(|p| !p.is_empty()).map(PathBuf::from).filter(|p| is_proton_dir(p))
}

/// The game's own Proton, then the default from settings, then [`default_version`].
pub fn resolve(game: &Value, prefs: &Value, versions: &[Value]) -> Option<PathBuf> {
    valid_dir(&game["protonPath"])
        .or_else(|| valid_dir(&prefs["defaultProtonPath"]))
        .or_else(|| default_version(versions).and_then(|v| v["path"].as_str()).map(PathBuf::from))
}

/// `getDefaultWinePrefixSelectionPath`: the folder that holds the per-game prefixes.
pub fn default_prefix_root(prefs: &Value) -> PathBuf {
    prefs["defaultWinePrefixPath"].as_str().filter(|p| !p.is_empty()).map(PathBuf::from).unwrap_or_else(|| home_dir().join("Games/Chimaera/prefixes"))
}

/// The game's chosen prefix, else its own folder under [`default_prefix_root`].
pub fn prefix_for(game: &Value, prefs: &Value, key: &str) -> PathBuf {
    if let Some(own) = game["winePrefixPath"].as_str().filter(|p| !p.is_empty()) {
        return PathBuf::from(own);
    }
    let folder: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    default_prefix_root(prefs).join(folder)
}

/// A game Steam installed itself, so Steam's own Proton choice should run it.
pub fn is_steam_install(exe: &str) -> bool {
    exe.contains("/steamapps/common/")
}

/// The AppImage points PYTHONHOME, LD_LIBRARY_PATH, GTK and GStreamer paths at its own
/// bundle, which breaks host tools such as umu-run (Python) and wine. Returns the
/// variables to change for a child: `None` removes one, `Some` sets it with the bundle's
/// paths taken out.
pub fn host_env_fixes(vars: impl IntoIterator<Item = (String, String)>, appdir: Option<&str>) -> Vec<(String, Option<String>)> {
    let Some(appdir) = appdir.filter(|d| !d.is_empty()) else { return Vec::new() };
    let mut fixes = Vec::new();
    for (key, value) in vars {
        if matches!(key.as_str(), "APPDIR" | "APPIMAGE" | "ARGV0" | "OWD" | "PYTHONDONTWRITEBYTECODE") {
            fixes.push((key, None));
        } else if value.contains(appdir) {
            let kept: Vec<&str> = value.split(':').filter(|part| !part.is_empty() && !part.contains(appdir)).collect();
            fixes.push((key, if kept.is_empty() { None } else { Some(kept.join(":")) }));
        }
    }
    fixes
}

/// Gives a host tool the user's environment instead of the AppImage's.
pub fn use_host_env(command: &mut std::process::Command) -> &mut std::process::Command {
    let appdir = std::env::var("APPDIR").ok();
    for (key, value) in host_env_fixes(std::env::vars(), appdir.as_deref()) {
        match value {
            Some(v) => command.env(key, v),
            None => command.env_remove(key),
        };
    }
    command
}

/// getWinePrefixWindowsUserProfilePath: `USERPROFILE` from user.reg's Volatile Environment, as `C:/users/<name>`.
pub fn windows_user_profile(user_reg: &str) -> Option<String> {
    let mut in_section = false;
    for line in user_reg.lines() {
        if line.starts_with('[') {
            in_section = line.starts_with("[Volatile Environment]");
        } else if let Some(value) = line.strip_prefix("\"USERPROFILE\"=\"").filter(|_| in_section) {
            let raw = value.strip_suffix('"')?;
            let mut path = String::new();
            for c in raw.chars() {
                // Backslashes are escaped in .reg files; any run of them is one separator.
                if c == '\\' { if !path.ends_with('/') { path.push('/') } } else { path.push(c) }
            }
            let path = path.trim_end_matches('/');
            return (!path.trim().is_empty()).then(|| path.to_string());
        }
    }
    None
}

/// cleanupStaleCompatibilityProcesses: a Wine process, an .exe or wineserver left in `prefix`.
pub fn is_stale_compat_process(name: &str, exe: Option<&str>, compat_data: Option<&str>, prefix: &str) -> bool {
    let name = name.to_lowercase();
    compat_data == Some(prefix) && (exe.is_some_and(|e| e.to_lowercase().contains("wine")) || name.ends_with(".exe") || name == "wineserver")
}

/// Kills what an earlier run left in the game's own prefix, so a stuck wineserver cannot block the launch.
pub fn kill_stale_prefix_processes(prefix: &Path) {
    let prefix = std::fs::canonicalize(prefix).unwrap_or_else(|_| prefix.to_path_buf());
    let prefix = prefix.to_string_lossy();
    let mut system = sysinfo::System::new();
    let kind = sysinfo::ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::Always).with_environ(sysinfo::UpdateKind::Always);
    system.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, true, kind);
    for process in system.processes().values() {
        let compat = process.environ().iter().filter_map(|e| e.to_str()).find_map(|e| e.strip_prefix("STEAM_COMPAT_DATA_PATH="));
        let compat = compat.map(|c| std::fs::canonicalize(c).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| c.to_string()));
        let exe = process.exe().map(|e| e.to_string_lossy().into_owned());
        if is_stale_compat_process(&process.name().to_string_lossy(), exe.as_deref(), compat.as_deref(), &prefix) {
            eprintln!("[launch] killing stale compatibility process {} in {prefix}", process.pid());
            let _ = process.kill_with(sysinfo::Signal::Kill);
        }
    }
}

pub fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(bin).is_file()))
}

/// Puts `umu-run` in front of the game, inside any `%command%` wrapper the user set,
/// plus gamemoderun when the game asks for it.
pub fn umu_command(umu: &[String], launch: LaunchCommand, exe: &str, proton: Option<&Path>, prefix: &Path, steam_app_id: Option<&str>, gamemode: bool, mangohud: bool) -> LaunchCommand {
    let mut argv: Vec<String> = std::iter::once(launch.command).chain(launch.args).collect();
    let at = argv.iter().position(|a| a == exe).unwrap_or(0);
    argv.splice(at..at, umu.iter().cloned());
    if gamemode {
        argv.insert(0, "gamemoderun".into());
    }
    // PROTON_LOG as upstream: Proton writes ~/steam-<id>.log for bug reports.
    let mut env = vec![("PROTON_LOG".to_string(), "1".to_string()), ("WINEPREFIX".to_string(), prefix.to_string_lossy().into_owned())];
    if let Some(proton) = proton {
        // Without PROTONPATH, umu-run fetches and uses UMU-Proton, as upstream relies on.
        env.push(("PROTONPATH".into(), proton.to_string_lossy().into_owned()));
    }
    if let Some(id) = steam_app_id {
        // umu-database keys protonfixes by Steam app id.
        env.push(("GAMEID".into(), format!("umu-{id}")));
    }
    if mangohud {
        env.push(("MANGOHUD".into(), "1".into()));
    }
    env.extend(launch.env);
    let command = argv.remove(0);
    LaunchCommand { command, args: argv, env }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_user_profile_from_user_reg() {
        let reg = "[Software\\\\Wine] 1\n\"USERPROFILE\"=\"C:\\\\wrong\"\n\n[Volatile Environment] 1794494193\n#time=1\n\"HOMEDRIVE\"=\"C:\"\n\"USERPROFILE\"=\"C:\\\\users\\\\steamuser\"\n";
        assert_eq!(windows_user_profile(reg).as_deref(), Some("C:/users/steamuser"));
        assert_eq!(windows_user_profile("[Volatile Environment]\n\"USERPROFILE\"=\"\"\n"), None);
        assert_eq!(windows_user_profile(""), None);
    }

    #[test]
    fn stale_process_needs_the_prefix_and_a_wine_name() {
        assert!(is_stale_compat_process("wineserver", None, Some("/p"), "/p"));
        assert!(is_stale_compat_process("Game.EXE", None, Some("/p"), "/p"));
        assert!(is_stale_compat_process("x", Some("/opt/proton/files/bin/wine64-preloader"), Some("/p"), "/p"));
        assert!(!is_stale_compat_process("wineserver", None, Some("/other"), "/p"));
        assert!(!is_stale_compat_process("bash", Some("/usr/bin/bash"), Some("/p"), "/p"));
    }

    #[test]
    fn strips_appimage_env() {
        let vars = [("PYTHONHOME", "/tmp/.mount_x/usr/"), ("PATH", "/tmp/.mount_x/usr/bin:/usr/bin"), ("HOME", "/home/u"), ("APPIMAGE", "/a.appimage"), ("LD_LIBRARY_PATH", "/tmp/.mount_x/usr/lib/:/tmp/.mount_x/lib")]
            .map(|(k, v)| (k.to_string(), v.to_string()));
        let fixes = host_env_fixes(vars.clone(), Some("/tmp/.mount_x"));
        assert_eq!(fixes, [("PYTHONHOME".into(), None), ("PATH".into(), Some("/usr/bin".into())), ("APPIMAGE".into(), None), ("LD_LIBRARY_PATH".into(), None)]);
        assert!(host_env_fixes(vars, None).is_empty());
    }

    #[test]
    fn sorts_versions_newest_first() {
        let mut names = vec!["GE-Proton9-27", "GE-Proton10-4", "GE-Proton10-25", "Proton 9.0"];
        names.sort_by(|a, b| natural_cmp(b, a));
        assert_eq!(names, ["Proton 9.0", "GE-Proton10-25", "GE-Proton10-4", "GE-Proton9-27"]);
    }

    #[test]
    fn picks_ge_then_experimental() {
        let v = |n: &str| json!({ "name": n, "path": format!("/p/{n}") });
        assert_eq!(default_version(&[v("Proton 9.0"), v("GE-Proton10-4"), v("Proton - Experimental")]).unwrap()["name"], "GE-Proton10-4");
        assert_eq!(default_version(&[v("Proton 9.0"), v("Proton - Experimental")]).unwrap()["name"], "Proton - Experimental");
        assert_eq!(default_version(&[v("Proton 9.0")]).unwrap()["name"], "Proton 9.0");
        assert!(default_version(&[]).is_none());
    }

    #[test]
    fn prefix_is_per_game_unless_chosen() {
        let prefs = json!({ "defaultWinePrefixPath": "/games/pfx" });
        assert_eq!(prefix_for(&json!({}), &prefs, "custom:a/b c"), PathBuf::from("/games/pfx/custom_a_b_c"));
        assert_eq!(prefix_for(&json!({ "winePrefixPath": "/mine" }), &prefs, "k"), PathBuf::from("/mine"));
    }

    #[test]
    fn wraps_the_exe_with_umu_inside_user_wrappers() {
        let launch = LaunchCommand { command: "gamescope".into(), args: vec!["-f".into(), "--".into(), "/g/a.exe".into(), "-x".into()], env: vec![("A".into(), "1".into())] };
        let cmd = umu_command(&["umu-run".into()], launch, "/g/a.exe", Some(Path::new("/p/GE")), Path::new("/pfx"), Some("105600"), true, false);
        assert_eq!(cmd.command, "gamemoderun");
        assert_eq!(cmd.args, ["gamescope", "-f", "--", "umu-run", "/g/a.exe", "-x"]);
        assert_eq!(cmd.env, [("PROTON_LOG".into(), "1".into()), ("WINEPREFIX".into(), "/pfx".into()), ("PROTONPATH".into(), "/p/GE".into()), ("GAMEID".into(), "umu-105600".into()), ("A".to_string(), "1".to_string())]);
        let plain = umu_command(&["python3".into(), "/d/umu-run".into()], LaunchCommand { command: "/g/a.exe".into(), args: vec![], env: vec![] }, "/g/a.exe", None, Path::new("/x"), None, false, true);
        assert_eq!((plain.command.as_str(), plain.args.as_slice()), ("python3", ["/d/umu-run".to_string(), "/g/a.exe".to_string()].as_slice()));
        assert!(plain.env.contains(&("MANGOHUD".into(), "1".into())));
    }

    #[test]
    fn spots_steam_installs() {
        assert!(is_steam_install("/home/u/.local/share/Steam/steamapps/common/Terraria/Terraria.exe"));
        assert!(!is_steam_install("/home/u/Games/Repack/game.exe"));
    }
}
