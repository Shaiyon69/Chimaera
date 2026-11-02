//! DXVK per game: swaps a DX9-11 game's graphics DLLs for DXVK (official or
//! gplasync), with backup and rollback. Windows only; Proton already ships DXVK.
//! Never in the same folder as OptiScaler: both own `dxgi.dll`.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::AppHandle;
use tokio::sync::Mutex;

use crate::optiscaler::{context, detect_anticheat, detect_api, download, game_root_from_exe_dir, github_json, inside, natural_cmp};

const MARKER: &str = "dxvk_manager.json";
const BACKUP: &str = ".dxvk_backup";
const CONF: &str = "dxvk.conf";
const BUILDS: &[&str] = &["dxvk", "gplasync"];
const OFFICIAL_API: &str = "https://api.github.com/repos/doitsujin/dxvk/releases/latest";
const OFFICIAL_RELEASES: &str = "https://api.github.com/repos/doitsujin/dxvk/releases";
const GPLASYNC_RELEASES: &str = "https://gitlab.com/api/v4/projects/Ph42oN%2Fdxvk-gplasync/releases";
const GPLASYNC_API: &str = "https://gitlab.com/api/v4/projects/Ph42oN%2Fdxvk-gplasync/releases?per_page=1";
const TAG_TTL: Duration = Duration::from_secs(60 * 60);

// ---------- detection ----------

/// 32 or 64 from the PE header's Machine field; None if not a PE x86/x64 exe.
fn pe_bits(exe: &Path) -> Option<u32> {
    let mut f = fs::File::open(exe).ok()?;
    let mut dos = [0u8; 0x40];
    f.read_exact(&mut dos).ok()?;
    if &dos[..2] != b"MZ" {
        return None;
    }
    let pe = u32::from_le_bytes(dos[0x3c..0x40].try_into().ok()?);
    f.seek(SeekFrom::Start(pe.into())).ok()?;
    let mut head = [0u8; 6];
    f.read_exact(&mut head).ok()?;
    if &head[..4] != b"PE\0\0" {
        return None;
    }
    match u16::from_le_bytes([head[4], head[5]]) {
        0x8664 => Some(64),
        0x14c => Some(32),
        _ => None,
    }
}

/// DLLs DXVK replaces for an API; None when DXVK does not apply (dx12, vulkan, unknown).
fn files_for(api: &str) -> Option<&'static [&'static str]> {
    match api {
        "dx9" => Some(&["d3d9.dll"]),
        "dx10" | "dx11" => Some(&["d3d10core.dll", "d3d11.dll", "dxgi.dll"]),
        _ => None,
    }
}

/// Why install is refused, if it is: "optiscaler", "reshade", "anticheat" or "api".
fn blocked(exe: &Path, dir: &Path, api: &str) -> Option<&'static str> {
    if crate::optiscaler::read_marker(dir).is_some() {
        Some("optiscaler")
    } else if crate::graphics_scan::reshade_dll(dir).is_some_and(|r| files_for(api).is_some_and(|f| f.contains(&r.as_str()))) {
        // DXVK would move ReShade's DLL to backup and silently turn it off.
        Some("reshade")
    } else if detect_anticheat(&game_root_from_exe_dir(dir)).is_some() {
        // Swapping the game's own graphics DLLs trips anti-cheat; never offer it there.
        Some("anticheat")
    } else if files_for(api).is_none() || pe_bits(exe).is_none() {
        Some("api")
    } else {
        None
    }
}

// ---------- releases ----------

/// Tag and `.tar.gz` URL of a release (or the first in a list) from GitHub or GitLab.
fn release_asset(build: &str, data: &Value) -> Option<(String, String)> {
    let release = if data.is_array() { &data[0] } else { data };
    let (assets, url_key) = if build == "gplasync" { (&release["assets"]["links"], "direct_asset_url") } else { (&release["assets"], "browser_download_url") };
    let tag = release["tag_name"].as_str()?.to_string();
    // The official release also carries a Linux-only dxvk-native tarball.
    let asset = assets.as_array()?.iter().find(|a| a["name"].as_str().is_some_and(|n| n.ends_with(".tar.gz") && !n.contains("native")))?;
    Some((tag, asset[url_key].as_str()?.to_string()))
}

