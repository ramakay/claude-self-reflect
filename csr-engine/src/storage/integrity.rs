//! The database integrity verdict: `PRAGMA integrity_check` plus the
//! keyword index's agreement with the chunks it indexes, cached in `meta`
//! because both walk the whole store.
//!
//! `chunks_fts` is an external-content FTS5 table over `chunks`. A rowid it
//! still indexes after its chunk row is gone (a delete the trigger never saw)
//! passes `PRAGMA integrity_check`, yet every keyword search that matches it
//! fails reading the chunk ("database disk image is malformed" from
//! `highlight`). FTS5 keeps one `chunks_fts_docsize` row per indexed rowid, so
//! the dangling rowids are the docsize rows with no chunk.
//!
//! The daemon's hourly refresh only counts them. `status --deep`, the
//! foreground command the unhealthy reason names, removes them on its own
//! connection ([`fts_repair`]), recounts, and stores what is left.

use std::path::Path;

use anyhow::Result;
use rusqlite::{Connection, OpenFlags, OptionalExtension};

use super::{queries, Storage, DEFAULT_BUSY_TIMEOUT};

mod fts_repair;
pub use fts_repair::FtsRepair;

/// `meta` key: dangling `chunks_fts` rowids counted with the last verdict.
const META_FTS_DANGLING: &str = "integrity_fts_dangling";

/// Why a verdict reads unhealthy and the command that fixes it; `None` when
/// it does not read unhealthy.
pub fn unhealthy_reason(healthy: Option<bool>, fts_dangling_rowids: Option<u64>) -> Option<String> {
    if healthy != Some(false) {
        return None;
    }
    Some(match fts_dangling_rowids {
        Some(rows) if rows > 0 => format!(
            "{rows} keyword-index rows point at deleted chunks; fix: csr-engine status --deep"
        ),
        _ => "SQLite integrity_check failed; recheck: csr-engine status --deep".to_string(),
    })
}

/// Add `health_reason` ([`unhealthy_reason`]) next to `healthy` in a
/// serialized status report; anything without a `healthy` key is unchanged.
pub fn add_health_reason(status: &mut serde_json::Value) {
    let Some(object) = status.as_object_mut() else {
        return;
    };
    if !object.contains_key("healthy") {
        return;
    }
    let reason = unhealthy_reason(
        object.get("healthy").and_then(serde_json::Value::as_bool),
        object
            .get("fts_dangling_rowids")
            .and_then(serde_json::Value::as_u64),
    );
    object.insert("health_reason".into(), reason.into());
}

/// When `verdict` counted dangling rowids, remove them on `conn` (a rebuild
/// if the targeted pass cannot), recount into `verdict`, log one line. A
/// failed repair keeps the verdict as measured.
fn repair_if_dangling(conn: &Connection, verdict: &mut IntegrityVerdict) {
    if verdict.fts_dangling_rowids == 0 {
        return;
    }
    let started = std::time::Instant::now();
    let repaired = fts_repair::repair_dangling(conn, true).and_then(|repair| {
        verdict.fts_dangling_rowids = fts_dangling_rowids(conn)?;
        Ok(repair)
    });
    match repaired {
        Ok(repair) => tracing::info!(
            found = repair.found,
            removed = repair.removed,
            unresolved = repair.unresolved,
            rebuilt = repair.rebuilt,
            left = verdict.fts_dangling_rowids,
            ms = started.elapsed().as_millis() as u64,
            "removed keyword-index rows whose chunk was gone"
        ),
        Err(error) => tracing::warn!(%error, "keyword-index repair failed"),
    }
}

/// One integrity measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegrityVerdict {
    /// `PRAGMA integrity_check` returned `ok`.
    pub sqlite_ok: bool,
    /// `chunks_fts` rowids with no `chunks` row.
    pub fts_dangling_rowids: u64,
}

