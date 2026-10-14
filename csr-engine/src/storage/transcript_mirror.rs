//! State rows for the transcript mirror (`crate::mirror`). One row per mirrored
//! transcript; the compressed bytes live on disk, this table says how much of
//! the source they cover so a crash or a second process can pick up exactly.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

/// Meta keys behind the `mirror` status block.
pub(crate) const MIRROR_ERRORS_KEY: &str = "mirror_errors";
pub(crate) const MIRROR_LAST_SYNC_KEY: &str = "mirror_last_sync";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorRow {
    /// Transcript path relative to the projects dir, without the `.zst` suffix.
    pub rel_path: String,
    /// Source bytes already mirrored: always ends on a line boundary.
    pub source_bytes: u64,
    /// blake3 (hex) of the last up-to-4096 source bytes already mirrored.
    pub tail_hash: String,
    /// Length of the `.zst` file the state describes.
    pub zst_bytes: u64,
    pub frames: u32,
    pub sealed: bool,
    pub updated_at: String,
    pub source_gone_at: Option<String>,
}

/// Sums over the state table: what `csr-engine status` shows without touching
/// the mirror directory.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MirrorTotals {
    pub files: i64,
    pub source_bytes: i64,
    pub zst_bytes: i64,
    pub sealed: i64,
    pub sources_gone: i64,
}

const ROW_COLUMNS: &str =
    "rel_path, source_bytes, tail_hash, zst_bytes, frames, sealed, updated_at, source_gone_at";

fn row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<MirrorRow> {
    Ok(MirrorRow {
        rel_path: row.get(0)?,
        source_bytes: row.get::<_, i64>(1)? as u64,
        tail_hash: row.get(2)?,
        zst_bytes: row.get::<_, i64>(3)? as u64,
        frames: row.get::<_, i64>(4)? as u32,
        sealed: row.get::<_, i64>(5)? != 0,
        updated_at: row.get(6)?,
        source_gone_at: row.get(7)?,
    })
}

fn upsert(conn: &Connection, row: &MirrorRow) -> Result<()> {
    conn.execute(
        "INSERT INTO transcript_mirror
            (rel_path, source_bytes, tail_hash, zst_bytes, frames, sealed, updated_at, source_gone_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(rel_path) DO UPDATE SET
            source_bytes = excluded.source_bytes,
            tail_hash = excluded.tail_hash,
            zst_bytes = excluded.zst_bytes,
            frames = excluded.frames,
            sealed = excluded.sealed,
            updated_at = excluded.updated_at,
            source_gone_at = excluded.source_gone_at",
        params![
            row.rel_path,
            row.source_bytes as i64,
            row.tail_hash,
            row.zst_bytes as i64,
            row.frames as i64,
            row.sealed as i64,
            row.updated_at,
            row.source_gone_at,
        ],
    )?;
    Ok(())
}

impl super::Storage {
    pub fn mirror_row(&self, rel_path: &str) -> Result<Option<MirrorRow>> {
        self.with_connection(|conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {ROW_COLUMNS} FROM transcript_mirror WHERE rel_path = ?1"),
                    [rel_path],
                    row_from,
                )
                .optional()?)
        })
    }

    /// Every state row, keyed by `rel_path`: the daemon pass loads this once.
    pub fn mirror_rows(&self) -> Result<HashMap<String, MirrorRow>> {
        self.with_connection(|conn| {
            let mut stmt = conn.prepare(&format!("SELECT {ROW_COLUMNS} FROM transcript_mirror"))?;
            let rows = stmt
                .query_map([], row_from)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows
                .into_iter()
                .map(|row| (row.rel_path.clone(), row))
                .collect())
        })
    }

    pub fn mirror_upsert(&self, row: &MirrorRow) -> Result<()> {
        self.with_connection(|conn| upsert(conn, row))
    }

    pub fn mirror_totals(&self) -> Result<MirrorTotals> {
        self.with_connection(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*), COALESCE(SUM(source_bytes), 0), COALESCE(SUM(zst_bytes), 0),
                        COALESCE(SUM(sealed), 0), COALESCE(SUM(source_gone_at IS NOT NULL), 0)
                 FROM transcript_mirror",
                [],
                |row| {
                    Ok(MirrorTotals {
                        files: row.get(0)?,
                        source_bytes: row.get(1)?,
                        zst_bytes: row.get(2)?,
                        sealed: row.get(3)?,
                        sources_gone: row.get(4)?,
                    })
                },
            )?)
        })
    }

    /// Drop every state row (`mirror purge --yes`). Returns how many went.
    pub fn mirror_clear(&self) -> Result<usize> {
        self.with_connection(|conn| Ok(conn.execute("DELETE FROM transcript_mirror", [])?))
    }

    pub fn mirror_record_sync(&self, when: &str) -> Result<()> {
        self.set_meta(MIRROR_LAST_SYNC_KEY, when)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;

    fn row(rel: &str, gone: Option<&str>) -> MirrorRow {
        MirrorRow {
            rel_path: rel.into(),
            source_bytes: 100,
            tail_hash: "h".into(),
            zst_bytes: 40,
            frames: 2,
            sealed: false,
            updated_at: "2026-10-14T00:00:00Z".into(),
            source_gone_at: gone.map(str::to_string),
        }
    }

    #[test]
    fn rows_round_trip_and_totals_sum_the_table() {
        let storage = Storage::open_memory().unwrap();
        assert_eq!(storage.mirror_totals().unwrap(), MirrorTotals::default());
        storage.mirror_upsert(&row("p/a.jsonl", None)).unwrap();
        let mut b = row("p/b.jsonl", Some("2026-10-14T01:00:00Z"));
        b.sealed = true;
        storage.mirror_upsert(&b).unwrap();
        assert_eq!(storage.mirror_row("p/b.jsonl").unwrap(), Some(b.clone()));
        assert_eq!(storage.mirror_row("p/none.jsonl").unwrap(), None);
        // The upsert replaces in place.
        b.source_bytes = 250;
        storage.mirror_upsert(&b).unwrap();
        assert_eq!(storage.mirror_rows().unwrap().len(), 2);
        let totals = storage.mirror_totals().unwrap();
        assert_eq!(
            totals,
            MirrorTotals {
                files: 2,
                source_bytes: 350,
                zst_bytes: 80,
                sealed: 1,
                sources_gone: 1,
            }
        );
        assert_eq!(storage.mirror_clear().unwrap(), 2);
        assert_eq!(storage.mirror_totals().unwrap(), MirrorTotals::default());
    }
}