/// Newest tag and URL; None offline. Cached an hour (GitHub allows 60 calls/h).
async fn latest(build: &str) -> Option<(String, String)> {
    static CACHE: Mutex<Option<HashMap<String, (String, String, Instant)>>> = Mutex::const_new(None);
    let mut cache = CACHE.lock().await;
    let map = cache.get_or_insert_with(HashMap::new);
    if let Some((tag, url, _)) = map.get(build).filter(|(_, _, at)| at.elapsed() < TAG_TTL) {
        return Some((tag.clone(), url.clone()));
    }
    let data = github_json(if build == "gplasync" { GPLASYNC_API } else { OFFICIAL_API }, 15).await.ok()?;
    let (tag, url) = release_asset(build, &data)?;
    map.insert(build.to_string(), (tag.clone(), url.clone(), Instant::now()));
    Some((tag, url))
}

fn cache_root(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(crate::save_backups::data_dir(app, "dxvk")?.join("cache"))
}

/// A release folder is usable once both architectures are there.
fn complete(dir: &Path) -> bool {
    ["x64", "x32"].iter().all(|a| dir.join(a).join("d3d11.dll").is_file())
}

/// Unpacks a `.tar.gz` and returns the folder holding `x64/` and `x32/`.
/// tar's `unpack` skips entries that would land outside `dest`.
fn unpack(archive: &Path, dest: &Path) -> Result<PathBuf, String> {
    let file = fs::File::open(archive).map_err(|e| e.to_string())?;
    tar::Archive::new(flate2::read::GzDecoder::new(file)).unpack(dest).map_err(|e| e.to_string())?;
    if complete(dest) {
        return Ok(dest.to_path_buf());
    }
    fs::read_dir(dest).map_err(|e| e.to_string())?.flatten().map(|e| e.path()).find(|p| complete(p)).ok_or("x64/x32 DLLs not found in the DXVK archive".into())
}

/// `getGraphicsReleases("dxvk" | "gplasync")`: recent releases, newest first.
pub async fn releases(build: &str) -> Result<Value, String> {
    let gl = build == "gplasync";
    let data = github_json(&format!("{}?per_page=15", if gl { GPLASYNC_RELEASES } else { OFFICIAL_RELEASES }), 15).await?;
    let date = if gl { "released_at" } else { "published_at" };
    Ok(Value::Array(data.as_array().into_iter().flatten().map(|r| json!({ "tag": r["tag_name"], "prerelease": r["prerelease"].as_bool().unwrap_or(false), "date": r[date] })).collect()))
}

/// Tag and URL of one release.
async fn tagged(build: &str, tag: &str) -> Result<(String, String), String> {
    let url = if build == "gplasync" { format!("{GPLASYNC_RELEASES}/{tag}") } else { format!("{OFFICIAL_RELEASES}/tags/{tag}") };
    release_asset(build, &github_json(&url, 15).await?).ok_or(format!("No Windows tarball in {build} {tag}"))
}

/// Release tag plus folder with `x64/` and `x32/`: `version` or the newest, which falls
/// back to the newest cached release when offline.
async fn fetch(app: &AppHandle, build: &str, version: Option<&str>) -> Result<(String, PathBuf), String> {
    static LOCK: Mutex<()> = Mutex::const_new(());
    let _guard = LOCK.lock().await;
    let cache = cache_root(app)?;
    if let Some(v) = version {
        if !crate::optiscaler::valid_tag(v) {
            return Err(format!("Invalid DXVK version: {v}"));
        }
        if complete(&cache.join(format!("{build}_{v}"))) {
            return Ok((v.to_string(), cache.join(format!("{build}_{v}"))));
        }
    }
    let found = match version {
        Some(v) => Some(tagged(build, v).await?),
        None => latest(build).await,
    };
    let Some((tag, url)) = found else {
        return newest_cached(&cache, build).ok_or(format!("Could not reach the {build} releases and none is cached"));
    };
    let dest = cache.join(format!("{build}_{tag}"));
    if complete(&dest) {
        return Ok((tag, dest));
    }
    let tmp = cache.join(format!("{build}_{tag}.tmp"));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let archive = tmp.join("release.tar.gz");
    let result = async {
        download(&url, &archive, 120).await?;
        let root = unpack(&archive, &tmp.join("out"))?;
        let _ = fs::remove_dir_all(&dest);
        fs::rename(root, &dest).map_err(|e| e.to_string())
    }
    .await;
    let _ = fs::remove_dir_all(&tmp);
    result.map(|_| (tag, dest))
}

