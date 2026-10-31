//! Games Chimaera ships itself (`custom` shop): their store pages and install
//! checks, ported from src/main/services/bloodborne/ and horizon/.
//! Download, launch and their launcher windows are not ported yet.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::electron_import::electron_user_data;

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

/// Whether the game's engine/launcher is installed. The Electron build installs into
/// its own data folder and the Tauri installer is not ported yet, so look there.
// shortcut: reads the Electron install folders; move to the Tauri app data folder when downloadBundledGame is ported.
fn installed_in(user_data: &Path, object_id: &str) -> bool {
    match object_id {
        "bloodborne" => {
            let dir = user_data.join("lantern-engine");
            // engine\ since 0.9.37, the package itself before (engineDirOf).
            let probe = |base: &Path| base.join("out").join("bb-probe.exe").exists();
            dir.join("Lantern Engine.exe").exists() && (probe(&dir.join("engine")) || probe(&dir))
        }
        "horizon" => user_data.join("horizon-launcher").join("horizon-launcher.exe").exists(),
        _ => false,
    }
}

/// `getBundledGameSetup`.
pub fn setup(app: &AppHandle, object_id: &str) -> Result<Value, String> {
    if find(object_id).is_none() {
        return Err(format!("Unknown bundled game: {object_id}"));
    }
    let user_data = electron_user_data(&app.path().config_dir().map_err(|e| e.to_string())?);
    Ok(json!({ "installed": installed_in(&user_data, object_id) }))
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
        assert!(!installed_in(&user_data, "bloodborne"));
        std::fs::write(dir.join("Lantern Engine.exe"), b"").unwrap();
        std::fs::write(dir.join("engine").join("out").join("bb-probe.exe"), b"").unwrap();
        assert!(installed_in(&user_data, "bloodborne"));
        assert!(!installed_in(&user_data, "horizon"));
        std::fs::remove_dir_all(&user_data).unwrap();
    }
}
