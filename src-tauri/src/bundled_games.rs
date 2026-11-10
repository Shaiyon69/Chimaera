//! Games Chimaera ships itself (`custom` shop): their store pages and install
//! checks, installers and launchers, ported from src/main/services/bloodborne/,
//! horizon/ and github-zip-install.ts. Launching the game itself is not ported yet.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};

use crate::electron_import::electron_user_data;
use crate::data_location::DataDir;

struct BundledGame {
    object_id: &'static str,
    title: &'static str,
    /// Folder under resources/ holding its art.
    art_dir: &'static str,
    screenshots: &'static [&'static str],
    about: &'static str,
    description: &'static str,
    developers: &'static [&'static str],
    publishers: &'static [&'static str],
    genres: &'static [(&'static str, &'static str)],
    requirements: (&'static str, &'static str),
    release_date: &'static str,
    /// Install folder in the app data folder (the same name Electron used).
    install_dir: &'static str,
    launcher_exe: &'static str,
    /// Names the error messages.
    label: &'static str,
    releases_url: &'static str,
    pick_release: fn(&Value) -> Option<Release>,
    /// The player's files that survive an update (Lantern's Updater.Kept).
    kept: &'static [&'static str],
}

#[derive(Debug, PartialEq)]
struct Release {
    url: String,
    sha256: String,
}

const BLOODBORNE: BundledGame = BundledGame {
    object_id: "bloodborne",
    title: "Bloodborne",
    art_dir: "bloodborne",
    screenshots: &[
        "screenshot-001.jpg", "screenshot-008.jpg", "screenshot-015.jpg", "screenshot-046.jpg", "screenshot-054.jpg",
        "screenshot-062.jpg", "screenshot-070.jpg", "screenshot-078.jpg", "screenshot-086.jpg",
    ],
    about: "Hunt your nightmares in the gothic city of Yharnam. Runs on Lantern Engine by Awaken: bring your own CUSA03173 dump.",
    description: concat!(
        "<p>Face your fears as you search for answers in the ancient city of Yharnam, now cursed with a strange endemic illness spreading through the streets like wildfire. Danger, death and madness lurk around every corner of this dark and horrifying world, and you must discover its darkest secrets in order to survive.</p>",
        "<ul><li>Fast, aggressive combat built around trick weapons and the regain system.</li>",
        "<li>Interconnected gothic world, optional chalice dungeons and three endings.</li>",
        "<li>The Old Hunters expansion adds the Hunter's Nightmare and new bosses.</li></ul>",
        "<p><strong>PC notes:</strong> runs on <a href=\"https://github.com/Awakenchan/LanternEngine\">Lantern Engine by Awaken</a>, where the game's own code runs natively and its graphics are translated to Vulkan. It needs your own decrypted CUSA03173 v1.09 dump and supports FSR, DLSS and TAA upscaling plus FPS and resolution patches.</p>",
    ),
    developers: &["FromSoftware"],
    publishers: &["Sony Interactive Entertainment"],
    genres: &[("1", "Action"), ("3", "RPG")],
    // Figures from earlier bbport-based builds and player reports
    // (RX 6650 XT + Ryzen 5 5600X: 89-100 FPS at 1080p).
    requirements: (
        concat!(
            "<ul class=\"bb_ul\"><li><strong>OS:</strong> Windows 10 (1903+) 64-bit</li>",
            "<li><strong>Processor:</strong> Intel Core 4th gen (Haswell) or AMD Ryzen, with AVX2</li>",
            "<li><strong>Memory:</strong> 6 GB free (RAM + page file)</li>",
            "<li><strong>Graphics:</strong> Vulkan 1.3 GPU: GeForce GTX 1060, Radeon RX 480 or Intel Arc A380</li>",
            "<li><strong>Additional Notes:</strong> Your own decrypted CUSA03173 v1.09 dump (eboot.bin). Target: 30-60 FPS at 720p-1080p with an upscaler.</li></ul>",
        ),
        concat!(
            "<ul class=\"bb_ul\"><li><strong>OS:</strong> Windows 11 64-bit</li>",
            "<li><strong>Processor:</strong> AMD Ryzen 5 5600X or better</li>",
            "<li><strong>Memory:</strong> 10 GB free (RAM + page file) for 1440p/4K output</li>",
            "<li><strong>Graphics:</strong> Radeon RX 6650 XT / GeForce RTX 3060 or better (RTX 20-series+ for DLSS)</li>",
            "<li><strong>Additional Notes:</strong> Current vendor drivers. Reported: 89-100 FPS at 1080p on an RX 6650 XT; about 90 FPS at 4K with FSR 4 Balanced on an RX 7800 XT.</li></ul>",
        ),
    ),
    release_date: "24 Mar, 2015",
    install_dir: "lantern-engine",
    launcher_exe: "Lantern Engine.exe",
    label: "Lantern Engine",
    releases_url: "https://api.github.com/repos/Awakenchan/LanternEngine/releases/latest",
    pick_release: pick_lantern_release,
    kept: &["lantern.ini", "bbport.ini", "mods.json", "patches.json", "user", "mods", "logs", "network", "engine-assets"],
};

