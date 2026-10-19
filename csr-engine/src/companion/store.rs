//! Key-value rows the relay's engine keeps between events and across restarts.
//!
//! One SQLite file under the user's `csr-companion` folder, WAL, shared by every `serve`
//! process. A row is `(sid, key)`; machine-wide rows use `sid = ""`. Values are JSON.
//! The file is opened on first use, so a `serve` that is never asked for state never makes it.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// How long a write waits for another process's write before it gives up.
const BUSY: Duration = Duration::from_secs(5);

/// The `sid` of a row that belongs to the machine, not to one session.
pub const MACHINE: &str = "";

pub struct Store {
    path: PathBuf,
    conn: Mutex<Option<Connection>>,
}

/// `<HOME>/.claude-self-reflect/csr-companion/companion.db`.
pub fn default_path() -> PathBuf {
    super::private::default_root().join("companion.db")
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.)
        .unwrap_or(0.)
}

fn open(path: &Path) -> Result<Connection> {
    let folder = path.parent().context("the store path has no folder")?;
    // The file is made 0600 before SQLite sees it; SQLite gives `-wal` and `-shm` its mode.
    super::private::open_private(path, true, folder)
        .with_context(|| format!("preparing {}", path.display()))?;
    let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    conn.busy_timeout(BUSY)?;
    conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0))?;
    conn.execute_batch(
        "PRAGMA synchronous=NORMAL;
         CREATE TABLE IF NOT EXISTS kv (
             sid TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL, at REAL NOT NULL,
             PRIMARY KEY (sid, key)
         );",
    )?;
    for suffix in ["", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let _ =
            std::fs::set_permissions(PathBuf::from(name), std::fs::Permissions::from_mode(0o600));
    }
    Ok(conn)
}