fn newest_cached(cache: &Path, build: &str) -> Option<(String, PathBuf)> {
    let prefix = format!("{build}_");
    let mut dirs: Vec<String> = fs::read_dir(cache).ok()?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|d| d.starts_with(&prefix) && !d.ends_with(".tmp") && complete(&cache.join(d))).collect();
    dirs.sort_by(|a, b| natural_cmp(b, a));
    dirs.first().map(|d| (d[prefix.len()..].to_string(), cache.join(d)))
}

// ---------- install ----------

fn read_marker(dir: &Path) -> Option<Value> {
    let marker: Value = serde_json::from_str(&fs::read_to_string(dir.join(MARKER)).ok()?).ok()?;
    marker["build"].as_str().filter(|b| BUILDS.contains(b))?;
    Some(marker)
}

/// True when this game folder has a DXVK install from Chimaera.
pub(crate) fn installed(dir: &Path) -> bool {
    read_marker(dir).is_some()
}

fn write_marker(dir: &Path, build: &str, tag: &str, files: &[String], backed: &[String]) -> Result<(), String> {
    let marker = json!({ "build": build, "version": tag, "files": files, "backed_up": backed });
    fs::write(dir.join(MARKER), serde_json::to_string_pretty(&marker).unwrap()).map_err(|e| e.to_string())
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect()
}

/// Removes our files and restores the backed-up originals. `keep_conf` keeps our dxvk.conf (reinstall).
fn uninstall(dir: &Path, keep_conf: bool) -> Result<(), String> {
    let marker = read_marker(dir).ok_or("No DXVK install marker found")?;
    for rel in strings(&marker["files"]) {
        if keep_conf && rel == CONF {
            continue;
        }
        if let Some(p) = inside(dir, &rel) {
            let _ = fs::remove_file(p);
        }
    }
    let backup = dir.join(BACKUP);
    for rel in strings(&marker["backed_up"]) {
        let (Some(src), Some(dst)) = (inside(&backup, &rel), inside(dir, &rel)) else { continue };
        if src.exists() {
            fs::rename(&src, &dst).map_err(|e| e.to_string())?;
        }
    }
    let _ = fs::remove_dir_all(&backup);
    let _ = fs::remove_file(dir.join(MARKER));
    Ok(())
}

/// Copies the DLLs for `bits` from `release` into `dir`, backing up what they replace.
/// The marker is written first, so a failure rolls back through `uninstall`.
fn install(release: &Path, dir: &Path, build: &str, tag: &str, files: &[&str], bits: u32) -> Result<(), String> {
    let old = read_marker(dir);
    if old.is_none() && dir.join(MARKER).exists() {
        return Err("dxvk_manager.json in this folder is unreadable; remove it first".into());
    }
    let had_conf = old.as_ref().is_some_and(|m| strings(&m["files"]).iter().any(|f| f == CONF));
    if old.is_some() {
        // Restore originals first so the backups stay the true originals.
        uninstall(dir, build == "gplasync")?;
    }
    let src_dir = release.join(if bits == 64 { "x64" } else { "x32" });
    let backup = dir.join(BACKUP);
    let (mut copied, mut backed): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    write_marker(dir, build, tag, &copied, &backed)?;

    let mut run = || -> Result<(), String> {
        for name in files {
            let dst = dir.join(name);
            if dst.exists() {
                fs::create_dir_all(&backup).map_err(|e| e.to_string())?;
                fs::rename(&dst, backup.join(name)).map_err(|e| e.to_string())?;
                backed.push(name.to_string());
            }
            fs::copy(src_dir.join(name), &dst).map_err(|e| e.to_string())?;
            copied.push(name.to_string());
            write_marker(dir, build, tag, &copied, &backed)?;
        }
        if build == "gplasync" {
            let conf = dir.join(CONF);
            if !conf.exists() {
                fs::write(&conf, "dxvk.enableAsync = True\n").map_err(|e| e.to_string())?;
                copied.push(CONF.into());
            } else if had_conf {
                // Still ours from the previous install: keep the user's edits.
                copied.push(CONF.into());
            }
        }
        write_marker(dir, build, tag, &copied, &backed)
    };
    run().inspect_err(|_| {
        // Rollback is best effort; the original error matters more.
        let _ = uninstall(dir, false);
    })
}

