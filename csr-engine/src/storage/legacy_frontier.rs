//! Conversations whose chunk rows were stored before the secret scrubber.
//!
//! Those rows are never rewritten or deleted by import, whatever a reparse
//! would now produce for them. A legacy conversation gets one frontier row the
//! first time it is imported with the scrub on: the highest message sequence
//! number its stored chunks cover, and the first chunk index free after them.
//! From then on only the messages after the frontier are chunked, scrubbed,
//! under ids that continue the sequence. A conversation first imported by a
//! scrub-aware build has no frontier row and imports as it always did.
//!
//! A legacy conversation whose place cannot be found is left alone, new
//! messages and all, until it can be placed. `legacy_import_unplaced` keeps a
//! row per such conversation so `status` can say how many wait.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

/// What the stored rows say about a conversation that has chunk rows, no
/// frontier row and no scrub stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyRows {
    /// First chunk index no stored row, nor the import checkpoint, uses.
    pub first_chunk: usize,
    /// Highest message sequence the stored chunks' receipts cover. `None` when
    /// some stored chunk has no transcript receipt.
    pub receipt_seq: Option<usize>,
    /// The newest stored chunk: id and text.
    pub last_chunk: (String, String),
}

impl super::Storage {
    /// The recorded frontier of a legacy conversation: `(after_seq, first_chunk)`.
    pub fn legacy_frontier(&self, conversation_id: &str) -> Result<Option<(usize, usize)>> {
        self.with_connection(|conn| frontier(conn, conversation_id))
    }

    /// The stored rows of `conversation_id` when it is a conversation the
    /// scrubber has not touched: chunk rows exist, no frontier row, and its
    /// import state (`file_path`) carries no scrub stamp.
    pub fn legacy_rows(
        &self,
        file_path: &std::path::Path,
        conversation_id: &str,
    ) -> Result<Option<LegacyRows>> {
        self.with_connection(|conn| legacy_rows(conn, file_path, conversation_id))
    }

    /// Record the frontier of a legacy conversation. The first record stands.
    /// A conversation that was waiting to be placed waits no more.
    pub fn record_legacy_frontier(
        &self,
        conversation_id: &str,
        after_seq: usize,
        first_chunk: usize,
    ) -> Result<()> {
        self.with_connection(|conn| {
            conn.execute(
                "INSERT OR IGNORE INTO legacy_import_frontier
                 (conversation_id, after_seq, first_chunk, recorded_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    conversation_id,
                    after_seq as i64,
                    first_chunk as i64,
                    chrono::Utc::now().to_rfc3339()
                ],
            )?;
            conn.execute(
                "DELETE FROM legacy_import_unplaced WHERE conversation_id = ?1",
                params![conversation_id],
            )?;
            Ok(())
        })
    }

    /// Note that import tried to place a legacy conversation and could not.
    /// Touches no chunk row and no frontier; each retry only bumps the count.
    pub fn record_legacy_unplaced(&self, conversation_id: &str) -> Result<()> {
        self.with_connection(|conn| {
            let now = chrono::Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO legacy_import_unplaced
                 (conversation_id, first_tried_at, last_tried_at, tries)
                 VALUES (?1, ?2, ?2, 1)
                 ON CONFLICT(conversation_id) DO UPDATE
                 SET last_tried_at = excluded.last_tried_at, tries = tries + 1",
                params![conversation_id, now],
            )?;
            Ok(())
        })
    }

    /// `(placed, waiting)`: legacy conversations with a recorded frontier, and
    /// those import tried to place and could not. Two primary-key table counts.
    pub fn legacy_frontier_counts(&self) -> Result<(i64, i64)> {
        self.with_connection(|conn| {
            let placed =
                conn.query_row("SELECT COUNT(*) FROM legacy_import_frontier", [], |r| {
                    r.get(0)
                })?;
            let waiting = conn.query_row(
                "SELECT COUNT(*) FROM legacy_import_unplaced u
                 WHERE NOT EXISTS (SELECT 1 FROM legacy_import_frontier f
                                   WHERE f.conversation_id = u.conversation_id)",
                [],
                |r| r.get(0),
            )?;
            Ok((placed, waiting))
        })
    }
}

fn frontier(conn: &Connection, conversation_id: &str) -> Result<Option<(usize, usize)>> {
    Ok(conn
        .query_row(
            "SELECT after_seq, first_chunk FROM legacy_import_frontier WHERE conversation_id = ?1",
            params![conversation_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)? as usize,
                    row.get::<_, i64>(1)? as usize,
                ))
            },
        )
        .optional()?)
}

fn legacy_rows(
    conn: &Connection,
    file_path: &std::path::Path,
    conversation_id: &str,
) -> Result<Option<LegacyRows>> {
    if frontier(conn, conversation_id)?.is_some() {
        return Ok(None);
    }
    let path = file_path.to_string_lossy();
    let (stamped, checkpoint): (bool, i64) = conn
        .query_row(
            "SELECT secrets_redacted IS NOT NULL, chunks_imported
             FROM import_state WHERE file_path = ?1",
            params![path.as_ref()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .unwrap_or((false, 0));
    if stamped {
        return Ok(None);
    }
    let (rows, max_seq, with_receipt): (i64, Option<i64>, i64) = conn.query_row(
        "SELECT COUNT(*), MAX(seq),
                SUM(EXISTS (SELECT 1 FROM chunk_spans s
                            JOIN provenance_events e ON e.event_id = s.event_id
                            WHERE s.chunk_id = chunks.id AND e.receipt_kind = 'jsonl'))
         FROM chunks WHERE conversation_id = ?1",
        params![conversation_id],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get::<_, Option<i64>>(2)?.unwrap_or(0),
            ))
        },
    )?;
    if rows == 0 {
        return Ok(None);
    }
    let receipt_seq = if with_receipt == rows {
        conn.query_row(
            "SELECT MAX(e.seq) FROM chunks c
             JOIN chunk_spans s ON s.chunk_id = c.id
             JOIN provenance_events e ON e.event_id = s.event_id
             WHERE c.conversation_id = ?1 AND e.receipt_kind = 'jsonl'",
            params![conversation_id],
            |row| row.get::<_, Option<i64>>(0),
        )?
        .map(|seq| seq as usize)
    } else {
        None
    };
    let last_chunk = conn.query_row(
        "SELECT id, content FROM chunks WHERE conversation_id = ?1
         ORDER BY seq DESC, rowid DESC LIMIT 1",
        params![conversation_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let first_chunk = (max_seq.unwrap_or(-1) + 1).max(rows).max(checkpoint) as usize;
    Ok(Some(LegacyRows {
        first_chunk,
        receipt_seq,
        last_chunk,
    }))
}
