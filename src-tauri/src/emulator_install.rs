//! Emulator installs (port of `emulator-install-sources.ts`, `dolphin-install-source.ts`,
//! `emulator-installer.ts`, `find-managed-emulator-executable.ts`). Options come from
//! GitHub releases (Dolphin from its tags); `installEmulator` re-resolves the option
//! id here, so the renderer never supplies the URL.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use regex::Regex;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_opener::OpenerExt;

use crate::emulators::{self, Known};
use crate::store::Store;

const GITHUB_API: &str = "https://api.github.com";
const DOLPHIN_DOWNLOADS_PAGE: &str = "https://dolphin-emu.org/download/";

enum Channel {
    Rolling,
    Latest,
    Prerelease,
}

enum Source {
    Github { id: &'static str, repo: &'static str, channel: Channel, label: Option<&'static str>, pattern: &'static str, kind: &'static str },
    Link { id: &'static str, link_kind: &'static str, url: &'static str },
}

fn system_of(binary: &str) -> Option<&'static Known> {
    emulators::KNOWN.iter().find(|k| k.binary == binary)
}

/// The GitHub and link entries for this platform (Dolphin is resolved separately).
fn sources(binary: &str, windows: bool, arm: bool) -> Vec<Source> {
    use Source::*;
    match binary {
        "duckstation" if windows => vec![Github {
            id: "duckstation-install",
            repo: "stenzek/duckstation",
            channel: Channel::Rolling,
            label: None,
            pattern: if arm { r"(?i)^duckstation-windows-arm64-installer\.exe$" } else { r"(?i)^duckstation-windows-x64-installer\.exe$" },
            kind: "windows-installer",
        }],
        "duckstation" => vec![
            Github {
                id: "duckstation-install",
                repo: "stenzek/duckstation",
                channel: Channel::Rolling,
                label: None,
                pattern: if arm { r"(?i)^DuckStation-arm64\.AppImage$" } else { r"(?i)^DuckStation-x64\.AppImage$" },
                kind: "linux-appimage",
            },
            Link { id: "duckstation-aur", link_kind: "aur", url: "https://aur.archlinux.org/packages/duckstation-git" },
        ],
        "pcsx2" => {
            let mut out = Vec::new();
            if windows || !arm {
                let pattern = match (windows, arm) {
                    (false, _) => r"(?i)linux-appimage-x64-Qt\.AppImage$",
                    (true, true) => r"(?i)windows-arm64-installer\.exe$",
                    (true, false) => r"(?i)windows-x64-installer\.exe$",
                };
                let kind = if windows { "windows-installer" } else { "linux-appimage" };
                out.push(Github { id: "pcsx2-release", repo: "PCSX2/pcsx2", channel: Channel::Latest, label: Some("release"), pattern, kind });
                out.push(Github { id: "pcsx2-prerelease", repo: "PCSX2/pcsx2", channel: Channel::Prerelease, label: Some("prerelease"), pattern, kind });
            }
            if !windows {
                out.push(Link { id: "pcsx2-aur", link_kind: "aur", url: "https://aur.archlinux.org/packages/pcsx2" });
                out.push(Link { id: "pcsx2-flatpak", link_kind: "flatpak", url: "https://flathub.org/en/apps/net.pcsx2.PCSX2" });
            }
            out
        }
        "rpcs3" if windows => vec![Github {
            id: "rpcs3-install",
            repo: "RPCS3/rpcs3-binaries-win",
            channel: Channel::Latest,
            label: None,
            // Builds are now named `_win64_msvc.7z`; Electron's `_win64\.7z$` misses them.
            pattern: r"(?i)_win64(_msvc)?\.7z$",
            kind: "windows-archive",
        }],
        "rpcs3" => {
            let mut out = Vec::new();
            if !arm {
                out.push(Github {
                    id: "rpcs3-install",
                    repo: "RPCS3/rpcs3-binaries-linux",
                    channel: Channel::Latest,
                    label: None,
                    pattern: r"(?i)_linux64\.AppImage$",
                    kind: "linux-appimage",
                });
            }
            out.push(Link { id: "rpcs3-aur", link_kind: "aur", url: "https://aur.archlinux.org/packages/rpcs3-git" });
            out.push(Link { id: "rpcs3-flatpak", link_kind: "flatpak", url: "https://flathub.org/en/apps/net.rpcs3.RPCS3" });
            out
        }
        "ppsspp" if windows => vec![Github {
            id: "ppsspp-install",
            repo: "hrydgard/PPSSPP",
            channel: Channel::Latest,
            label: None,
            pattern: if arm { r"(?i)^PPSSPP-v.*-Windows-ARM64\.zip$" } else { r"(?i)^PPSSPP-v.*-Windows-x64\.zip$" },
            kind: "portable-archive",
        }],
        "ppsspp" => vec![
            Github {
                id: "ppsspp-install",
                repo: "hrydgard/PPSSPP",
                channel: Channel::Latest,
                label: None,
                pattern: if arm { r"(?i)^PPSSPP-v.*-anylinux-aarch64\.AppImage$" } else { r"(?i)^PPSSPP-v.*-anylinux-x86_64\.AppImage$" },
                kind: "linux-appimage",
            },
            Link { id: "ppsspp-flatpak", link_kind: "flatpak", url: "https://flathub.org/apps/org.ppsspp.PPSSPP" },
        ],
        _ => Vec::new(),
    }
}

fn link_option(id: &str, binary: &str, channel: Option<&str>, version: Option<&str>, html: Option<&str>, url: &str, link_kind: &str) -> Value {
    json!({
        "id": id, "binary": binary, "kind": "link", "channel": channel, "downloadUrl": null, "fileName": null,
        "version": version, "htmlUrl": html, "linkUrl": url, "linkKind": link_kind,
    })
}

async fn github_json(url: &str) -> Result<Value, String> {
    reqwest::Client::builder()
        .user_agent("Chimaera")
        .build()
        .map_err(|e| e.to_string())?
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())
}