/// True once DXVK wrote a log next to the exe after the install, i.e. the game loaded it.
fn loaded(exe: &Path, dir: &Path) -> bool {
    let (Some(stem), Ok(marker)) = (exe.file_stem().map(|s| s.to_string_lossy().into_owned()), fs::metadata(dir.join(MARKER)).and_then(|m| m.modified())) else { return false };
    ["d3d11", "d3d9", "dxgi"].iter().any(|api| fs::metadata(dir.join(format!("{stem}_{api}.log"))).and_then(|m| m.modified()).is_ok_and(|t| t >= marker))
}

// ---------- channels ----------

/// `getDxvkStatus`.
pub async fn status(app: &AppHandle, shop: &str, object_id: &str) -> Result<Value, String> {
    let (exe, dir) = context(app, shop, object_id)?;
    let marker = read_marker(&dir);
    let api = detect_api(&exe);
    let build = marker.as_ref().and_then(|m| m["build"].as_str()).unwrap_or("dxvk").to_string();
    let latest_tag = latest(&build).await.map(|(t, _)| t);
    let version = marker.as_ref().and_then(|m| m["version"].as_str()).map(str::to_string);
    let update = matches!((&version, &latest_tag), (Some(v), Some(l)) if v != l);
    Ok(json!({
        "installed": marker.is_some(),
        "build": marker.as_ref().map(|_| build),
        "version": version,
        "latestTag": latest_tag,
        "updateAvailable": update,
        "api": api,
        "bits": pe_bits(&exe),
        "anticheat": detect_anticheat(&game_root_from_exe_dir(&dir)),
        // A DXVK install is never blocked by itself; reinstall stays possible.
        "blockedBy": if marker.is_some() { None } else { blocked(&exe, &dir, api) },
        "loaded": marker.is_some() && loaded(&exe, &dir),
    }))
}

/// `installDxvk`.
pub async fn install_for_game(app: &AppHandle, shop: &str, object_id: &str, build: &str, version: Option<&str>) -> Result<Value, String> {
    if !BUILDS.contains(&build) {
        return Err(format!("Invalid DXVK build: {build}"));
    }
    let (exe, dir) = context(app, shop, object_id)?;
    let api = detect_api(&exe);
    match blocked(&exe, &dir, api) {
        Some("optiscaler") => return Err("OptiScaler is installed in this game; uninstall it first".into()),
        Some("anticheat") => return Err("This game uses anti-cheat; DXVK is not allowed here".into()),
        Some("reshade") => return Err("ReShade uses one of the DLLs DXVK replaces; remove ReShade or install it under another name".into()),
        Some(_) => return Err(format!("DXVK supports DirectX 9-11 games only (detected: {api})")),
        None => {}
    }
    let (files, bits) = (files_for(api).unwrap_or_default(), pe_bits(&exe).unwrap_or(64));
    let (tag, release) = fetch(app, build, version).await?;
    let build = build.to_string();
    tauri::async_runtime::spawn_blocking(move || install(&release, &dir, &build, &tag, files, bits).map(|_| json!({ "version": tag })))
        .await
        .map_err(|e| e.to_string())?
}

