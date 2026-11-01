//! Non-Steam game shortcuts in Steam's `userdata/<id>/config/shortcuts.vdf`, ported from
//! src/main/services/steam.ts and src/main/events/library/{check,create,delete}-steam-shortcut.ts.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::{DynamicImage, ImageFormat};
use serde_json::{json, Value};

use crate::store::Store;

/// getSteamLocation.
#[cfg(windows)]
pub fn steam_path() -> Option<PathBuf> {
    let key = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER).open_subkey(r"Software\Valve\Steam").ok()?;
    key.get_value::<String, _>("SteamPath").ok().map(PathBuf::from)
}

#[cfg(not(windows))]
pub fn steam_path() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    if cfg!(target_os = "macos") {
        return Some(home.join("Library/Application Support/Steam"));
    }
    [".steam/steam", ".local/share/Steam", ".var/app/com.valvesoftware.Steam/.local/share/Steam", "snap/steam/common/.local/share/Steam"]
        .iter()
        .map(|p| home.join(p))
        .find(|p| p.exists())
}

/// getSteamUsersIds: the numeric folders under `userdata`.
pub fn user_ids(steam: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(steam.join("userdata")) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.parse::<u64>().is_ok())
        .collect()
}

fn config_dir(steam: &Path, user: &str) -> PathBuf {
    steam.join("userdata").join(user).join("config")
}

/// A binary VDF value. Fixed-size types (int32, float, color, uint64, ...) keep their raw
/// bytes, so fields Steam adds that Chimaera never reads survive a rewrite.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Map(Vec<(String, Node)>),
    Str(String),
    Fixed(u8, Vec<u8>),
}

pub(crate) type Entries = Vec<(String, Node)>;

fn read_cstr(buf: &[u8], pos: &mut usize) -> Result<String, String> {
    let rest = buf.get(*pos..).ok_or("shortcuts.vdf: truncated")?;
    let end = rest.iter().position(|&b| b == 0).ok_or("shortcuts.vdf: unterminated string")?;
    *pos += end + 1;
    Ok(String::from_utf8_lossy(&rest[..end]).into_owned())
}

pub(crate) fn parse_map(buf: &[u8], pos: &mut usize) -> Result<Entries, String> {
    let mut entries = Vec::new();
    loop {
        let kind = *buf.get(*pos).ok_or("shortcuts.vdf: truncated")?;
        *pos += 1;
        if kind == 0x08 {
            return Ok(entries);
        }
        let name = read_cstr(buf, pos)?;
        let value = match kind {
            0x00 => Node::Map(parse_map(buf, pos)?),
            0x01 => Node::Str(read_cstr(buf, pos)?),
            0x02 | 0x03 | 0x04 | 0x06 | 0x07 | 0x0A => {
                let size = if matches!(kind, 0x07 | 0x0A) { 8 } else { 4 };
                let bytes = buf.get(*pos..*pos + size).ok_or("shortcuts.vdf: truncated")?.to_vec();
                *pos += size;
                Node::Fixed(kind, bytes)
            }
            // Refuse rather than guess: a wrong guess would corrupt the user's Steam shortcuts.
            other => return Err(format!("shortcuts.vdf: unknown value type {other:#x}")),
        };
        entries.push((name, value));
    }
}

fn write_map(entries: &Entries, out: &mut Vec<u8>) {
    for (name, value) in entries {
        let kind = match value {
            Node::Map(_) => 0x00,
            Node::Str(_) => 0x01,
            Node::Fixed(kind, _) => *kind,
        };
        out.push(kind);
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        match value {
            Node::Map(children) => write_map(children, out),
            Node::Str(s) => {
                out.extend_from_slice(s.as_bytes());
                out.push(0);
            }
            Node::Fixed(_, bytes) => out.extend_from_slice(bytes),
        }
    }
    out.push(0x08);
}