async fn fetch_release(repo: &str, channel: &Channel) -> Option<Value> {
    let result = match channel {
        Channel::Rolling => github_json(&format!("{GITHUB_API}/repos/{repo}/releases/tags/latest")).await,
        Channel::Latest => github_json(&format!("{GITHUB_API}/repos/{repo}/releases/latest")).await,
        Channel::Prerelease => github_json(&format!("{GITHUB_API}/repos/{repo}/releases?per_page=20")).await.map(|list| {
            list.as_array()
                .and_then(|l| l.iter().find(|r| r["prerelease"] == true && r["draft"] != true).cloned())
                .unwrap_or(Value::Null)
        }),
    };
    match result {
        Ok(release) if release.is_object() => Some(release),
        Ok(_) => None,
        Err(e) => {
            eprintln!("[emulator-install] {repo} release lookup failed: {e}");
            None
        }
    }
}

/// The option for a resolved release: the matching asset, else its release page.
fn github_option(binary: &str, id: &str, label: Option<&str>, kind: &str, pattern: &str, repo: &str, release: Option<&Value>) -> Value {
    let Some(release) = release else {
        return link_option(id, binary, label, None, None, &format!("https://github.com/{repo}/releases"), "release_page");
    };
    let (tag, html) = (release["tag_name"].as_str(), release["html_url"].as_str());
    let re = Regex::new(pattern).expect("static pattern");
    let asset = release["assets"].as_array().and_then(|a| a.iter().find(|a| a["name"].as_str().is_some_and(|n| re.is_match(n))));
    match asset {
        Some(asset) => json!({
            "id": id, "binary": binary, "kind": kind, "channel": label, "downloadUrl": asset["browser_download_url"],
            "fileName": asset["name"], "version": tag, "htmlUrl": html, "linkUrl": null, "linkKind": null,
        }),
        None => link_option(id, binary, label, tag, html, html.unwrap_or_default(), "release_page"),
    }
}

