//! Local notifications (the in-app list) and OS toasts. Ported from
//! src/main/services/notifications/local-notifications.ts and index.ts.

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::library::iso_now;
use crate::store::Store;

const KIND: &str = "localNotifications";

/// LocalNotificationManager.createNotification: stores it and tells the main window.
pub fn create(app: &AppHandle, kind: &str, title: &str, description: &str, picture_url: Option<&str>, url: Option<&str>) -> Result<Value, String> {
    let notification = new_notification(kind, title, description, picture_url, url);
    app.state::<Store>().put(KIND, notification["id"].as_str().unwrap_or_default(), &notification)?;
    let _ = app.emit_to("main", "on-local-notification-created", [&notification]);
    Ok(notification)
}

fn new_notification(kind: &str, title: &str, description: &str, picture_url: Option<&str>, url: Option<&str>) -> Value {
    let id: String = (0..16).map(|_| char::from_digit(fastrand::u32(0..16), 16).unwrap()).collect();
    json!({
        "id": id, "type": kind, "title": title, "description": description,
        "pictureUrl": picture_url, "url": url, "isRead": false, "createdAt": iso_now(),
    })
}

/// Newest first.
pub fn list(store: &Store) -> Result<Value, String> {
    let mut all: Vec<Value> = store.entries(KIND)?.into_iter().map(|(_, n)| n).collect();
    // ISO timestamps from one clock sort as strings.
    all.sort_by(|a, b| b["createdAt"].as_str().cmp(&a["createdAt"].as_str()));
    Ok(Value::Array(all))
}

pub fn unread_count(store: &Store) -> Result<usize, String> {
    Ok(store.entries(KIND)?.iter().filter(|(_, n)| !n["isRead"].as_bool().unwrap_or(false)).count())
}

pub fn set_read(store: &Store, id: &str, read: bool) -> Result<(), String> {
    let mut notification = store.get(KIND, id)?;
    if notification.is_object() {
        notification["isRead"] = json!(read);
        store.put(KIND, id, &notification)?;
    }
    Ok(())
}

pub fn mark_all_read(store: &Store) -> Result<(), String> {
    for (id, n) in store.entries(KIND)? {
        if !n["isRead"].as_bool().unwrap_or(false) {
            set_read(store, &id, true)?;
        }
    }
    Ok(())
}

pub fn delete(store: &Store, id: &str) -> Result<(), String> {
    store.del(KIND, id)
}

pub fn clear_all(store: &Store) -> Result<(), String> {
    store.clear(KIND)
}

/// An OS toast. Failures are logged, never raised: a toast is never worth failing the caller.
pub fn toast(app: &AppHandle, title: &str, body: &str) {
    if let Err(error) = app.notification().builder().title(title).body(body).show() {
        eprintln!("[notifications] toast failed: {error}");
    }
}

fn preference(store: &Store, name: &str) -> bool {
    store.get("", "userPreferences").ok().and_then(|p| p[name].as_bool()).unwrap_or(false)
}

/// publishNewRepacksNotification.
pub fn new_repacks(app: &AppHandle, count: i64) {
    let store = app.state::<Store>();
    if count < 1 || !preference(&store, "repackUpdatesNotificationsEnabled") {
        return;
    }
    let title = crate::i18n::t(&store, "notifications", "repack_list_updated", &[]);
    let body = crate::i18n::t(&store, "notifications", "repack_count", &[("count", &count.to_string())]);
    toast(app, &title, &body);
}

/// publishDownloadCompleteNotification: a toast when enabled, and always the in-app entry.
pub fn download_complete(app: &AppHandle, game: &Value) {
    let store = app.state::<Store>();
    let title = crate::i18n::t(&store, "notifications", "download_complete", &[]);
    let name = game["title"].as_str().unwrap_or_default();
    let body = crate::i18n::t(&store, "notifications", "game_ready_to_install", &[("title", name)]);
    if preference(&store, "downloadNotificationsEnabled") {
        toast(app, &title, &body);
    }
    let url = format!("/game/{}/{}", game["shop"].as_str().unwrap_or_default(), game["objectId"].as_str().unwrap_or_default());
    if let Err(error) = create(app, "DOWNLOAD_COMPLETE", &title, &body, game["iconUrl"].as_str(), Some(&url)) {
        eprintln!("[notifications] could not store the download notification: {error}");
    }
}

/// publishExtractionCompleteNotification: always a toast, plus the in-app entry.
pub fn extraction_complete(app: &AppHandle, game: &Value) {
    let store = app.state::<Store>();
    let title = crate::i18n::t(&store, "notifications", "extraction_complete", &[]);
    let body = crate::i18n::t(&store, "notifications", "game_extracted", &[("title", game["title"].as_str().unwrap_or_default())]);
    toast(app, &title, &body);
    let url = format!("/game/{}/{}", game["shop"].as_str().unwrap_or_default(), game["objectId"].as_str().unwrap_or_default());
    if let Err(error) = create(app, "EXTRACTION_COMPLETE", &title, &body, game["iconUrl"].as_str(), Some(&url)) {
        eprintln!("[notifications] could not store the extraction notification: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_state_listing_and_delete() {
        let store = Store::memory().unwrap();
        let mut old = new_notification("A", "old", "", None, None);
        old["createdAt"] = json!("2026-01-01T00:00:00.000Z");
        let new = new_notification("B", "new", "", None, Some("/x"));
        assert_eq!(new["id"].as_str().unwrap().len(), 16);
        for n in [&old, &new] {
            store.put(KIND, n["id"].as_str().unwrap(), n).unwrap();
        }
        assert_eq!(list(&store).unwrap()[0]["title"], "new");
        assert_eq!(unread_count(&store).unwrap(), 2);
        set_read(&store, old["id"].as_str().unwrap(), true).unwrap();
        assert_eq!(unread_count(&store).unwrap(), 1);
        set_read(&store, "missing", true).unwrap();
        assert!(store.get(KIND, "missing").unwrap().is_null());
        mark_all_read(&store).unwrap();
        assert_eq!(unread_count(&store).unwrap(), 0);
        delete(&store, new["id"].as_str().unwrap()).unwrap();
        assert_eq!(list(&store).unwrap().as_array().unwrap().len(), 1);
        clear_all(&store).unwrap();
        assert_eq!(list(&store).unwrap(), json!([]));
    }
}
