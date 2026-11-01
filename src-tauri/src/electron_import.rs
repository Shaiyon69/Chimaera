//! First-run import of the Electron build's data: the LevelDB (`hydra-db`, which
//! also holds an imported Hydra backup: profile, collections, preferences) and the
//! SQLite library (`chimaera.sqlite`: games and play sessions). It reads copies,
//! never the originals, so the Electron app keeps working and can run alongside.

use rusqlite::{Connection, OpenFlags};
use rusty_leveldb::{LdbIterator, Options, DB};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::store::Store;

const MARKER: &str = "electronImport";

/// Electron's userData for productName "chimaera": %APPDATA%\chimaera,
/// ~/.config/chimaera or ~/Library/Application Support/chimaera.
pub fn electron_user_data(config_dir: &Path) -> PathBuf {
    config_dir.join("chimaera")
}

/// classic-level keys a sublevel entry as `!sublevel!key`; root keys are bare.
pub fn split_key(raw: &str) -> (&str, &str) {
    raw.strip_prefix('!').and_then(|rest| rest.split_once('!')).unwrap_or(("", raw))
}

/// Values were written as JSON, except a few root keys stored as utf8 (`language`).
pub fn decode_value(raw: &[u8]) -> Value {
    let text = String::from_utf8_lossy(raw);
    serde_json::from_str(&text).unwrap_or_else(|_| Value::String(text.into_owned()))
}

pub(crate) fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        // The LOCK belongs to a running Electron; the copy must not inherit it.
        if entry.file_type()?.is_file() && entry.file_name() != "LOCK" {
            std::fs::copy(entry.path(), to.join(entry.file_name()))?;
        }
    }
    Ok(())
}

fn import_leveldb(store: &Store, dir: &Path) -> Result<usize, String> {
    let mut options = Options::default();
    options.create_if_missing = false;
    let mut db = DB::open(dir, options).map_err(|e| format!("open LevelDB copy: {e}"))?;
    let mut iter = db.new_iter().map_err(|e| e.to_string())?;
    let mut count = 0;
    while let Some((key, value)) = iter.next() {
        let key = String::from_utf8_lossy(&key);
        let (sublevel, key) = split_key(&key);
        // The library moved to SQLite; its LevelDB copy is a stale pre-migration fallback.
        if sublevel == "games" {
            continue;
        }
        store.put(sublevel, key, &decode_value(&value))?;
        count += 1;
    }
    Ok(count)
}

fn import_sqlite(store: &Store, path: &Path) -> Result<(usize, usize), String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
    let mut games = conn.prepare("SELECT key, data FROM games").map_err(|e| e.to_string())?;
    let rows = games
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?;
    let mut game_count = 0;
    for row in rows {
        let (key, data) = row.map_err(|e| e.to_string())?;
        store.put("games", &key, &decode_value(data.as_bytes()))?;
        game_count += 1;
    }
    let mut sessions = conn
        .prepare("SELECT id, game_key, started_at, ended_at, duration_ms FROM play_sessions")
        .map_err(|e| e.to_string())?;
    let rows = sessions
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                json!({ "gameKey": r.get::<_, String>(1)?, "startedAt": r.get::<_, i64>(2)?, "endedAt": r.get::<_, i64>(3)?, "durationMs": r.get::<_, i64>(4)? }),
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut session_count = 0;
    for row in rows {
        let (id, session) = row.map_err(|e| e.to_string())?;
        // Zero-padded so the store's key order is session order.
        store.put("playSessions", &format!("{id:012}"), &session)?;
        session_count += 1;
    }
    Ok((game_count, session_count))
}

/// Imports once; later launches return the recorded result. Ok(None) when there
/// is no Electron data on this machine.
pub fn run_once(store: &Store, user_data: &Path, scratch: &Path) -> Result<Option<Value>, String> {
    let done = store.get("", MARKER)?;
    if !done.is_null() {
        return Ok(Some(done));
    }
    let level_dir = user_data.join("hydra-db");
    let sqlite = user_data.join("chimaera.sqlite");
    if !level_dir.is_dir() && !sqlite.is_file() {
        return Ok(None);
    }

    let _ = std::fs::remove_dir_all(scratch);
    std::fs::create_dir_all(scratch).map_err(|e| e.to_string())?;
    let mut keys = 0;
    if level_dir.is_dir() {
        let copy = scratch.join("hydra-db");
        copy_dir(&level_dir, &copy).map_err(|e| e.to_string())?;
        keys = import_leveldb(store, &copy)?;
    }
    let (mut games, mut sessions) = (0, 0);
    if sqlite.is_file() {
        // The WAL files hold recent writes, so they travel with the database.
        for suffix in ["", "-wal", "-shm"] {
            let from = user_data.join(format!("chimaera.sqlite{suffix}"));
            if from.is_file() {
                std::fs::copy(&from, scratch.join(format!("chimaera.sqlite{suffix}"))).map_err(|e| e.to_string())?;
            }
        }
        (games, sessions) = import_sqlite(store, &scratch.join("chimaera.sqlite"))?;
    }
    let _ = std::fs::remove_dir_all(scratch);

    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let result = json!({ "importedAt": at, "from": user_data.to_string_lossy(), "keys": keys, "games": games, "playSessions": sessions });
    store.put("", MARKER, &result)?;
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_sublevel_keys() {
        assert_eq!(split_key("!games!steam:620"), ("games", "steam:620"));
        assert_eq!(split_key("!gameShopCache!steam:620:en"), ("gameShopCache", "steam:620:en"));
        assert_eq!(split_key("userPreferences"), ("", "userPreferences"));
    }

    #[test]
    fn decodes_json_and_utf8_values() {
        assert_eq!(decode_value(br#"{"a":1}"#), json!({ "a": 1 }));
        assert_eq!(decode_value(b"pt-BR"), json!("pt-BR"));
        assert_eq!(decode_value(b"\"en\""), json!("en"));
    }

    #[test]
    fn imports_sqlite_library_and_runs_once() {
        let dir = std::env::temp_dir().join(format!("chimaera-import-test-{}", fastrand::u64(..)));
        let user_data = dir.join("userData");
        std::fs::create_dir_all(&user_data).unwrap();
        let conn = Connection::open(user_data.join("chimaera.sqlite")).unwrap();
        conn.execute_batch(
            "CREATE TABLE games (key TEXT PRIMARY KEY, data TEXT NOT NULL);
             CREATE TABLE play_sessions (id INTEGER PRIMARY KEY, game_key TEXT, started_at INTEGER, ended_at INTEGER, duration_ms INTEGER);
             INSERT INTO games VALUES ('steam:620', '{\"title\":\"Portal 2\",\"shop\":\"steam\",\"objectId\":\"620\"}');
             INSERT INTO play_sessions VALUES (1, 'steam:620', 10, 20, 10);",
        )
        .unwrap();
        drop(conn);

        let store = Store::open(&dir.join("store.db")).unwrap();
        let first = run_once(&store, &user_data, &dir.join("scratch")).unwrap().unwrap();
        assert_eq!((first["games"].as_u64(), first["playSessions"].as_u64()), (Some(1), Some(1)));
        assert_eq!(store.get("games", "steam:620").unwrap()["title"], "Portal 2");
        // The original stays put and a second run does not import again.
        assert!(user_data.join("chimaera.sqlite").is_file());
        store.put("games", "steam:620", &json!({ "title": "edited" })).unwrap();
        run_once(&store, &user_data, &dir.join("scratch")).unwrap();
        assert_eq!(store.get("games", "steam:620").unwrap()["title"], "edited");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