const HORIZON: BundledGame = BundledGame {
    object_id: "horizon",
    title: "Minecraft (Horizon)",
    art_dir: "horizon",
    // In-game shots from playhorizon.dev: village, survival, night, nether, underwater, launcher, settings.
    screenshots: &[
        "screenshot-01.jpg", "screenshot-02.jpg", "screenshot-03.jpg", "screenshot-04.jpg", "screenshot-05.jpg", "screenshot-06.jpg",
        "screenshot-07.jpg",
    ],
    about: "Minecraft: Java Edition 26.3, rewritten from scratch in Rust. Bring your own Java Edition account.",
    description: concat!(
        "<p>Horizon is a from-scratch Rust rewrite of Minecraft: Java Edition 26.3, checked against vanilla behaviour. Play singleplayer worlds, join servers, or host one from the launcher.</p>",
        "<p><strong>PC notes:</strong> runs through the <a href=\"https://playhorizon.dev\">Horizon launcher</a>, which updates itself and installs the game. It needs a Microsoft account that owns Minecraft: Java Edition; Horizon ships no Minecraft code or assets.</p>",
        "<p>Not an official Minecraft product. Not approved by or associated with Mojang or Microsoft.</p>",
    ),
    developers: &["Horizon contributors"],
    publishers: &["Horizon contributors"],
    genres: &[("1", "Sandbox"), ("2", "Survival")],
    requirements: (
        concat!(
            "<ul class=\"bb_ul\"><li><strong>OS:</strong> Windows 10 64-bit</li>",
            "<li><strong>Additional Notes:</strong> A Microsoft account that owns Minecraft: Java Edition. The launcher downloads the 26.3 game files from Mojang for that account.</li></ul>",
        ),
        "",
    ),
    release_date: "",
    install_dir: "horizon-launcher",
    launcher_exe: "horizon-launcher.exe",
    label: "Horizon launcher",
    // releases/latest is the game build; the launcher has launcher@ tags.
    releases_url: "https://api.github.com/repos/horizon-rust/horizon/releases?per_page=30",
    pick_release: pick_horizon_release,
    kept: &[],
};

fn find(object_id: &str) -> Option<&'static BundledGame> {
    [&BLOODBORNE, &HORIZON].into_iter().find(|g| g.object_id == object_id)
}

/// `local:` URL of a file in the game's bundled art folder, or null when it is missing.
fn art(resources: &Path, game: &BundledGame, name: &str) -> Value {
    let file = resources.join(game.art_dir).join(name);
    if file.exists() { json!(format!("local:{}", file.to_string_lossy())) } else { Value::Null }
}