/// pickLatestDolphinReleaseTag: the highest `NNNN` or `NNNNa` tag.
fn latest_dolphin_tag<'a>(tags: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let re = Regex::new(r"(?i)^(\d{4})([a-z]?)$").expect("static pattern");
    tags.filter_map(|t| {
        let c = re.captures(t)?;
        Some((c[1].parse::<u32>().ok()?, c[2].to_lowercase(), t))
    })
    .max_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)))
    .map(|(_, _, t)| t)
}

async fn dolphin_options(windows: bool, arm: bool) -> Vec<Value> {
    if !windows {
        return vec![link_option("dolphin-flatpak", "dolphin", None, None, None, "https://flatpak.dolphin-emu.org/releases.flatpakrepo", "flatpak")];
    }
    let tags = github_json(&format!("{GITHUB_API}/repos/dolphin-emu/dolphin/tags?per_page=30")).await;
    let version = tags.ok().and_then(|t| t.as_array().and_then(|a| latest_dolphin_tag(a.iter().filter_map(|t| t["name"].as_str())).map(str::to_string)));
    match version {
        Some(version) => {
            let file = format!("dolphin-{version}-{}.7z", if arm { "arm64" } else { "x64" });
            vec![json!({
                "id": "dolphin-install", "binary": "dolphin", "kind": "portable-archive", "channel": "release",
                "downloadUrl": format!("https://dl.dolphin-emu.org/releases/{version}/{file}"), "fileName": file, "version": version,
                "htmlUrl": format!("{DOLPHIN_DOWNLOADS_PAGE}release/{version}/"), "linkUrl": null, "linkKind": null,
            })]
        }
        None => vec![link_option("dolphin-releases", "dolphin", None, None, None, DOLPHIN_DOWNLOADS_PAGE, "release_page")],
    }
}

/// resolveEmulatorInstallOptions. macOS has no build here, so it is not offered.
pub async fn options(binary: &str) -> Vec<Value> {
    if system_of(binary).is_none() || !(cfg!(windows) || cfg!(target_os = "linux")) {
        return Vec::new();
    }
    let (windows, arm) = (cfg!(windows), cfg!(target_arch = "aarch64"));
    if binary == "dolphin" {
        return dolphin_options(windows, arm).await;
    }
    let mut out = Vec::new();
    for source in sources(binary, windows, arm) {
        out.push(match source {
            Source::Link { id, link_kind, url } => link_option(id, binary, None, None, None, url, link_kind),
            Source::Github { id, repo, channel, label, pattern, kind } => {
                let release = fetch_release(repo, &channel).await;
                github_option(binary, id, label, kind, pattern, repo, release.as_ref())
            }
        });
    }
    out
}

/// findManagedEmulatorExecutable: the first known executable name under `root`.
fn find_executable(root: &Path, k: &Known) -> Option<PathBuf> {
    let names: Vec<String> = k.linux_names.iter().chain(k.windows_names).map(|n| n.to_lowercase()).collect();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() && names.contains(&entry.file_name().to_string_lossy().to_lowercase()) {
                return Some(entry.path());
            }
        }
    }
    None
}

fn progress(app: &AppHandle, binary: &str, option_id: &str, phase: &str, extra: Value) {
    let mut payload = json!({ "binary": binary, "optionId": option_id, "phase": phase });
    if let (Some(p), Some(e)) = (payload.as_object_mut(), extra.as_object()) {
        p.extend(e.clone());
    }
    let _ = app.emit("on-emulator-install-progress", [payload]);
}