impl IntegrityVerdict {
    pub fn healthy(&self) -> bool {
        self.sqlite_ok && self.fts_dangling_rowids == 0
    }
}

/// Indexed rowids whose chunk is gone. 0 when the store has no keyword index.
fn fts_dangling_rowids(conn: &Connection) -> Result<u64> {
    let indexed: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'chunks_fts_docsize'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if indexed.is_none() {
        return Ok(0);
    }
    let dangling: i64 = conn.query_row(
        "SELECT COUNT(*) FROM chunks_fts_docsize d
         WHERE NOT EXISTS (SELECT 1 FROM chunks c WHERE c.rowid = d.id)",
        [],
        |row| row.get(0),
    )?;
    Ok(dangling.max(0) as u64)
}

fn store_verdict(conn: &Connection, verdict: &IntegrityVerdict) -> Result<()> {
    queries::set_meta(
        conn,
        "integrity_ok",
        if verdict.healthy() { "1" } else { "0" },
    )?;
    queries::set_meta(
        conn,
        META_FTS_DANGLING,
        &verdict.fts_dangling_rowids.to_string(),
    )?;
    queries::set_meta(
        conn,
        "integrity_checked_at",
        &chrono::Utc::now().to_rfc3339(),
    )?;
    Ok(())
}

impl Storage {
    /// Run both checks now (seconds on a multi-GB store: never on a hook path).
    pub fn integrity_verdict(&self) -> Result<IntegrityVerdict> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        let result: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        Ok(IntegrityVerdict {
            sqlite_ok: result == "ok",
            fts_dangling_rowids: fts_dangling_rowids(&conn)?,
        })
    }

    /// Run the integrity checks. Returns true if the database is healthy.
    pub fn integrity_check(&self) -> Result<bool> {
        Ok(self.integrity_verdict()?.healthy())
    }

    /// Read the cached verdict without running a scan, including on a cache miss.
    pub fn cached_integrity(&self) -> Result<Option<bool>> {
        Ok(
            match (
                self.get_meta("integrity_ok")?,
                self.get_meta("integrity_checked_at")?,
            ) {
                (Some(ok), Some(at)) if chrono::DateTime::parse_from_rfc3339(&at).is_ok() => {
                    match ok.as_str() {
                        "1" => Some(true),
                        "0" => Some(false),
                        _ => None,
                    }
                }
                _ => None,
            },
        )
    }

    /// The dangling `chunks_fts` rowids the cached verdict counted; `None`
    /// before a verdict that counted them.
    pub fn cached_fts_dangling_rowids(&self) -> Result<Option<u64>> {
        Ok(self
            .get_meta(META_FTS_DANGLING)?
            .and_then(|count| count.parse().ok()))
    }

    /// Publish a status --deep verdict without migrations, only if maintenance
    /// permits writes. The guard covers opening, the repair, the transaction
    /// and closing. Dangling keyword rowids are removed first, by a full index
    /// rebuild if the targeted pass cannot remove them all: this is the
    /// foreground command the unhealthy reason names.
    pub(crate) fn store_integrity_verdict(
        path: &Path,
        verdict: &mut IntegrityVerdict,
    ) -> Result<()> {
        let data_dir = path.parent().unwrap_or(Path::new("."));
        let Some(_operation) = crate::maintenance::try_shared(data_dir)? else {
            return Ok(());
        };
        let mut conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        conn.busy_timeout(DEFAULT_BUSY_TIMEOUT)?;
        repair_if_dangling(&conn, verdict);
        let tx = conn.transaction()?;
        store_verdict(&tx, verdict)?;
        tx.commit()?;
        Ok(())
    }

    /// What `status` reports: the healthy verdict and the dangling keyword
    /// rowids. `deep` measures now and publishes the verdict; otherwise the
    /// cached verdict is read and nothing scans, even on a cache miss.
    pub(crate) fn status_integrity(
        &self,
        db_path: &Path,
        deep: bool,
    ) -> (Option<bool>, Option<u64>) {
        if !deep {
            let dangling = self.cached_fts_dangling_rowids().unwrap_or(None);
            return (self.cached_integrity().unwrap_or(None), dangling);
        }
        let Ok(mut verdict) = self.integrity_verdict() else {
            return (None, None);
        };
        if let Err(error) = Self::store_integrity_verdict(db_path, &mut verdict) {
            tracing::debug!(%error, "could not cache deep integrity verdict");
        }
        (Some(verdict.healthy()), Some(verdict.fts_dangling_rowids))
    }

    /// Integrity check with an app-level cache. SQLite recomputes
    /// `PRAGMA integrity_check` from scratch on every call — ~10s of CPU on a
    /// multi-GB DB (it walks every btree, including FTS5) — and the keyword
    /// index check reads every indexed rowid, so the result is persisted in
    /// `meta` and reused.
    ///
    /// - Cache younger than `ttl_hours`: cached verdict, no recompute.
    /// - Cache stale: recomputes only when `refresh_if_stale` is true (daemon
    ///   ticks and `status --deep`); other callers keep serving the stale
    ///   verdict so a statusline poll never eats the 10s hit.
    /// - No cache yet: computes once and stores (fresh installs are small/fast).
    ///
    /// It counts dangling keyword rowids and never removes them: the repair
    /// reads the whole keyword index, too long to hold the daemon's
    /// connection. `status --deep` removes them.
    pub fn integrity_check_cached(&self, ttl_hours: i64, refresh_if_stale: bool) -> Result<bool> {
        let cached = self.get_meta("integrity_ok")?;
        let checked_at = self
            .get_meta("integrity_checked_at")?
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|t| t.with_timezone(&chrono::Utc));

        if let (Some(ok), Some(ts)) = (&cached, checked_at) {
            let age = chrono::Utc::now().signed_duration_since(ts);
            let fresh = age < chrono::Duration::hours(ttl_hours) && age.num_seconds() >= 0;
            if fresh || !refresh_if_stale {
                return Ok(ok == "1");
            }
        }

        let verdict = self.integrity_verdict()?;
        if !self.with_connection(|conn| Ok(conn.is_readonly("main")?))? {
            self.with_connection(|conn| store_verdict(conn, &verdict))?;
        }
        Ok(verdict.healthy())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(id: &str, content: &str) -> crate::import::ConversationChunk {
        crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: "conv-fts".into(),
            project_name: "project-fts".into(),
            timestamp: "2026-08-12T00:00:00Z".into(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::ToolResult,
            seq: 0,
            is_sidechain: false,
        }
    }

    /// A chunk deleted while the delete trigger was absent leaves its rowid in
    /// the keyword index; `PRAGMA integrity_check` alone still said `ok`. The
    /// verdict counts it. The daemon's refresh only counts it; `status
    /// --deep`, the command the unhealthy reason names, removes it.
    #[test]
    fn a_keyword_rowid_whose_chunk_is_gone_is_counted_by_the_refresh_and_removed_by_status_deep() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("csr-engine.db");
        let storage = Storage::open(&db).unwrap();
        storage
            .insert_chunk(&chunk("kept", "kept words"), &[0.0; 384])
            .unwrap();
        storage
            .insert_chunk(&chunk("gone", "gone words"), &[0.0; 384])
            .unwrap();
        assert!(storage.integrity_check_cached(24, true).unwrap());
        storage
            .with_connection(|conn| {
                // What a delete the trigger never saw leaves behind: the
                // index still holds a rowid no chunk has.
                conn.execute(
                    "INSERT INTO chunks_fts(rowid, content) VALUES (999999, 'gone words')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        let verdict = storage.integrity_verdict().unwrap();
        assert!(verdict.sqlite_ok, "{verdict:?}");
        assert_eq!(verdict.fts_dangling_rowids, 1);

        let stale = chrono::Utc::now() - chrono::Duration::hours(48);
        storage
            .set_meta("integrity_checked_at", &stale.to_rfc3339())
            .unwrap();
        assert!(!storage.integrity_check_cached(24, true).unwrap());
        assert_eq!(storage.cached_fts_dangling_rowids().unwrap(), Some(1));
        assert_eq!(storage.integrity_verdict().unwrap().fts_dangling_rowids, 1);

        let reader = Storage::open_read_only(&db).unwrap();
        assert_eq!(reader.status_integrity(&db, true), (Some(true), Some(0)));
        assert_eq!(storage.cached_integrity().unwrap(), Some(true));
        assert_eq!(storage.integrity_verdict().unwrap().fts_dangling_rowids, 0);
    }

    #[test]
    fn the_unhealthy_reason_names_the_cause_and_the_fixing_command() {
        assert_eq!(unhealthy_reason(Some(true), Some(0)), None);
        assert_eq!(unhealthy_reason(None, None), None);
        let dangling = unhealthy_reason(Some(false), Some(172)).unwrap();
        assert!(
            dangling.starts_with("172 keyword-index rows point at deleted chunks"),
            "{dangling}"
        );
        assert!(
            dangling.ends_with("fix: csr-engine status --deep"),
            "{dangling}"
        );
        let mut status = serde_json::json!({"healthy": false, "fts_dangling_rowids": 172});
        add_health_reason(&mut status);
        assert_eq!(status["health_reason"], dangling.as_str());
        let mut status = serde_json::json!({"healthy": true, "fts_dangling_rowids": 0});
        add_health_reason(&mut status);
        assert!(status["health_reason"].is_null());
        let sqlite = unhealthy_reason(Some(false), Some(0)).unwrap();
        assert!(
            sqlite.starts_with("SQLite integrity_check failed"),
            "{sqlite}"
        );
        assert_eq!(unhealthy_reason(Some(false), None).unwrap(), sqlite);
    }

    #[test]
    fn integrity_cached_no_cache_computes_and_stores() {
        let storage = Storage::open_memory().unwrap();
        assert!(storage.integrity_check_cached(24, false).unwrap());
        assert_eq!(
            storage.get_meta("integrity_ok").unwrap().as_deref(),
            Some("1")
        );
        assert!(storage.get_meta("integrity_checked_at").unwrap().is_some());
        assert_eq!(storage.cached_fts_dangling_rowids().unwrap(), Some(0));
    }

    #[test]
    fn integrity_cached_fresh_serves_cache_without_recompute() {
        let storage = Storage::open_memory().unwrap();
        // Plant a fresh-but-wrong verdict: if the cache is honored, we get the
        // planted value back instead of the real (healthy) recompute.
        storage.set_meta("integrity_ok", "0").unwrap();
        storage
            .set_meta("integrity_checked_at", &chrono::Utc::now().to_rfc3339())
            .unwrap();
        assert!(!storage.integrity_check_cached(24, true).unwrap());
        assert!(!storage.integrity_check_cached(24, false).unwrap());
    }

    #[test]
    fn integrity_cached_stale_behavior_depends_on_refresh_flag() {
        let storage = Storage::open_memory().unwrap();
        let stale = chrono::Utc::now() - chrono::Duration::hours(48);
        storage.set_meta("integrity_ok", "0").unwrap();
        storage
            .set_meta("integrity_checked_at", &stale.to_rfc3339())
            .unwrap();
        // Stale + no refresh: keep serving the stale verdict (statusline path).
        assert!(!storage.integrity_check_cached(24, false).unwrap());
        // Stale + refresh: recompute (daemon path) and update the cache.
        assert!(storage.integrity_check_cached(24, true).unwrap());
        assert_eq!(
            storage.get_meta("integrity_ok").unwrap().as_deref(),
            Some("1")
        );
    }
}