/// getSteamShortcuts: each shortcut's fields. No file is no shortcuts.
pub fn read_shortcuts(steam: &Path, user: &str) -> Result<Vec<Entries>, String> {
    let Ok(buf) = fs::read(config_dir(steam, user).join("shortcuts.vdf")) else { return Ok(Vec::new()) };
    let root = parse_map(&buf, &mut 0)?;
    Ok(match root.into_iter().find(|(name, _)| name.eq_ignore_ascii_case("shortcuts")) {
        Some((_, Node::Map(list))) => list.into_iter().filter_map(|(_, s)| if let Node::Map(f) = s { Some(f) } else { None }).collect(),
        _ => Vec::new(),
    })
}

/// writeSteamShortcuts: renumbers the list "0", "1", ... as Steam does.
pub fn write_shortcuts(steam: &Path, user: &str, shortcuts: Vec<Entries>) -> Result<(), String> {
    let list = shortcuts.into_iter().enumerate().map(|(i, s)| (i.to_string(), Node::Map(s))).collect();
    let mut out = Vec::new();
    write_map(&vec![("shortcuts".into(), Node::Map(list))], &mut out);
    let dir = config_dir(steam, user);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    fs::write(dir.join("shortcuts.vdf"), out).map_err(|e| e.to_string())
}

// Steam itself writes "AppName" where Electron's writer used "appname", so look up case-insensitively.
fn field<'a>(shortcut: &'a Entries, name: &str) -> Option<&'a Node> {
    shortcut.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v)
}

fn str_field<'a>(shortcut: &'a Entries, name: &str) -> Option<&'a str> {
    match field(shortcut, name) {
        Some(Node::Str(s)) => Some(s),
        _ => None,
    }
}

pub fn app_id(shortcut: &Entries) -> Option<u32> {
    match field(shortcut, "appid") {
        Some(Node::Fixed(0x02, b)) => Some(u32::from_le_bytes(b.as_slice().try_into().ok()?)),
        _ => None,
    }
}

/// generateSteamShortcutAppId.
pub fn generate_app_id(exe: &str, title: &str) -> u32 {
    crc32fast::hash(format!("{exe}{title}").as_bytes()) | 0x8000_0000
}

fn int(v: u32) -> Node {
    Node::Fixed(0x02, v.to_le_bytes().to_vec())
}

/// composeSteamShortcut, field for field.
pub fn compose(title: &str, exe: &str, icon: Option<&Path>, open_vr: bool) -> Entries {
    let start_dir = Path::new(exe).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let s = |v: &str| Node::Str(v.into());
    vec![
        ("appid".into(), int(generate_app_id(exe, title))),
        ("appname".into(), s(title)),
        ("Exe".into(), s(&format!("\"{exe}\""))),
        ("StartDir".into(), s(&format!("\"{start_dir}\""))),
        ("icon".into(), s(&icon.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default())),
        ("ShortcutPath".into(), s("")),
        ("LaunchOptions".into(), s("")),
        ("IsHidden".into(), int(0)),
        ("AllowDesktopConfig".into(), int(1)),
        ("AllowOverlay".into(), int(1)),
        ("OpenVR".into(), int(open_vr as u32)),
        ("Devkit".into(), int(0)),
        ("DevkitGameID".into(), s("")),
        ("DevkitOverrideAppID".into(), int(0)),
        ("LastPlayTime".into(), int(0)),
        ("FlatpakAppID".into(), s("")),
    ]
}

/// matchesGameFallback for shop games. Electron compared against the quoted `Exe` as
/// stored, which never matched; this compares the unquoted path.
fn matches_game(game: &Value, shortcut: &Entries) -> bool {
    let exe = str_field(shortcut, "Exe").map(|e| e.trim_matches('"'));
    (game["executablePath"].as_str().is_some_and(|p| Some(p) == exe)) || game["title"].as_str().is_some_and(|t| Some(t) == str_field(shortcut, "appname"))
}

