//! Episode tables: upsert, watermark, compact records. Additive; never touches
//! `chunks`, vectors, the HNSW or the USearch generation.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use super::parse::Episode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watermark {
    pub conversation_id: String,
    /// Decoded bytes consumed (complete lines only).
    pub source_bytes: u64,
    /// blake3 of the last 4096 consumed bytes.
    pub tail_hash: String,
    /// Offset where the newest prompt group starts: an incremental parse
    /// restarts there so an open group is rebuilt whole.
    pub resume_offset: u64,
    /// First ordinal of that group; -1 marks a skipped file (CSR agent run).
    pub resume_ordinal: i64,
}

pub fn has_episodes(conn: &Connection) -> bool {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM episodes)", [], |r| {
        r.get::<_, i64>(0)
    })
    .map(|n| n != 0)
    .unwrap_or(false)
}

/// The conversation an episode id belongs to.
pub fn episode_session(conn: &Connection, id: &str) -> Option<String> {
    conn.query_row(
        "SELECT conversation_id FROM episodes WHERE id = ?1",
        [id],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

pub fn episode_id(conversation_id: &str, ordinal: i64) -> String {
    format!("ep:{conversation_id}:{ordinal}")
}

pub fn get_watermark(conn: &Connection, rel: &str) -> Result<Option<Watermark>> {
    Ok(conn
        .query_row(
            "SELECT conversation_id, source_bytes, tail_hash, resume_offset, resume_ordinal
             FROM episode_watermarks WHERE rel_path = ?1",
            [rel],
            |r| {
                Ok(Watermark {
                    conversation_id: r.get(0)?,
                    source_bytes: r.get::<_, i64>(1)? as u64,
                    tail_hash: r.get(2)?,
                    resume_offset: r.get::<_, i64>(3)? as u64,
                    resume_ordinal: r.get(4)?,
                })
            },
        )
        .optional()?)
}

pub fn set_watermark(conn: &Connection, rel: &str, w: &Watermark, now: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO episode_watermarks
            (rel_path, conversation_id, source_bytes, tail_hash, updated_at, resume_offset, resume_ordinal)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(rel_path) DO UPDATE SET
            conversation_id = excluded.conversation_id,
            source_bytes = excluded.source_bytes,
            tail_hash = excluded.tail_hash,
            updated_at = excluded.updated_at,
            resume_offset = excluded.resume_offset,
            resume_ordinal = excluded.resume_ordinal",
        params![
            rel,
            w.conversation_id,
            w.source_bytes as i64,
            w.tail_hash,
            now,
            w.resume_offset as i64,
            w.resume_ordinal
        ],
    )?;
    Ok(())
}

/// Whether another file already owns this conversation id.
pub fn conversation_owner(conn: &Connection, conversation_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT rel_path FROM episode_watermarks WHERE conversation_id = ?1 LIMIT 1",
            [conversation_id],
            |r| r.get(0),
        )
        .optional()?)
}

pub fn delete_conversation(conn: &Connection, conversation_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM episodes WHERE conversation_id = ?1",
        [conversation_id],
    )?;
    conn.execute(
        "DELETE FROM session_cards WHERE conversation_id = ?1",
        [conversation_id],
    )?;
    Ok(())
}

pub fn delete_from_ordinal(conn: &Connection, conversation_id: &str, ordinal: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM episodes WHERE conversation_id = ?1 AND ordinal >= ?2",
        params![conversation_id, ordinal],
    )?;
    Ok(())
}

pub fn upsert_episode(
    conn: &Connection,
    conversation_id: &str,
    project: &str,
    source: &str,
    e: &Episode,
) -> Result<()> {
    let id = episode_id(conversation_id, e.ordinal);
    // Delete then insert so the external-content index sees both sides.
    conn.execute("DELETE FROM episodes WHERE id = ?1", [&id])?;
    conn.execute(
        "INSERT INTO episodes
            (id, conversation_id, project_name, ordinal, first_uuid, last_uuid,
             timestamp, text, token_est, echo_class, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            id,
            conversation_id,
            project,
            e.ordinal,
            e.first_uuid,
            e.last_uuid,
            e.timestamp,
            e.text,
            e.token_est,
            e.echo_class,
            source
        ],
    )?;
    Ok(())
}

/// Idempotent: a re-parse never duplicates a record.
pub fn insert_compact(
    conn: &Connection,
    uuid: &str,
    conversation_id: &str,
    kind: &str,
    timestamp: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO episode_compact_records (uuid, conversation_id, kind, timestamp)
         VALUES (?1, ?2, ?3, ?4)",
        params![uuid, conversation_id, kind, timestamp],
    )?;
    Ok(())
}

pub fn count(conn: &Connection, table: &str) -> Result<i64> {
    Ok(conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?)
}