async fn download_to(app: &AppHandle, url: &str, dest: &Path, binary: &str, option_id: &str) -> Result<(), String> {
    let mut response = reqwest::Client::builder()
        .user_agent("Chimaera")
        .build()
        .map_err(|e| e.to_string())?
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?;
    let total = response.content_length();
    let mut file = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let (mut loaded, mut last) = (0u64, 0u64);
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        loaded += chunk.len() as u64;
        // A progress event per chunk floods the webview; one per MiB is plenty.
        if loaded - last >= 1 << 20 {
            last = loaded;
            progress(app, binary, option_id, "downloading", json!({ "loaded": loaded, "total": total }));
        }
    }
    file.flush().map_err(|e| e.to_string())
}

fn spawn_detached(path: &Path) -> bool {
    let mut command = std::process::Command::new(path);
    command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    command.spawn().is_ok()
}

async fn run_install(app: &AppHandle, binary: &str, k: &Known, option: &Value, option_id: &str, dest: &Path, install_dir: &Path) -> Result<Value, String> {
    let kind = option["kind"].as_str().unwrap_or_default();
    if kind == "linux-appimage" {
        std::fs::create_dir_all(install_dir).map_err(|e| e.to_string())?;
    }
    progress(app, binary, option_id, "downloading", json!({ "loaded": 0 }));
    download_to(app, option["downloadUrl"].as_str().unwrap_or_default(), dest, binary, option_id).await?;

    match kind {
        "windows-installer" => {
            progress(app, binary, option_id, "running", json!({}));
            let ok = spawn_detached(dest) || app.opener().open_path(dest.to_string_lossy(), None::<&str>).is_ok();
            progress(app, binary, option_id, if ok { "done" } else { "error" }, json!({ "path": dest, "reason": (!ok).then_some("launch_failed") }));
            // Usually locked by the running installer; Electron ignores that too.
            let _ = std::fs::remove_file(dest);
            Ok(if ok { json!({ "ok": true, "path": dest }) } else { json!({ "ok": false, "reason": "launch_failed" }) })
        }
        "linux-appimage" => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(dest).map_err(|e| e.to_string())?.permissions().mode();
                std::fs::set_permissions(dest, std::fs::Permissions::from_mode(mode | 0o100)).map_err(|e| e.to_string())?;
            }
            if !emulators::is_valid_executable(dest) {
                return Err("downloaded AppImage is not executable".into());
            }
            let _ = app.opener().reveal_item_in_dir(dest);
            progress(app, binary, option_id, "done", json!({ "path": dest }));
            Ok(json!({ "ok": true, "path": dest }))
        }
        _ => {
            progress(app, binary, option_id, "extracting", json!({}));
            std::fs::create_dir_all(install_dir).map_err(|e| e.to_string())?;
            let seven_zip = crate::extraction::seven_zip(app)?;
            let (archive, out) = (dest.to_path_buf(), install_dir.to_path_buf());
            tauri::async_runtime::spawn_blocking(move || crate::extraction::run_7z(&seven_zip, &archive, &out, &mut |_| {}))
                .await
                .map_err(|e| e.to_string())??;
            let executable = find_executable(install_dir, k)
                .filter(|e| emulators::is_valid_executable(e))
                .ok_or_else(|| format!("No {} executable found in archive", k.display_name))?;
            let _ = std::fs::remove_file(dest);
            let _ = app.opener().reveal_item_in_dir(&executable);
            progress(app, binary, option_id, "done", json!({ "path": install_dir }));
            Ok(json!({ "ok": true, "path": install_dir }))
        }
    }
}