fn assets(resources: &Path, game: &BundledGame) -> Value {
    json!({
        "objectId": game.object_id,
        "shop": "custom",
        "title": game.title,
        "iconUrl": art(resources, game, "icon.png"),
        "libraryHeroImageUrl": art(resources, game, "hero.jpg"),
        "libraryImageUrl": art(resources, game, "header.png"),
        "logoImageUrl": art(resources, game, "logo.png"),
        "logoPosition": null,
        "coverImageUrl": art(resources, game, "cover.png"),
        "downloadSources": [],
    })
}

fn details(resources: &Path, game: &BundledGame) -> Value {
    let screenshots: Vec<Value> = game
        .screenshots
        .iter()
        .filter_map(|name| art(resources, game, name).as_str().map(str::to_string))
        .enumerate()
        .map(|(id, url)| json!({ "id": id, "path_thumbnail": url, "path_full": url }))
        .collect();
    let genres: Vec<Value> = game.genres.iter().map(|(id, name)| json!({ "id": id, "name": name })).collect();
    let empty = json!({ "minimum": "", "recommended": "" });
    json!({
        "objectId": game.object_id,
        "name": game.title,
        "steam_appid": 0,
        "detailed_description": game.description,
        "about_the_game": game.description,
        "short_description": game.about,
        "screenshots": screenshots,
        "developers": game.developers,
        "publishers": game.publishers,
        "genres": genres,
        "supported_languages": "English",
        "pc_requirements": { "minimum": game.requirements.0, "recommended": game.requirements.1 },
        "mac_requirements": empty,
        "linux_requirements": empty,
        "release_date": { "coming_soon": false, "date": game.release_date },
        "content_descriptors": { "ids": [] },
    })
}

fn resource_dir(app: &AppHandle) -> PathBuf {
    app.path().resource_dir().unwrap_or_default()
}

/// `getGameShopDetails` for the custom shop: a bundled game's page, or null for the user's own games.
pub fn shop_details(app: &AppHandle, object_id: &str) -> Value {
    let Some(game) = find(object_id) else { return Value::Null };
    let resources = resource_dir(app);
    let mut page = details(&resources, game);
    page["assets"] = assets(&resources, game);
    page
}

/// The sha256 GitHub publishes for an asset, as `sha256:<hex>`.
fn asset_release(asset: &Value) -> Option<Release> {
    let url = asset["browser_download_url"].as_str()?;
    let hex = asset["digest"].as_str()?.strip_prefix("sha256:")?;
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| Release { url: url.to_string(), sha256: hex.to_ascii_lowercase() })
}

/// The Windows package zip of a releases/latest payload (pickPortRelease).
fn pick_lantern_release(release: &Value) -> Option<Release> {
    release["tag_name"].as_str()?;
    let asset = release["assets"].as_array()?.iter().find(|a| {
        a["name"].as_str().is_some_and(|n| {
            let n = n.to_ascii_lowercase();
            n.len() > "lanternengine--win64.zip".len() && n.starts_with("lanternengine-") && n.ends_with("-win64.zip")
        })
    })?;
    asset_release(asset)
}

/// The newest stable launcher@ release with a verifiable Windows zip (pickLauncherRelease).
fn pick_horizon_release(releases: &Value) -> Option<Release> {
    releases.as_array()?.iter().find_map(|r| {
        if r["prerelease"].as_bool() == Some(true) || r["draft"].as_bool() == Some(true) {
            return None;
        }
        r["tag_name"].as_str().filter(|t| t.starts_with("launcher@"))?;
        let asset = r["assets"].as_array()?.iter().find(|a| a["name"] == "horizon-launcher-windows-x64.zip")?;
        asset_release(asset)
    })
}

/// Whether `dir` holds a usable package: the launcher, plus for Lantern the engine probe
/// (in engine\ since 0.9.37, the package itself before; engineDirOf).
fn is_valid_dir(game: &BundledGame, dir: &Path) -> bool {
    let probe = |base: &Path| base.join("out").join("bb-probe.exe").exists();
    dir.join(game.launcher_exe).exists() && (game.object_id != "bloodborne" || probe(&dir.join("engine")) || probe(dir))
}