/// `uninstallDxvk`.
pub fn uninstall_for_game(app: &AppHandle, shop: &str, object_id: &str) -> Result<(), String> {
    uninstall(&context(app, shop, object_id)?.1, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chimaera-dxvk-{name}-{}", crate::library::now_ms()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn pe(machine: u16) -> Vec<u8> {
        let mut b = vec![0u8; 0x80];
        b[..2].copy_from_slice(b"MZ");
        b[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        b[0x40..0x44].copy_from_slice(b"PE\0\0");
        b[0x44..0x46].copy_from_slice(&machine.to_le_bytes());
        b
    }

    /// A fake release folder: `<root>/<arch>/<dll>` holding "<arch>:<dll>".
    fn release(root: &Path) -> PathBuf {
        let rel = root.join("release");
        for arch in ["x64", "x32"] {
            fs::create_dir_all(rel.join(arch)).unwrap();
            for dll in ["d3d9.dll", "d3d10core.dll", "d3d11.dll", "dxgi.dll"] {
                fs::write(rel.join(arch).join(dll), format!("{arch}:{dll}")).unwrap();
            }
        }
        rel
    }

    #[test]
    fn reads_pe_bits() {
        let dir = temp("pe");
        let (x64, x86, junk) = (dir.join("a.exe"), dir.join("b.exe"), dir.join("c.exe"));
        fs::write(&x64, pe(0x8664)).unwrap();
        fs::write(&x86, pe(0x14c)).unwrap();
        fs::write(&junk, b"not an exe").unwrap();
        assert_eq!(pe_bits(&x64), Some(64));
        assert_eq!(pe_bits(&x86), Some(32));
        assert_eq!(pe_bits(&junk), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn picks_dlls_per_api() {
        assert_eq!(files_for("dx9"), Some(&["d3d9.dll"][..]));
        assert_eq!(files_for("dx11").unwrap().len(), 3);
        assert!(files_for("dx12").is_none() && files_for("vulkan").is_none() && files_for("unknown").is_none());
    }

    #[test]
    fn finds_release_assets() {
        let gh = json!({ "tag_name": "v3.1.1", "assets": [
            { "name": "dxvk-native-3.1.1-steamrt-sniper.tar.gz", "browser_download_url": "native" },
            { "name": "dxvk-3.1.1.tar.gz", "browser_download_url": "win" } ] });
        assert_eq!(release_asset("dxvk", &gh), Some(("v3.1.1".into(), "win".into())));
        let gl = json!([{ "tag_name": "v3.1.1-1", "assets": { "links": [
            { "name": "dxvk-gplasync-v3.1.1-1.zip", "direct_asset_url": "zip" },
            { "name": "dxvk-gplasync-v3.1.1-1.tar.gz", "direct_asset_url": "tgz" } ] } }]);
        assert_eq!(release_asset("gplasync", &gl), Some(("v3.1.1-1".into(), "tgz".into())));
    }

    #[test]
    fn unpacks_tar_gz_with_a_top_folder() {
        let root = temp("unpack");
        let rel = release(&root);
        let archive = root.join("r.tar.gz");
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(fs::File::create(&archive).unwrap(), flate2::Compression::fast()));
        tar.append_dir_all("dxvk-9.9", &rel).unwrap();
        tar.into_inner().unwrap().finish().unwrap();
        let out = unpack(&archive, &root.join("out")).unwrap();
        assert_eq!(out, root.join("out").join("dxvk-9.9"));
        assert_eq!(fs::read_to_string(out.join("x32").join("d3d9.dll")).unwrap(), "x32:d3d9.dll");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn installs_backs_up_reinstalls_and_rolls_back() {
        let root = temp("install");
        let rel = release(&root);
        let game = root.join("game");
        fs::create_dir_all(&game).unwrap();
        fs::write(game.join("d3d11.dll"), b"original").unwrap();
        let dx11 = files_for("dx11").unwrap();

        install(&rel, &game, "gplasync", "v1", dx11, 32).unwrap();
        assert_eq!(fs::read_to_string(game.join("d3d11.dll")).unwrap(), "x32:d3d11.dll");
        assert_eq!(fs::read(game.join(BACKUP).join("d3d11.dll")).unwrap(), b"original");
        assert!(fs::read_to_string(game.join(CONF)).unwrap().contains("enableAsync"));
        assert!(installed(&game));

        // User edits to our dxvk.conf survive a reinstall; the backup keeps the true original.
        fs::write(game.join(CONF), "edited").unwrap();
        install(&rel, &game, "gplasync", "v2", dx11, 64).unwrap();
        assert_eq!(fs::read_to_string(game.join(CONF)).unwrap(), "edited");
        assert_eq!(fs::read_to_string(game.join("dxgi.dll")).unwrap(), "x64:dxgi.dll");
        assert_eq!(fs::read(game.join(BACKUP).join("d3d11.dll")).unwrap(), b"original");

        uninstall(&game, false).unwrap();
        assert_eq!(fs::read(game.join("d3d11.dll")).unwrap(), b"original");
        assert!(!game.join("dxgi.dll").exists() && !game.join(CONF).exists() && !game.join(MARKER).exists() && !game.join(BACKUP).exists());

        // A user's own dxvk.conf is never written over or removed.
        fs::write(game.join(CONF), "theirs").unwrap();
        install(&rel, &game, "gplasync", "v1", dx11, 64).unwrap();
        uninstall(&game, false).unwrap();
        assert_eq!(fs::read_to_string(game.join(CONF)).unwrap(), "theirs");

        // A missing DLL in the release rolls back to the original files.
        fs::remove_file(rel.join("x64").join("dxgi.dll")).unwrap();
        assert!(install(&rel, &game, "dxvk", "v3", dx11, 64).is_err());
        assert_eq!(fs::read(game.join("d3d11.dll")).unwrap(), b"original");
        assert!(!game.join(MARKER).exists() && !game.join("d3d10core.dll").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn blocks_optiscaler_anticheat_and_wrong_api() {
        let dir = temp("blocked");
        let exe = dir.join("game.exe");
        fs::write(&exe, pe(0x8664)).unwrap();
        assert_eq!(blocked(&exe, &dir, "dx11"), None);
        assert_eq!(blocked(&exe, &dir, "dx12"), Some("api"));
        fs::write(dir.join(crate::optiscaler::MARKER), r#"{"proxy":"dxgi.dll"}"#).unwrap();
        assert_eq!(blocked(&exe, &dir, "dx11"), Some("optiscaler"));
        fs::remove_file(dir.join(crate::optiscaler::MARKER)).unwrap();
        let reshade: Vec<u8> = "ReShade".bytes().flat_map(|b| [b, 0]).collect();
        fs::write(dir.join("dxgi.dll"), reshade).unwrap();
        assert_eq!(blocked(&exe, &dir, "dx11"), Some("reshade"));
        // DX9 only replaces d3d9.dll, so ReShade as dxgi.dll is fine there.
        assert_eq!(blocked(&exe, &dir, "dx9"), None);
        fs::remove_file(dir.join("dxgi.dll")).unwrap();
        fs::create_dir_all(dir.join("EasyAntiCheat")).unwrap();
        assert_eq!(blocked(&exe, &dir, "dx11"), Some("anticheat"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore = "network: lists releases and resolves a tag for each build"]
    fn lists_live_releases() {
        tauri::async_runtime::block_on(async {
            for build in BUILDS {
                let list = releases(build).await.unwrap();
                let tag = list[0]["tag"].as_str().unwrap().to_string();
                let (t, url) = tagged(build, &tag).await.unwrap();
                assert_eq!(t, tag);
                assert!(url.contains(".tar.gz"), "{url}");
            }
            assert!(crate::optiscaler::releases().await.unwrap().as_array().unwrap().len() > 1);
        });
    }

    #[test]
    #[ignore = "network: downloads both DXVK releases"]
    fn fetches_live_releases() {
        tauri::async_runtime::block_on(async {
            for build in BUILDS {
                let (tag, url) = latest(build).await.expect("release");
                let root = temp(build);
                let archive = root.join("r.tar.gz");
                download(&url, &archive, 120).await.unwrap();
                let out = unpack(&archive, &root.join("out")).unwrap();
                assert!(out.join("x64").join("d3d11.dll").is_file(), "{build} {tag}");
                fs::remove_dir_all(root).unwrap();
            }
        });
    }
}