/// downloadAndInstallEmulator.
pub async fn install(app: &AppHandle, binary: &str, option_id: &str) -> Value {
    let Some(k) = system_of(binary) else { return json!({ "ok": false, "reason": "invalid_binary" }) };
    let options = options(binary).await;
    let Some(option) = options.iter().find(|o| o["id"] == option_id && o["kind"] != "link" && o["downloadUrl"].is_string()) else {
        return json!({ "ok": false, "reason": "option_not_installable" });
    };
    let default_downloads = app.path().download_dir().unwrap_or_default();
    let install_dir = crate::installer::downloads_path(&app.state::<Store>(), &default_downloads).join(k.display_name);
    let url = option["downloadUrl"].as_str().unwrap_or_default();
    let name = option["fileName"].as_str().unwrap_or(url);
    // basename: the file name never picks the folder it lands in.
    let name = Path::new(name).file_name().map(|n| n.to_os_string()).unwrap_or_else(|| "emulator-download".into());
    let dest = if option["kind"] == "linux-appimage" { install_dir.join(&name) } else { std::env::temp_dir().join(&name) };

    match run_install(app, binary, k, option, option_id, &dest, &install_dir).await {
        Ok(result) => result,
        Err(e) => {
            eprintln!("[emulator-install] {binary} install failed: {e}");
            let _ = std::fs::remove_file(&dest);
            progress(app, binary, option_id, "error", json!({ "reason": "install_failed" }));
            json!({ "ok": false, "reason": "install_failed" })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_newest_dolphin_release_tag() {
        let tags = ["2412", "2503a", "5.0-19870", "2503", "2409b", "beta"];
        assert_eq!(latest_dolphin_tag(tags.iter().copied()), Some("2503a"));
        assert_eq!(latest_dolphin_tag(["5.0"].iter().copied()), None);
    }

    #[test]
    fn options_match_assets_or_fall_back_to_the_release_page() {
        let release = json!({
            "tag_name": "v2.2.0", "html_url": "https://github.com/PCSX2/pcsx2/releases/tag/v2.2.0",
            "assets": [{ "name": "pcsx2-v2.2.0-windows-x64-installer.exe", "browser_download_url": "https://x/installer.exe" }],
        });
        let pcsx2 = sources("pcsx2", true, false);
        let [Source::Github { id, repo, label, pattern, kind, .. }, ..] = pcsx2.as_slice() else { panic!() };
        let option = github_option("pcsx2", id, *label, kind, pattern, repo, Some(&release));
        assert_eq!(option["kind"], "windows-installer");
        assert_eq!(option["downloadUrl"], "https://x/installer.exe");
        assert_eq!(option["channel"], "release");

        let option = github_option("pcsx2", id, *label, kind, r"(?i)arm64", repo, Some(&release));
        assert_eq!((option["kind"].as_str(), option["linkKind"].as_str()), (Some("link"), Some("release_page")));
        let option = github_option("pcsx2", id, *label, kind, pattern, repo, None);
        assert_eq!(option["linkUrl"], "https://github.com/PCSX2/pcsx2/releases");
    }

    #[test]
    fn rpcs3_pattern_matches_msvc_builds() {
        let rpcs3 = sources("rpcs3", true, false);
        let [Source::Github { pattern, .. }] = rpcs3.as_slice() else { panic!() };
        let re = Regex::new(pattern).unwrap();
        assert!(re.is_match("rpcs3-v0.0.43-20549-d0f66e3f_win64_msvc.7z") && re.is_match("rpcs3-v0.0.30-1_win64.7z"));
        assert!(!re.is_match("rpcs3-v0.0.43-20549-d0f66e3f_win64_msvc.7z.sha256"));
    }

    #[test]
    fn linux_arm_gets_only_links_for_pcsx2_and_rpcs3() {
        assert!(sources("pcsx2", false, true).iter().all(|s| matches!(s, Source::Link { .. })));
        assert!(sources("rpcs3", false, true).iter().all(|s| matches!(s, Source::Link { .. })));
        assert!(sources("unknown", true, false).is_empty());
    }

    #[test]
    fn finds_a_nested_executable() {
        let root = std::env::temp_dir().join(format!("chimaera-emu-install-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("rpcs3-v0.0.1").join("bin")).unwrap();
        std::fs::write(root.join("rpcs3-v0.0.1").join("bin").join("RPCS3.EXE"), b"MZ").unwrap();
        let k = emulators::known("ps3").unwrap();
        assert!(find_executable(&root, k).unwrap().ends_with("RPCS3.EXE"));
        assert!(find_executable(&root, emulators::known("ps2").unwrap()).is_none());
    }
}