/// The Tauri app data folder, unless only the Electron build's folder holds an install.
/// That one is used in place so the player's Lantern saves (in `user\`) stay put.
fn pick_install_dir(game: &BundledGame, app_data: &Path, electron_user_data: &Path) -> PathBuf {
    let own = app_data.join(game.install_dir);
    let electron = electron_user_data.join(game.install_dir);
    if !is_valid_dir(game, &own) && is_valid_dir(game, &electron) { electron } else { own }
}

fn install_dir(app: &AppHandle, game: &BundledGame) -> Result<PathBuf, String> {
    let app_data = app.path().chimaera_data_dir().map_err(|e| e.to_string())?;
    let electron = electron_user_data(&app.path().config_dir().map_err(|e| e.to_string())?);
    Ok(pick_install_dir(game, &app_data, &electron))
}

fn known(object_id: &str) -> Result<&'static BundledGame, String> {
    find(object_id).ok_or_else(|| format!("Unknown bundled game: {object_id}"))
}

/// `getBundledGameSetup`.
pub fn setup(app: &AppHandle, object_id: &str) -> Result<Value, String> {
    let game = known(object_id)?;
    Ok(json!({ "installed": is_valid_dir(game, &install_dir(app, game)?) }))
}

/// `openBundledGameLauncher`: starts Lantern's or Horizon's own launcher, detached.
pub fn open_launcher(app: &AppHandle, object_id: &str) -> Result<Value, String> {
    let game = known(object_id)?;
    let dir = install_dir(app, game)?;
    if let Err(error) = std::process::Command::new(dir.join(game.launcher_exe)).current_dir(&dir).spawn() {
        let problem = format!("could not start the {}: {error}", game.label);
        let _ = app.emit("on-bundled-game-launch-error", [json!(game.object_id), json!([problem])]);
    }
    Ok(Value::Null)
}

// One install at a time per game (BLOODBORNE, HORIZON); a second click waits for the first.
static INSTALLING: [tokio::sync::Mutex<()>; 2] = [tokio::sync::Mutex::const_new(()), tokio::sync::Mutex::const_new(())];

/// `downloadBundledGame`: installs the latest release. The game's own launcher keeps it current after that.
// shortcut: no migrateLegacyData (Mrsuss60 port and old launcher saves) and no NSIS "Include Bloodborne" marker; Electron builds did both already.
pub async fn download(app: &AppHandle, object_id: &str) -> Result<Value, String> {
    let game = known(object_id)?;
    let _guard = INSTALLING[usize::from(game.object_id != "bloodborne")].lock().await;
    let target = install_dir(app, game)?;
    let scratch = app.path().chimaera_data_dir().map_err(|e| e.to_string())?;
    let zip_path = scratch.join(format!("{}.zip.part", game.install_dir));
    let extract_dir = scratch.join(format!("{}.tmp", game.install_dir));
    let progress = |phase: &str, percent: u64| {
        let _ = app.emit("on-bundled-game-progress", [json!({ "id": game.object_id, "phase": phase, "percent": percent })]);
    };
    let result = install(game, &target, &zip_path, &extract_dir, &progress).await;
    let _ = std::fs::remove_file(&zip_path);
    let _ = std::fs::remove_dir_all(&extract_dir);
    result.map(|()| Value::Null)
}