impl Store {
    /// A store at `path`. Nothing is made or opened until the first call.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            conn: Mutex::new(None),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn with<T>(&self, work: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let mut slot = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(open(&self.path)?);
        }
        let conn = slot.as_ref().expect("opened just above");
        work(conn).with_context(|| format!("reading or writing {}", self.path.display()))
    }

    pub fn get(&self, sid: &str, key: &str) -> Result<Option<Value>> {
        let text: Option<String> = self.with(|conn| {
            conn.query_row(
                "SELECT value FROM kv WHERE sid = ?1 AND key = ?2",
                params![sid, key],
                |row| row.get(0),
            )
            .optional()
        })?;
        text.map(|text| serde_json::from_str(&text).context("a stored value is not JSON"))
            .transpose()
    }

    pub fn set(&self, sid: &str, key: &str, value: &Value) -> Result<()> {
        let text = serde_json::to_string(value)?;
        self.with(|conn| {
            conn.execute(
                "INSERT INTO kv (sid, key, value, at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (sid, key) DO UPDATE SET value = excluded.value, at = excluded.at",
                params![sid, key, text, now_ms()],
            )
            .map(drop)
        })
    }

    pub fn delete(&self, sid: &str, key: &str) -> Result<()> {
        self.with(|conn| {
            conn.execute(
                "DELETE FROM kv WHERE sid = ?1 AND key = ?2",
                params![sid, key],
            )
            .map(drop)
        })
    }

    /// The keys of a session that start with `prefix`, in order.
    pub fn keys(&self, sid: &str, prefix: &str) -> Result<Vec<String>> {
        self.with(|conn| {
            let mut ask = conn.prepare(
                "SELECT key FROM kv WHERE sid = ?1 AND substr(key, 1, length(?2)) = ?2
                 ORDER BY key",
            )?;
            let rows = ask.query_map(params![sid, prefix], |row| row.get(0))?;
            rows.collect()
        })
    }

    /// Every sid that has a row, in order; the machine's own rows show as `MACHINE`.
    pub fn sids(&self) -> Result<Vec<String>> {
        self.with(|conn| {
            let mut ask = conn.prepare("SELECT DISTINCT sid FROM kv ORDER BY sid")?;
            let rows = ask.query_map([], |row| row.get(0))?;
            rows.collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn path() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("csr-companion/companion.db");
        (dir, path)
    }

    #[test]
    fn nothing_is_made_until_the_first_call() {
        let (_dir, path) = path();
        let store = Store::at(&path);
        assert!(!path.exists());
        assert_eq!(store.get("s", "k").unwrap(), None);
        assert!(path.exists());
    }

    #[test]
    fn a_value_survives_a_close_and_a_second_open() {
        let (_dir, path) = path();
        let first = Store::at(&path);
        first
            .set("s1", "pace", &json!({ "steps": 3, "words": ["a"] }))
            .unwrap();
        first.set(MACHINE, "statusline", &json!("wrapped")).unwrap();
        drop(first);
        let second = Store::at(&path);
        assert_eq!(
            second.get("s1", "pace").unwrap(),
            Some(json!({ "steps": 3, "words": ["a"] }))
        );
        assert_eq!(
            second.get(MACHINE, "statusline").unwrap(),
            Some(json!("wrapped"))
        );
        assert_eq!(second.get("s2", "pace").unwrap(), None);
    }

    #[test]
    fn set_replaces_and_delete_removes_and_keys_follow_a_prefix() {
        let (_dir, path) = path();
        let store = Store::at(&path);
        store.set("s", "setup:dreams", &json!(1)).unwrap();
        store.set("s", "setup:dreams", &json!(2)).unwrap();
        store.set("s", "setup:compact", &json!(3)).unwrap();
        store.set("s", "pace", &json!(4)).unwrap();
        store.set("t", "setup:other", &json!(5)).unwrap();
        assert_eq!(store.get("s", "setup:dreams").unwrap(), Some(json!(2)));
        assert_eq!(
            store.keys("s", "setup:").unwrap(),
            ["setup:compact", "setup:dreams"]
        );
        assert_eq!(store.keys("s", "").unwrap().len(), 3);
        // A prefix with SQL wildcards matches nothing it should not.
        assert!(store.keys("s", "%").unwrap().is_empty());
        store.delete("s", "setup:dreams").unwrap();
        store.delete("s", "never-set").unwrap();
        assert_eq!(store.get("s", "setup:dreams").unwrap(), None);
        assert_eq!(store.keys("t", "").unwrap(), ["setup:other"]);
    }

    #[test]
    fn sids_lists_each_sid_with_a_row_once_and_forgets_one_with_none_left() {
        let (_dir, path) = path();
        let store = Store::at(&path);
        assert!(store.sids().unwrap().is_empty());
        store.set("b", "pace", &json!(1)).unwrap();
        store.set("b", "chapter", &json!(2)).unwrap();
        store.set("a", "pace", &json!(3)).unwrap();
        store.set(MACHINE, "seenDreams", &json!([])).unwrap();
        assert_eq!(store.sids().unwrap(), [MACHINE, "a", "b"]);
        store.delete("a", "pace").unwrap();
        assert_eq!(store.sids().unwrap(), [MACHINE, "b"]);
    }

    #[test]
    fn the_folder_is_0700_and_every_file_0600() {
        let (_dir, path) = path();
        let store = Store::at(&path);
        store.set("s", "k", &json!(1)).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        assert_eq!(mode(&path), 0o600);
        let mut wal = path.as_os_str().to_os_string();
        wal.push("-wal");
        assert_eq!(mode(Path::new(&wal)), 0o600);
        let journal: String = Connection::open(&path)
            .unwrap()
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal, "wal");
    }

    #[test]
    fn two_stores_on_one_file_share_it_while_both_write() {
        let (_dir, path) = path();
        let handles: Vec<_> = (0..4)
            .map(|n| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let store = Store::at(path);
                    for i in 0..50 {
                        store.set("s", &format!("w{n}-{i}"), &json!(i)).unwrap();
                        store.get("s", &format!("w{}-{i}", (n + 1) % 4)).unwrap();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(Store::at(&path).keys("s", "w").unwrap().len(), 200);
    }

    #[test]
    fn a_stored_value_that_is_not_json_is_an_error() {
        let (_dir, path) = path();
        let store = Store::at(&path);
        store.set("s", "k", &json!(1)).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute("UPDATE kv SET value = '{nope'", [])
            .unwrap();
        assert!(store.get("s", "k").is_err());
    }
}
