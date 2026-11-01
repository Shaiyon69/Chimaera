//! Download sources, ported from src/main/events/download-sources/. Chimaera has no
//! Hydra sign-in, so the profile sync those handlers do when subscribed is dropped.

use serde_json::{json, Value};

use crate::hydra_api;
use crate::library::iso_now;
use crate::store::Store;

const SUBLEVEL: &str = "downloadSources";

/// `getDownloadSources`: newest first. createdAt is ISO-8601, so string order is time order.
pub fn list(store: &Store) -> Result<Value, String> {
    let mut sources: Vec<Value> = store.entries(SUBLEVEL)?.into_iter().map(|(_, v)| v).collect();
    sources.sort_by(|a, b| b["createdAt"].as_str().unwrap_or_default().cmp(a["createdAt"].as_str().unwrap_or_default()));
    Ok(Value::Array(sources))
}

/// `addDownloadSource`: registers the URL with the API and stores what it returns.
pub async fn add(store: &Store, version: &str, url: &str) -> Result<Value, String> {
    if store.entries(SUBLEVEL)?.iter().any(|(_, s)| s["url"] == url) {
        return Err("Download source with this URL already exists".into());
    }
    let request = json!({ "method": "post", "url": "/download-sources", "data": { "url": url }, "options": { "needsAuth": false } });
    let source = hydra_api::call(store, version, &request).await?;
    let id = source["id"].as_str().ok_or("Invalid download source response")?.to_string();
    let mut stored = source.clone();
    stored["isRemote"] = json!(true);
    stored["createdAt"] = json!(iso_now());
    store.put(SUBLEVEL, &id, &stored)?;
    Ok(source)
}

/// `removeDownloadSource(removeAll, downloadSourceId)`.
pub fn remove(store: &Store, remove_all: bool, id: Option<&str>) -> Result<(), String> {
    match (remove_all, id) {
        (true, _) => store.clear(SUBLEVEL),
        (false, Some(id)) => store.del(SUBLEVEL, id),
        (false, None) => Ok(()),
    }
}

/// Each synced source over the stored one with the same id, as the handler's spread does.
fn merge(existing: &Value, synced: &Value) -> Value {
    let mut merged = existing.as_object().cloned().unwrap_or_default();
    if let Value::Object(fields) = synced {
        merged.extend(fields.clone());
    }
    Value::Object(merged)
}

/// `syncDownloadSources`: refreshes name, status and counts from the API.
pub async fn sync(store: &Store, version: &str) -> Result<(), String> {
    let existing = store.entries(SUBLEVEL)?;
    let ids: Vec<&str> = existing.iter().filter_map(|(_, s)| s["id"].as_str()).collect();
    let request = json!({ "method": "post", "url": "/download-sources/sync", "data": { "ids": ids }, "options": { "needsAuth": false } });
    let response = hydra_api::call(store, version, &request).await?;
    for synced in response.as_array().into_iter().flatten() {
        let Some(id) = synced["id"].as_str() else { continue };
        let current = existing.iter().find(|(_, s)| s["id"] == id).map_or(&Value::Null, |(_, s)| s);
        store.put(SUBLEVEL, id, &merge(current, synced))?;
    }
    Ok(())
}

/// Root timestamps the download-sources checker writes. Electron stored them as utf8,
/// and the baseline sometimes had quotes around it.
pub fn timestamp(store: &Store, key: &str) -> Result<Value, String> {
    Ok(match store.get("", key)? {
        Value::String(s) => json!(s.replace('"', "")),
        _ => Value::Null,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let dir = std::env::temp_dir().join(format!("chimaera-ds-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Store::open(&dir.join("t.db")).unwrap()
    }

    #[test]
    fn lists_newest_first_and_removes() {
        let s = store();
        s.put(SUBLEVEL, "a", &json!({ "id": "a", "createdAt": "2026-01-01T00:00:00.000Z" })).unwrap();
        s.put(SUBLEVEL, "b", &json!({ "id": "b", "createdAt": "2026-06-01T00:00:00.000Z" })).unwrap();
        assert_eq!(list(&s).unwrap()[0]["id"], "b");
        remove(&s, false, Some("b")).unwrap();
        assert_eq!(list(&s).unwrap().as_array().unwrap().len(), 1);
        remove(&s, true, None).unwrap();
        assert_eq!(list(&s).unwrap(), json!([]));
    }

    #[test]
    fn sync_keeps_local_fields() {
        let merged = merge(&json!({ "id": "a", "isRemote": true, "createdAt": "x", "name": "old" }), &json!({ "id": "a", "name": "new" }));
        assert_eq!(merged, json!({ "id": "a", "isRemote": true, "createdAt": "x", "name": "new" }));
    }

    #[test]
    fn timestamps_drop_quotes() {
        let s = store();
        s.put("", "downloadSourcesCheckBaseline", &json!("\"2026-10-31T00:00:00.000Z\"")).unwrap();
        assert_eq!(timestamp(&s, "downloadSourcesCheckBaseline").unwrap(), "2026-10-31T00:00:00.000Z");
        assert_eq!(timestamp(&s, "downloadSourcesSinceValue").unwrap(), Value::Null);
    }
}