async fn install(game: &BundledGame, target: &Path, zip_path: &Path, extract_dir: &Path, progress: &(dyn Fn(&str, u64) + Sync)) -> Result<(), String> {
    let label = game.label;
    // GitHub's API refuses requests without a User-Agent.
    let client = reqwest::Client::builder().user_agent("Chimaera").build().map_err(|e| e.to_string())?;
    let response = client.get(game.releases_url).header("Accept", "application/vnd.github+json").send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("{label} release lookup failed: HTTP {}", response.status().as_u16()));
    }
    let payload: Value = response.json().await.map_err(|e| e.to_string())?;
    let release = (game.pick_release)(&payload).ok_or_else(|| format!("No {label} release has a verifiable zip"))?;

    let mut response = client.get(&release.url).send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("{label} download failed: HTTP {}", response.status().as_u16()));
    }
    if let Some(parent) = zip_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let total = response.content_length().unwrap_or(0);
    let mut file = std::fs::File::create(zip_path).map_err(|e| e.to_string())?;
    let (mut hash, mut received, mut last) = (Sha256::new(), 0u64, u64::MAX);
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        hash.update(&chunk);
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        received += chunk.len() as u64;
        let percent = if total > 0 { received * 100 / total } else { 0 };
        if percent != last {
            last = percent;
            progress("downloading", percent);
        }
    }
    drop(file);
    let digest: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if digest != release.sha256 {
        return Err(format!("{label} download is corrupted (checksum mismatch)"));
    }

    progress("extracting", 0);
    let (zip, out) = (zip_path.to_path_buf(), extract_dir.to_path_buf());
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let _ = std::fs::remove_dir_all(&out);
        // extract() refuses entries whose paths escape `out`.
        zip::ZipArchive::new(std::fs::File::open(&zip).map_err(|e| e.to_string())?)
            .and_then(|mut archive| archive.extract(&out))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    progress("extracting", 100);
    swap_in(game, extract_dir, target)
}