fn eligible(game: &Value) -> bool {
    // shortcut: classics (launchbox) shortcuts go through a chimaera:// deep link, which the Tauri shell does not handle yet.
    !game.is_null() && game["shop"] != "launchbox" && game["executablePath"].is_string()
}

/// checkSteamShortcut: whether any Steam user has a shortcut for the game; records its appid.
pub fn check(store: &Store, steam: Option<&Path>, key: &str) -> Result<bool, String> {
    let game = store.get("games", key)?;
    let Some(steam) = steam.filter(|_| eligible(&game)) else { return Ok(false) };
    let users = user_ids(steam);
    let all: Vec<Entries> = users.iter().filter_map(|u| read_shortcuts(steam, u).ok()).flatten().collect();
    let known = game["steamShortcutAppId"].as_u64();
    if known.is_some() && all.iter().any(|s| app_id(s).map(u64::from) == known) {
        return Ok(true);
    }
    let Some(found) = all.iter().find(|s| matches_game(&game, s)) else { return Ok(false) };
    if let (None, Some(id)) = (known, app_id(found)) {
        crate::games::update(store, key, |g| {
            g.insert("steamShortcutAppId".into(), json!(id));
        })?;
    }
    Ok(true)
}

/// Shortcut art, in the order createSteamShortcut downloads it: (file name, format).
pub const ASSETS: [(&str, ImageFormat); 5] =
    [("icon.ico", ImageFormat::Ico), ("hero.jpg", ImageFormat::Jpeg), ("logo.png", ImageFormat::Png), ("cover.jpg", ImageFormat::Jpeg), ("library.jpg", ImageFormat::Jpeg)];

/// resolveShortcutAssetUrls: the user's custom art first, then the shop's.
pub fn asset_urls(game: &Value, assets: &Value) -> [Option<String>; 5] {
    let pick = |custom: &str, shop: &str| game[custom].as_str().or(assets[shop].as_str()).map(String::from);
    [
        pick("customIconUrl", "iconUrl"),
        pick("customHeroImageUrl", "libraryHeroImageUrl"),
        pick("customLogoImageUrl", "logoImageUrl"),
        pick("customCoverImageUrl", "coverImageUrl"),
        assets["libraryImageUrl"].as_str().map(String::from),
    ]
}

/// The bytes behind a `local:` or http(s) asset URL; None on any failure, as in Electron.
pub async fn fetch_asset(url: &str) -> Option<Vec<u8>> {
    if let Some(path) = url.strip_prefix("local:") {
        return fs::read(path).ok();
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return None;
    }
    let response = reqwest::get(url).await.ok()?.error_for_status().ok()?;
    response.bytes().await.ok().map(|b| b.to_vec())
}

/// convertSteamShortcutAsset: ICO 256x256 cover-cropped, PNG, or JPEG q90 on black.
pub fn convert(source: &[u8], format: ImageFormat) -> Result<Vec<u8>, String> {
    if image::guess_format(source).ok() == Some(format) {
        return Ok(source.to_vec());
    }
    let image = image::load_from_memory(source).map_err(|e| e.to_string())?;
    let mut out = Cursor::new(Vec::new());
    match format {
        ImageFormat::Ico => image.resize_to_fill(256, 256, image::imageops::FilterType::Lanczos3).write_to(&mut out, ImageFormat::Ico),
        ImageFormat::Png => image.write_to(&mut out, ImageFormat::Png),
        _ => {
            let mut rgba = image.to_rgba8();
            for p in rgba.pixels_mut() {
                let a = p[3] as u16;
                for c in 0..3 {
                    p[c] = (p[c] as u16 * a / 255) as u8;
                }
            }
            let rgb = DynamicImage::ImageRgba8(rgba).to_rgb8();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90).encode_image(&rgb)
        }
    }
    .map_err(|e| e.to_string())?;
    Ok(out.into_inner())
}

