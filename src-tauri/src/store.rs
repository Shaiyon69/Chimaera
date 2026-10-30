//! Key-value persistence that replaces the Electron LevelDB (`src/main/level`).
//! One SQLite table keyed by (sublevel, key); root keys use sublevel "". Values
//! are JSON text, so "utf8" values are stored as JSON strings. The first-run
//! import of an existing LevelDB fills this same table (milestone F10).

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::path::Path;
use std::sync::Mutex;

pub struct Store(Mutex<Connection>);

// Renderer allowlist, the same sets as assertRendererAccess
// (src/main/events/leveldb/helpers.ts). Everything else is main-process only.
const READABLE_ROOT_KEYS: &[&str] = &["userPreferences", "downloadSourcesSinceValue", "searchHistory"];
const WRITABLE_ROOT_KEYS: &[&str] = &["searchHistory"];
const READABLE_SUBLEVELS: &[&str] = &["themes", "games", "downloadSources"];
const WRITABLE_SUBLEVELS: &[&str] = &["themes", "games"];

pub fn assert_renderer_access(write: bool, sublevel: Option<&str>, key: Option<&str>) -> Result<(), String> {
    let allowed = match (sublevel, key) {
        (Some(s), _) => (if write { WRITABLE_SUBLEVELS } else { READABLE_SUBLEVELS }).contains(&s),
        (None, Some(k)) => (if write { WRITABLE_ROOT_KEYS } else { READABLE_ROOT_KEYS }).contains(&k),
        (None, None) => false,
    };
    if allowed {
        return Ok(());
    }
    let name = match sublevel {
        Some(s) => format!("{s}/*"),
        None => format!("root/{}", key.unwrap_or_default()),
    };
    Err(format!("LevelDB access denied: {name}"))
}

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Self::init(Connection::open(path)?)
    }

    #[cfg(test)]
    pub(crate) fn memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS kv (
               sublevel TEXT NOT NULL,
               key TEXT NOT NULL,
               value TEXT NOT NULL,
               PRIMARY KEY (sublevel, key)
             );",
        )?;
        Ok(Self(Mutex::new(conn)))
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        // A panic while holding the lock leaves the connection usable; keep going.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn get(&self, sublevel: &str, key: &str) -> Result<Value, String> {
        let text: Option<String> = self
            .conn()
            .query_row("SELECT value FROM kv WHERE sublevel = ?1 AND key = ?2", params![sublevel, key], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?;
        Ok(text.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null))
    }

    pub fn put(&self, sublevel: &str, key: &str, value: &Value) -> Result<(), String> {
        self.conn()
            .execute(
                "INSERT INTO kv (sublevel, key, value) VALUES (?1, ?2, ?3)
                 ON CONFLICT (sublevel, key) DO UPDATE SET value = excluded.value",
                params![sublevel, key, value.to_string()],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn del(&self, sublevel: &str, key: &str) -> Result<(), String> {
        self.conn()
            .execute("DELETE FROM kv WHERE sublevel = ?1 AND key = ?2", params![sublevel, key])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn clear(&self, sublevel: &str) -> Result<(), String> {
        self.conn().execute("DELETE FROM kv WHERE sublevel = ?1", params![sublevel]).map(|_| ()).map_err(|e| e.to_string())
    }

    /// (key, value) pairs in key order, like a LevelDB iterator.
    pub fn entries(&self, sublevel: &str) -> Result<Vec<(String, Value)>, String> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT key, value FROM kv WHERE sublevel = ?1 ORDER BY key").map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![sublevel], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        rows.map(|row| {
            let (key, text) = row.map_err(|e| e.to_string())?;
            Ok((key, serde_json::from_str(&text).unwrap_or(Value::Null)))
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_and_orders_like_leveldb() {
        let store = Store::memory().unwrap();
        assert_eq!(store.get("", "userPreferences").unwrap(), Value::Null);
        store.put("games", "steam:b", &json!({ "title": "B" })).unwrap();
        store.put("games", "steam:a", &json!({ "title": "A" })).unwrap();
        store.put("games", "steam:a", &json!({ "title": "A2" })).unwrap();
        let keys: Vec<_> = store.entries("games").unwrap().into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["steam:a", "steam:b"]);
        assert_eq!(store.get("games", "steam:a").unwrap()["title"], "A2");
        store.del("games", "steam:a").unwrap();
        store.clear("games").unwrap();
        assert!(store.entries("games").unwrap().is_empty());
    }

    #[test]
    fn keeps_the_electron_allowlist() {
        assert!(assert_renderer_access(false, None, Some("userPreferences")).is_ok());
        assert!(assert_renderer_access(true, None, Some("userPreferences")).is_err());
        assert!(assert_renderer_access(false, None, Some("auth")).is_err());
        assert!(assert_renderer_access(false, Some("downloadSources"), None).is_ok());
        assert!(assert_renderer_access(true, Some("downloadSources"), None).is_err());
        assert!(assert_renderer_access(false, Some("steamAccount"), None).is_err());
    }
}