/// Moves the extracted package into place last, so a failed attempt never leaves a
/// half-installed one. Windows refuses the first rename while the game runs, which
/// aborts and leaves the old package untouched.
fn swap_in(game: &BundledGame, extract_dir: &Path, target: &Path) -> Result<(), String> {
    // Some release zips wrap everything in one folder.
    let inner: Vec<PathBuf> = std::fs::read_dir(extract_dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    let package = if !is_valid_dir(game, extract_dir) && inner.len() == 1 { inner[0].clone() } else { extract_dir.to_path_buf() };
    if !is_valid_dir(game, &package) {
        return Err(format!("Archive is missing {}", game.launcher_exe));
    }
    let mut old = target.as_os_str().to_owned();
    old.push(".old");
    let old = PathBuf::from(old);
    let _ = std::fs::remove_dir_all(&old);
    if target.exists() {
        std::fs::rename(target, &old).map_err(|e| e.to_string())?;
    }
    if let Err(error) = std::fs::rename(&package, target) {
        if old.exists() {
            let _ = std::fs::rename(&old, target);
        }
        return Err(error.to_string());
    }
    if !old.exists() {
        return Ok(());
    }
    for name in game.kept {
        let source = old.join(name);
        if !source.exists() {
            continue;
        }
        let dest = target.join(name);
        let _ = std::fs::remove_dir_all(&dest);
        let _ = std::fs::remove_file(&dest);
        // Leaves `.old` in place on failure so the player's files are never lost.
        std::fs::rename(&source, &dest).map_err(|e| format!("could not keep {name}: {e}"))?;
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_list_only_art_that_exists() {
        let resources = std::env::temp_dir().join(format!("chimaera-bundled-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(resources.join("bloodborne")).unwrap();
        std::fs::write(resources.join("bloodborne").join("screenshot-015.jpg"), b"").unwrap();
        std::fs::write(resources.join("bloodborne").join("cover.png"), b"").unwrap();
        let page = details(&resources, &BLOODBORNE);
        assert_eq!(page["screenshots"].as_array().unwrap().len(), 1);
        assert_eq!(page["screenshots"][0]["id"], 0);
        assert_eq!(page["genres"][1], json!({ "id": "3", "name": "RPG" }));
        let art = assets(&resources, &BLOODBORNE);
        assert!(art["coverImageUrl"].as_str().unwrap().starts_with("local:"));
        assert!(art["heroImageUrl"].is_null() && art["libraryHeroImageUrl"].is_null());
        std::fs::remove_dir_all(&resources).unwrap();
    }

    #[test]
    fn detects_both_lantern_layouts() {
        let user_data = std::env::temp_dir().join(format!("chimaera-bundled-{}", uuid::Uuid::new_v4()));
        let dir = user_data.join("lantern-engine");
        std::fs::create_dir_all(dir.join("engine").join("out")).unwrap();
        assert!(!is_valid_dir(&BLOODBORNE, &dir));
        std::fs::write(dir.join("Lantern Engine.exe"), b"").unwrap();
        std::fs::write(dir.join("engine").join("out").join("bb-probe.exe"), b"").unwrap();
        assert!(is_valid_dir(&BLOODBORNE, &dir));
        assert!(!is_valid_dir(&HORIZON, &user_data.join("horizon-launcher")));
        // Only the Electron folder holds an install: it is used in place.
        let own = user_data.join("tauri");
        assert_eq!(pick_install_dir(&BLOODBORNE, &own, &user_data), dir);
        assert_eq!(pick_install_dir(&HORIZON, &own, &user_data), own.join("horizon-launcher"));
        std::fs::remove_dir_all(&user_data).unwrap();
    }

    #[test]
    fn picks_verifiable_release_zips() {
        let digest = format!("sha256:{}", "AB".repeat(32));
        let lantern = json!({ "tag_name": "v1", "assets": [
            { "name": "LanternEngine-1.0-linux.zip", "browser_download_url": "x", "digest": digest },
            { "name": "LanternEngine-1.0-win64.zip", "browser_download_url": "u", "digest": digest },
        ]});
        assert_eq!(pick_lantern_release(&lantern), Some(Release { url: "u".into(), sha256: "ab".repeat(32) }));
        let unsigned = json!({ "tag_name": "v1", "assets": [{ "name": "LanternEngine-1-win64.zip", "browser_download_url": "u" }] });
        assert_eq!(pick_lantern_release(&unsigned), None);
        let asset = |url: &str| json!({ "name": "horizon-launcher-windows-x64.zip", "browser_download_url": url, "digest": digest });
        let horizon = json!([
            { "tag_name": "v26.3", "assets": [asset("game")] },
            { "tag_name": "launcher@2", "prerelease": true, "assets": [asset("pre")] },
            { "tag_name": "launcher@1", "assets": [asset("stable")] },
        ]);
        assert_eq!(pick_horizon_release(&horizon).unwrap().url, "stable");
    }

    #[test]
    #[ignore = "downloads the Horizon launcher from GitHub"]
    fn installs_live_horizon_release() {
        let root = std::env::temp_dir().join(format!("chimaera-bundled-{}", uuid::Uuid::new_v4()));
        let target = root.join("horizon-launcher");
        let progress = |_: &str, _: u64| {};
        tauri::async_runtime::block_on(install(&HORIZON, &target, &root.join("h.zip.part"), &root.join("h.tmp"), &progress)).unwrap();
        assert!(is_valid_dir(&HORIZON, &target));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn update_keeps_player_files_and_unwraps_one_folder() {
        let root = std::env::temp_dir().join(format!("chimaera-bundled-{}", uuid::Uuid::new_v4()));
        let (target, extract) = (root.join("lantern-engine"), root.join("tmp"));
        let package = extract.join("LanternEngine-1.0");
        std::fs::create_dir_all(package.join("out")).unwrap();
        std::fs::write(package.join("Lantern Engine.exe"), b"new").unwrap();
        std::fs::write(package.join("out").join("bb-probe.exe"), b"").unwrap();
        std::fs::write(package.join("lantern.ini"), b"default").unwrap();
        std::fs::create_dir_all(target.join("user")).unwrap();
        std::fs::write(target.join("user").join("save.dat"), b"save").unwrap();
        std::fs::write(target.join("lantern.ini"), b"mine").unwrap();
        swap_in(&BLOODBORNE, &extract, &target).unwrap();
        assert_eq!(std::fs::read(target.join("Lantern Engine.exe")).unwrap(), b"new");
        assert_eq!(std::fs::read(target.join("user").join("save.dat")).unwrap(), b"save");
        assert_eq!(std::fs::read(target.join("lantern.ini")).unwrap(), b"mine");
        assert!(!root.join("lantern-engine.old").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
