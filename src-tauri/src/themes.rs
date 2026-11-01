//! Custom themes (ports of `src/main/events/themes/`): the `themes` store, the
//! per-theme achievement sound under `<data>/themes/`, and the editor windows.

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::library::iso_now;
use crate::store::Store;

const SUBLEVEL: &str = "themes";
const SOUND_FORMATS: &[&str] = &["wav", "mp3", "ogg", "m4a"];

/// `<data>/themes`; Electron's folder while it is the only one, so its sounds keep playing.
fn themes_root(app: &AppHandle) -> Result<PathBuf, String> {
    crate::save_backups::data_dir(app, "themes")
}

fn sanitize_folder_name(name: &str) -> String {
    let kept: String = name.to_lowercase().chars().filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_') || c.is_whitespace()).collect();
    let dashed = kept.split_whitespace().collect::<Vec<_>>().join("-");
    let mut out = String::new();
    for c in dashed.chars() {
        if !(c == '-' && out.ends_with('-')) {
            out.push(c);
        }
    }
    out.trim_matches('-').to_string()
}

/// getThemePath: the folder named after the theme, else its id.
fn theme_dir(root: &Path, id: &str, name: Option<&str>) -> PathBuf {
    match name.map(sanitize_folder_name).filter(|n| !n.is_empty()) {
        Some(folder) => root.join(folder),
        None => root.join(id),
    }
}

fn sound_in(dir: &Path) -> Option<PathBuf> {
    SOUND_FORMATS.iter().map(|f| dir.join(format!("achievement.{f}"))).find(|p| p.is_file())
}

/// getThemeSoundPath: the named folder first, then the legacy id folder.
fn sound_path(root: &Path, id: &str, name: Option<&str>) -> Option<PathBuf> {
    sound_in(&theme_dir(root, id, name)).or_else(|| name.and_then(|_| sound_in(&root.join(id))))
}

fn get(store: &Store, id: &str) -> Result<Value, String> {
    match store.get(SUBLEVEL, id)? {
        theme @ Value::Object(_) => Ok(theme),
        _ => Err("Theme not found".into()),
    }
}

/// Puts `theme` with `changes` merged and a fresh `updatedAt`.
fn put(store: &Store, id: &str, mut theme: Value, changes: Value) -> Result<(), String> {
    if let (Some(t), Value::Object(c)) = (theme.as_object_mut(), changes) {
        t.extend(c);
        t.insert("updatedAt".into(), json!(iso_now()));
    }
    store.put(SUBLEVEL, id, &theme)
}

pub fn all(store: &Store) -> Result<Value, String> {
    Ok(Value::Array(store.entries(SUBLEVEL)?.into_iter().map(|(_, v)| v).collect()))
}

pub fn active(store: &Store) -> Result<Value, String> {
    Ok(store.entries(SUBLEVEL)?.into_iter().map(|(_, v)| v).find(|t| t["isActive"] == json!(true)).unwrap_or(Value::Null))
}

pub fn add(store: &Store, theme: &Value) -> Result<(), String> {
    let id = theme["id"].as_str().filter(|id| !id.is_empty()).ok_or("Theme id missing")?;
    store.put(SUBLEVEL, id, theme)
}

pub fn toggle(app: &AppHandle, id: &str, is_active: bool) -> Result<(), String> {
    let store = app.state::<Store>();
    put(&store, id, get(&store, id)?, json!({ "isActive": is_active }))?;
    // Electron told only the achievement pop-up; the main window ignores this for a toggle it made itself.
    let _ = app.emit("on-custom-theme-updated", ());
    Ok(())
}

pub fn update_code(app: &AppHandle, id: &str, code: &str) -> Result<(), String> {
    let store = app.state::<Store>();
    let theme = get(&store, id)?;
    let was_active = theme["isActive"] == json!(true);
    put(&store, id, theme, json!({ "code": code }))?;
    if was_active {
        let _ = app.emit("on-custom-theme-updated", ());
    }
    Ok(())
}

pub fn sound_path_of(app: &AppHandle, id: &str) -> Result<Value, String> {
    let name = app.state::<Store>().get(SUBLEVEL, id)?["name"].as_str().map(str::to_string);
    Ok(json!(sound_path(&themes_root(app)?, id, name.as_deref()).map(|p| p.to_string_lossy().into_owned())))
}

fn mime_of(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("wav") => "audio/wav",
        Some("ogg") => "audio/ogg",
        Some("m4a") => "audio/mp4",
        _ => "audio/mpeg",
    }
}

/// `getThemeSoundDataUrl`: null on any failure, like Electron.
pub fn sound_data_url(app: &AppHandle, id: &str) -> Value {
    let Some(path) = sound_path_of(app, id).ok().and_then(|v| v.as_str().map(PathBuf::from)) else { return Value::Null };
    match fs::read(&path) {
        Ok(bytes) => json!(format!("data:{};base64,{}", mime_of(&path), base64::engine::general_purpose::STANDARD.encode(bytes))),
        Err(_) => Value::Null,
    }
}

