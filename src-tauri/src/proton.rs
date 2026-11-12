//! Linux Proton through umu-run: the port of src/main/services/umu.ts and wine.ts.
//! Proton builds come from Steam (`steamapps/common/Proton*`) and from compatibility tools
//! (`compatibilitytools.d`, where ProtonPlus installs GE-Proton).

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::classics_import::home_dir;
use crate::games::is_proton_dir;
use crate::launch::LaunchCommand;

fn steam_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = [".local/share/Steam", ".steam/root", ".steam/steam", ".var/app/com.valvesoftware.Steam/data/Steam"]
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
    for root in steam_roots(&home_dir()) {
        found.extend(proton_dirs_in(&root.join("steamapps/common"), |n| n.starts_with("Proton")).into_iter().map(|(n, p)| (n, p, "steam")));
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

pub fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(bin).is_file()))
}

/// Puts `umu-run` in front of the game, inside any `%command%` wrapper the user set,
/// plus gamemoderun when the game asks for it.
pub fn umu_command(launch: LaunchCommand, exe: &str, proton: &Path, prefix: &Path, steam_app_id: Option<&str>, gamemode: bool, mangohud: bool) -> LaunchCommand {
    let mut argv: Vec<String> = std::iter::once(launch.command).chain(launch.args).collect();
    let at = argv.iter().position(|a| a == exe).unwrap_or(0);
    argv.insert(at, "umu-run".into());
    if gamemode {
        argv.insert(0, "gamemoderun".into());
    }
    let mut env = vec![("WINEPREFIX".to_string(), prefix.to_string_lossy().into_owned()), ("PROTONPATH".to_string(), proton.to_string_lossy().into_owned())];
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
        let cmd = umu_command(launch, "/g/a.exe", Path::new("/p/GE"), Path::new("/pfx"), Some("105600"), true, false);
        assert_eq!(cmd.command, "gamemoderun");
        assert_eq!(cmd.args, ["gamescope", "-f", "--", "umu-run", "/g/a.exe", "-x"]);
        assert_eq!(cmd.env, [("WINEPREFIX".into(), "/pfx".into()), ("PROTONPATH".into(), "/p/GE".into()), ("GAMEID".into(), "umu-105600".into()), ("A".to_string(), "1".to_string())]);
        let plain = umu_command(LaunchCommand { command: "/g/a.exe".into(), args: vec![], env: vec![] }, "/g/a.exe", Path::new("/p"), Path::new("/x"), None, false, true);
        assert_eq!((plain.command.as_str(), plain.args.as_slice()), ("umu-run", ["/g/a.exe".to_string()].as_slice()));
        assert!(plain.env.contains(&("MANGOHUD".into(), "1".into())));
    }

    #[test]
    fn spots_steam_installs() {
        assert!(is_steam_install("/home/u/.local/share/Steam/steamapps/common/Terraria/Terraria.exe"));
        assert!(!is_steam_install("/home/u/Games/Repack/game.exe"));
    }
}
