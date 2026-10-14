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
    /// The stored chunk indexes are exactly `0..n`: the rows are the whole
    /// opening of the conversation, with none missing.
    pub from_the_start: bool,
}

/// What the stored rows say about a conversation, read in one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyState {
    /// A frontier is recorded: `(after_seq, first_chunk)`.
    Placed(usize, usize),
    /// Chunk rows predate the scrubber and nothing has placed them yet.
    Legacy(LegacyRows),
    /// No chunk row predates the scrubber.
    Fresh,
}

/// How a frontier commit ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontierCommit {
    /// The frontier now stored: `(after_seq, first_chunk)`. When another import
    /// recorded one first this is theirs, not the one offered.
    Stored(usize, usize),
    /// The conversation is no longer legacy (a scrub-aware import stamped it).
    NoLongerLegacy,
    /// The stored rows are not the ones the candidate was computed from.
    RowsChanged,
}

impl super::Storage {
    /// The recorded frontier of a legacy conversation: `(after_seq, first_chunk)`.
    pub fn legacy_frontier(&self, conversation_id: &str) -> Result<Option<(usize, usize)>> {
        self.with_connection(|conn| frontier(conn, conversation_id))
    }

    /// What the stored rows say about `conversation_id`, in one read snapshot:
    /// a recorded frontier, chunk rows no scrubber has touched (chunk rows, no
    /// frontier row, and an import state (`file_path`) with no scrub stamp), or
    /// neither. A recorded frontier is reported as such, never as "fresh".
    pub fn legacy_state(
        &self,
        file_path: &std::path::Path,
        conversation_id: &str,
    ) -> Result<LegacyState> {
        self.with_connection(|conn| {
            let tx = conn.unchecked_transaction()?;
            legacy_state(&tx, file_path, conversation_id)
        })
    }

    /// The stored chunks of `conversation_id`, oldest first: id and text.
    pub fn legacy_chunk_texts(&self, conversation_id: &str) -> Result<Vec<(String, String)>> {
        self.with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, content FROM chunks WHERE conversation_id = ?1
                 ORDER BY seq, rowid",
            )?;
            let rows = stmt
                .query_map(params![conversation_id], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Record the frontier of a legacy conversation, deciding in one short
    /// write transaction. `expected` is the state the candidate was computed
    /// from, outside any transaction: the file parse is slow and another import
    /// may have got there first. Inside the transaction a frontier already
    /// stored stands (the first record wins) and is what comes back; a
    /// conversation that is no longer legacy, or whose rows are not `expected`,
    /// gets nothing written. A conversation that was waiting to be placed waits
    /// no more.
    pub fn commit_legacy_frontier(
        &self,
        file_path: &std::path::Path,
        conversation_id: &str,
        expected: &LegacyRows,
        after_seq: usize,
    ) -> Result<FrontierCommit> {
        let mut conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let first_chunk = match legacy_state(&tx, file_path, conversation_id)? {
            LegacyState::Placed(after_seq, first_chunk) => {
                return Ok(FrontierCommit::Stored(after_seq, first_chunk));
            }
            LegacyState::Fresh => return Ok(FrontierCommit::NoLongerLegacy),
            LegacyState::Legacy(rows) if &rows != expected => {
                return Ok(FrontierCommit::RowsChanged);
            }
            LegacyState::Legacy(rows) => rows.first_chunk,
        };
        tx.execute(
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
        tx.execute(
            "DELETE FROM legacy_import_unplaced WHERE conversation_id = ?1",
            params![conversation_id],
        )?;
        tx.commit()?;
        Ok(FrontierCommit::Stored(after_seq, first_chunk))
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

fn legacy_state(
    conn: &Connection,
    file_path: &std::path::Path,
    conversation_id: &str,
) -> Result<LegacyState> {
    if let Some((after_seq, first_chunk)) = frontier(conn, conversation_id)? {
        return Ok(LegacyState::Placed(after_seq, first_chunk));
    }
    Ok(match legacy_rows(conn, file_path, conversation_id)? {
        Some(rows) => LegacyState::Legacy(rows),
        None => LegacyState::Fresh,
    })
}

fn legacy_rows(
    conn: &Connection,
    file_path: &std::path::Path,
    conversation_id: &str,
) -> Result<Option<LegacyRows>> {
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
    let (rows, min_seq, max_seq, with_receipt): (i64, Option<i64>, Option<i64>, i64) = conn
        .query_row(
            "SELECT COUNT(*), MIN(seq), MAX(seq),
                SUM(EXISTS (SELECT 1 FROM chunk_spans s
                            JOIN provenance_events e ON e.event_id = s.event_id
                            WHERE s.chunk_id = chunks.id AND e.receipt_kind = 'jsonl'))
         FROM chunks WHERE conversation_id = ?1",
            params![conversation_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get::<_, Option<i64>>(3)?.unwrap_or(0),
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
        from_the_start: min_seq == Some(0) && max_seq == Some(rows - 1),
    }))
}