pub fn copy_sound(app: &AppHandle, id: &str, source: &str) -> Result<(), String> {
    let source_path = Path::new(source);
    if source.is_empty() || !source_path.is_file() {
        return Err("Source file does not exist".into());
    }
    let store = app.state::<Store>();
    let theme = get(&store, id)?;
    let dir = theme_dir(&themes_root(app)?, id, theme["name"].as_str());
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let ext = source_path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    fs::copy(source_path, dir.join(format!("achievement{ext}"))).map_err(|e| e.to_string())?;
    put(&store, id, theme, json!({ "hasCustomSound": true, "originalSoundPath": source }))
}

pub fn remove_sound(app: &AppHandle, id: &str) -> Result<(), String> {
    let store = app.state::<Store>();
    let theme = get(&store, id)?;
    let root = themes_root(app)?;
    for dir in [theme_dir(&root, id, theme["name"].as_str()), root.join(id)] {
        for format in SOUND_FORMATS {
            let _ = fs::remove_file(dir.join(format!("achievement.{format}")));
        }
    }
    let mut theme = theme;
    if let Some(t) = theme.as_object_mut() {
        t.remove("originalSoundPath");
    }
    put(&store, id, theme, json!({ "hasCustomSound": false }))
}

/// `importThemeSoundFromStore`: the first format the theme store has; none found is not an error.
pub async fn import_sound_from_store(app: &AppHandle, id: &str, theme_name: &str, store_url: &str) -> Result<(), String> {
    let theme = get(&app.state::<Store>(), id)?;
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build().map_err(|e| e.to_string())?;
    for format in SOUND_FORMATS {
        let url = format!("{store_url}/themes/{}/achievement.{format}", theme_name.to_lowercase());
        let Ok(response) = client.get(&url).send().await.and_then(|r| r.error_for_status()) else { continue };
        let Ok(bytes) = response.bytes().await else { continue };
        let dir = theme_dir(&themes_root(app)?, id, theme["name"].as_str());
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        fs::write(dir.join(format!("achievement.{format}")), &bytes).map_err(|e| e.to_string())?;
        return put(&app.state::<Store>(), id, theme, json!({ "hasCustomSound": true }));
    }
    eprintln!("[themes] no sound file found for theme {theme_name} in store");
    Ok(())
}

const EDITOR_PREFIX: &str = "theme-editor-";

fn editor_label(id: &str) -> String {
    let safe: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    format!("{EDITOR_PREFIX}{safe}")
}

/// `openEditorWindow`: one per theme; native title bar, since the editor page draws none.
pub fn open_editor(app: &AppHandle, id: &str) -> Result<(), String> {
    let label = editor_label(id);
    if let Some(existing) = app.get_webview_window(&label) {
        let _ = existing.unminimize();
        let _ = existing.set_focus();
        return Ok(());
    }
    let query = url::form_urlencoded::Serializer::new(String::new()).append_pair("themeId", id).finish();
    WebviewWindowBuilder::new(app, label, WebviewUrl::App(format!("tauri.html#/theme-editor?{query}").into()))
        .title("Chimaera")
        .inner_size(720.0, 720.0)
        .min_inner_size(600.0, 540.0)
        .background_color(tauri::webview::Color(0x0c, 0x0c, 0x0c, 0xff))
        .initialization_script(crate::process_script())
        .build()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// `closeEditorWindow`: one theme's editor, or all of them.
pub fn close_editor(app: &AppHandle, id: Option<&str>) {
    let wanted = id.map(editor_label);
    for (label, window) in app.webview_windows() {
        if label.starts_with(EDITOR_PREFIX) && wanted.as_ref().is_none_or(|w| *w == label) {
            let _ = window.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names_match_electron() {
        assert_eq!(sanitize_folder_name("  My Cool  Theme!! "), "my-cool-theme");
        assert_eq!(sanitize_folder_name("--a__b--"), "a__b");
        assert_eq!(sanitize_folder_name("日本"), "");
        let root = Path::new("r");
        assert_eq!(theme_dir(root, "id1", Some("日本")), root.join("id1"));
        assert_eq!(theme_dir(root, "id1", Some("Neon Night")), root.join("neon-night"));
        assert_eq!(editor_label("a b/c"), "theme-editor-a_b_c");
    }

    #[test]
    fn finds_sound_in_named_then_legacy_folder() {
        let root = std::env::temp_dir().join(format!("chimaera-themes-test-{}", crate::library::now_ms()));
        fs::create_dir_all(root.join("id1")).unwrap();
        fs::write(root.join("id1").join("achievement.ogg"), b"x").unwrap();
        assert_eq!(sound_path(&root, "id1", Some("Neon")), Some(root.join("id1").join("achievement.ogg")));
        fs::create_dir_all(root.join("neon")).unwrap();
        fs::write(root.join("neon").join("achievement.mp3"), b"x").unwrap();
        let found = sound_path(&root, "id1", Some("Neon")).unwrap();
        assert_eq!(found, root.join("neon").join("achievement.mp3"));
        assert_eq!(mime_of(&found), "audio/mpeg");
        assert_eq!(sound_path(&root, "id2", None), None);
        fs::remove_dir_all(root).unwrap();
    }
}