/// createSteamShortcut after the downloads: writes the art, adds the shortcut for every
/// Steam user that lacks one with this title, and records the appid. Returns the appid.
pub fn create(store: &Store, steam: Option<&Path>, assets_dir: &Path, key: &str, sources: [Option<Vec<u8>>; 5], open_vr: bool) -> Result<Option<u32>, String> {
    let game = store.get("games", key)?;
    if game.is_null() {
        return Ok(None);
    }
    if game["shop"] == "launchbox" {
        return Err("Classic games cannot get a Steam shortcut yet".into());
    }
    let exe = game["executablePath"].as_str().ok_or("No executable path found for game")?;
    let title = game["title"].as_str().unwrap_or_default();
    let steam = steam.ok_or("No Steam user ID found")?;
    let users = user_ids(steam);
    if users.is_empty() {
        return Err("No Steam user ID found".into());
    }

    let game_dir = assets_dir.join(format!("{}-{}", game["shop"].as_str().unwrap_or_default(), game["objectId"].as_str().unwrap_or_default()));
    let files: Vec<Option<PathBuf>> = sources
        .into_iter()
        .zip(ASSETS)
        .map(|(source, (name, format))| {
            let data = convert(&source?, format).map_err(|e| eprintln!("[createSteamShortcut] {name}: {e}")).ok()?;
            fs::create_dir_all(&game_dir).ok()?;
            let path = game_dir.join(name);
            fs::write(&path, data).ok()?;
            Some(path)
        })
        .collect();

    let shortcut = compose(title, exe, files[0].as_deref(), open_vr);
    let id = app_id(&shortcut).unwrap_or_default();
    let grid_names = [format!("{id}_hero.jpg"), format!("{id}_logo.png"), format!("{id}p.jpg"), format!("{id}.jpg")];
    for user in &users {
        let mut shortcuts = read_shortcuts(steam, user)?;
        if shortcuts.iter().any(|s| str_field(s, "appname") == Some(title)) {
            continue;
        }
        let grid = config_dir(steam, user).join("grid");
        fs::create_dir_all(&grid).map_err(|e| e.to_string())?;
        for (file, name) in files[1..].iter().zip(&grid_names) {
            if let Some(file) = file {
                fs::copy(file, grid.join(name)).map_err(|e| e.to_string())?;
            }
        }
        shortcuts.push(shortcut.clone());
        write_shortcuts(steam, user, shortcuts)?;
    }

    // configureLinuxWinePrefix: Proton runs the shortcut in its own compatdata prefix.
    let wine_prefix = (cfg!(target_os = "linux") && !game["winePrefixPath"].is_string())
        .then(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/Steam/steamapps/compatdata").join(id.to_string()).join("pfx")))
        .flatten();
    if let Some(prefix) = &wine_prefix {
        fs::create_dir_all(prefix).map_err(|e| e.to_string())?;
    }
    crate::games::update(store, key, |g| {
        g.insert("steamShortcutAppId".into(), json!(id));
        if let Some(prefix) = wine_prefix {
            g.insert("winePrefixPath".into(), json!(prefix.to_string_lossy()));
        }
    })?;
    Ok(Some(id))
}

/// deleteSteamShortcut: removes the game's shortcut and grid art from every Steam user.
pub fn delete(store: &Store, steam: Option<&Path>, key: &str) -> Result<bool, String> {
    let game = store.get("games", key)?;
    let (Some(steam), Some(id)) = (steam, game["steamShortcutAppId"].as_u64().filter(|_| eligible(&game))) else { return Ok(false) };
    let users = user_ids(steam);
    if users.is_empty() {
        return Ok(false);
    }
    for user in &users {
        let shortcuts = read_shortcuts(steam, user)?;
        let before = shortcuts.len();
        let kept: Vec<Entries> = shortcuts.into_iter().filter(|s| app_id(s).map(u64::from) != Some(id)).collect();
        if kept.len() == before {
            continue;
        }
        write_shortcuts(steam, user, kept)?;
        let grid = config_dir(steam, user).join("grid");
        for name in [format!("{id}_hero.jpg"), format!("{id}_logo.png"), format!("{id}p.jpg"), format!("{id}.jpg"), format!("{id}.ico")] {
            let _ = fs::remove_file(grid.join(name));
        }
    }
    crate::games::update(store, key, |g| {
        g.remove("steamShortcutAppId");
    })?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steam_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chimaera-steam-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("userdata").join("42")).unwrap();
        fs::create_dir_all(dir.join("userdata").join("anonymous")).unwrap();
        dir
    }

    #[test]
    fn vdf_round_trips_unknown_fields() {
        let mut shortcut = compose("Game", r"C:\Games\game.exe", None, false);
        shortcut.push(("tags".into(), Node::Map(vec![("0".into(), Node::Str("fav".into()))])));
        shortcut.push(("Future".into(), Node::Fixed(0x07, vec![1, 2, 3, 4, 5, 6, 7, 8])));
        let dir = steam_dir("vdf");
        write_shortcuts(&dir, "42", vec![shortcut.clone()]).unwrap();
        assert_eq!(read_shortcuts(&dir, "42").unwrap(), vec![shortcut]);
        assert!(parse_map(&[0x05, b'x', 0], &mut 0).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn app_id_matches_electron() {
        // crc32("C:\\g.exeGame") with the high bit set, as generateSteamShortcutAppId computes it.
        let id = generate_app_id(r"C:\g.exe", "Game");
        assert_eq!(id, crc32fast::hash(br"C:\g.exeGame") | 0x8000_0000);
        assert!(id >= 0x8000_0000);
        assert_eq!(app_id(&compose("Game", r"C:\g.exe", None, false)), Some(id));
    }

    #[test]
    fn create_check_delete() {
        let dir = steam_dir("flow");
        let assets = dir.join("assets");
        let store = Store::memory().unwrap();
        let game = json!({ "shop": "steam", "objectId": "1", "title": "Game", "executablePath": r"C:\g.exe" });
        store.put("games", "steam:1", &game).unwrap();
        let mut png = Cursor::new(Vec::new());
        DynamicImage::new_rgba8(8, 4).write_to(&mut png, ImageFormat::Png).unwrap();
        let png = png.into_inner();

        assert_eq!(user_ids(&dir), vec!["42".to_string()]);
        let id = create(&store, Some(&dir), &assets, "steam:1", [Some(png.clone()), Some(png.clone()), Some(png), None, None], true).unwrap().unwrap();
        let grid = dir.join("userdata/42/config/grid");
        assert!(grid.join(format!("{id}_hero.jpg")).is_file() && grid.join(format!("{id}_logo.png")).is_file());
        assert_eq!(image::guess_format(&fs::read(assets.join("steam-1/icon.ico")).unwrap()).unwrap(), ImageFormat::Ico);
        assert_eq!(image::guess_format(&fs::read(grid.join(format!("{id}_hero.jpg"))).unwrap()).unwrap(), ImageFormat::Jpeg);
        // A second create for the same title adds nothing.
        create(&store, Some(&dir), &assets, "steam:1", [None, None, None, None, None], false).unwrap();
        assert_eq!(read_shortcuts(&dir, "42").unwrap().len(), 1);

        // check finds it by exe even when the record lost its appid.
        crate::games::update(&store, "steam:1", |g| {
            g.remove("steamShortcutAppId");
        })
        .unwrap();
        assert!(check(&store, Some(&dir), "steam:1").unwrap());
        assert_eq!(store.get("games", "steam:1").unwrap()["steamShortcutAppId"], json!(id));

        assert!(delete(&store, Some(&dir), "steam:1").unwrap());
        assert!(read_shortcuts(&dir, "42").unwrap().is_empty());
        assert!(!grid.join(format!("{id}_hero.jpg")).exists());
        assert!(store.get("games", "steam:1").unwrap().get("steamShortcutAppId").is_none());
        assert!(!check(&store, Some(&dir), "steam:1").unwrap());
        fs::remove_dir_all(&dir).unwrap();
    }
}
