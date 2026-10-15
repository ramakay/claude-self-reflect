//! Chunk rows for prompts typed while Claude was busy.
//!
//! The transcript holds such a prompt only as a `queued_command` attachment, so
//! the conversation's own chunks never contain it. Each human one becomes chunk
//! rows of its own (one per ~900 characters), written under ids derived from the
//! conversation and the transcript line, never into the conversation's chunk
//! sequence: no stored row is rewritten, no chunk index shifts, and a reparse of
//! the transcript cannot move a chunk boundary.
//!
//! `queued_prompt_chunks` keys every such chunk by its transcript line, in the
//! same transaction that writes it. That is what makes a prompt indexed once
//! however many passes (import, watcher, backfill, reruns) see it, and what tells
//! these rows apart from a conversation's own: legacy placement ignores them.
//! `queued_prompt_scan` records that a conversation's transcript was read.

use std::collections::HashSet;

use anyhow::Result;
use rusqlite::params;

use crate::import::ConversationChunk;
use crate::provenance::{ChunkEvidence, ChunkProvenance};

/// The chunks of one queued prompt, ready to write.
pub struct QueuedPromptWrite {
    pub conversation_id: String,
    /// Byte offset of the prompt's line in the transcript.
    pub line_offset: u64,
    /// The storage `source` of the conversation's own rows.
    pub source: String,
    /// The conversation the rows belong to for provenance: the parent for a
    /// subagent transcript.
    pub source_conv_id: String,
    pub chunks: Vec<(ConversationChunk, Vec<f32>, ChunkEvidence)>,
}

/// What `status` says about queued prompts.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct QueuedPromptCounts {
    /// Prompts with chunk rows.
    pub indexed: i64,
    /// Conversations whose transcript the pass has read.
    pub scanned: i64,
    /// Imported conversations it has not read yet.
    pub pending: i64,
}

impl super::Storage {
    /// Transcript offsets of the prompts already indexed for a conversation.
    pub fn queued_prompt_offsets(&self, conversation_id: &str) -> Result<HashSet<u64>> {
        self.with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT line_offset FROM queued_prompt_chunks WHERE conversation_id = ?1",
            )?;
            let offsets = stmt
                .query_map(params![conversation_id], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(offsets.into_iter().map(|offset| offset as u64).collect())
        })
    }

    /// Write a queued prompt's chunks, embeddings, provenance and ledger rows in
    /// one transaction. `false`, and nothing written, when the prompt is already
    /// indexed. A chunk row that already exists is left as it is.
    pub fn insert_queued_prompt(&self, write: &QueuedPromptWrite) -> Result<bool> {
        self.with_immediate_transaction(|tx| {
            let indexed: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM queued_prompt_chunks
                               WHERE conversation_id = ?1 AND line_offset = ?2)",
                params![write.conversation_id, write.line_offset as i64],
                |row| row.get(0),
            )?;
            if indexed {
                return Ok(false);
            }
            let now = chrono::Utc::now().to_rfc3339();
            for (part, (chunk, embedding, evidence)) in write.chunks.iter().enumerate() {
                // seq stays NULL: the row is no part of the conversation's chunk
                // sequence, and must never read as its newest chunk.
                tx.execute(
                    "INSERT INTO chunks (id, conversation_id, project_name, timestamp, content,
                                         message_count, summary, seq, is_sidechain, source)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9)
                     ON CONFLICT(id) DO NOTHING",
                    params![
                        chunk.id,
                        chunk.conversation_id,
                        chunk.project_name,
                        chunk.timestamp,
                        chunk.content,
                        chunk.message_count as i64,
                        chunk.summary,
                        chunk.is_sidechain as i64,
                        write.source,
                    ],
                )?;
                tx.execute(
                    "INSERT INTO chunk_embeddings (chunk_id, embedding) VALUES (?1, ?2)
                     ON CONFLICT(chunk_id) DO NOTHING",
                    params![chunk.id, super::queries::vec_to_bytes(embedding)],
                )?;
                super::queries::insert_chunk_provenance(
                    tx,
                    &chunk.id,
                    &ChunkProvenance {
                        author: chunk.author,
                        source_conv_id: write.source_conv_id.clone(),
                        supersedes: None,
                    },
                )?;
                super::queries::replace_chunk_evidence_in(tx, evidence)?;
                tx.execute(
                    "INSERT INTO queued_prompt_chunks
                        (chunk_id, conversation_id, line_offset, part, indexed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        chunk.id,
                        write.conversation_id,
                        write.line_offset as i64,
                        part as i64,
                        now
                    ],
                )?;
            }
            Ok(true)
        })
    }

    /// Note that a conversation's transcript was read: `found` prompts, `added`
    /// of them newly indexed by this pass. Writes nothing when a pass changes
    /// nothing, so a live session's watcher does not write on every turn.
    pub fn record_queued_scan(
        &self,
        conversation_id: &str,
        found: usize,
        added: usize,
    ) -> Result<()> {
        self.with_connection(|conn| {
            conn.execute(
                "INSERT INTO queued_prompt_scan (conversation_id, scanned_at, found, added)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(conversation_id) DO UPDATE SET
                    scanned_at = excluded.scanned_at,
                    found = excluded.found,
                    added = queued_prompt_scan.added + excluded.added
                 WHERE queued_prompt_scan.found != excluded.found OR excluded.added > 0",
                params![
                    conversation_id,
                    chrono::Utc::now().to_rfc3339(),
                    found as i64,
                    added as i64
                ],
            )?;
            Ok(())
        })
    }

    /// Forget that a conversation's transcript was read, so the backfill reads it
    /// again. A pass that has prompts to index calls this first and writes the
    /// record back (`record_queued_scan`) only once every prompt is indexed: a
    /// failure midway then leaves the conversation pending, not marked done.
    pub fn clear_queued_scan(&self, conversation_id: &str) -> Result<()> {
        self.with_connection(|conn| {
            conn.execute(
                "DELETE FROM queued_prompt_scan WHERE conversation_id = ?1",
                params![conversation_id],
            )?;
            Ok(())
        })
    }

    /// Imported transcripts the pass has not read, by file path after `after`.
    pub fn pending_queued_scan_batch(
        &self,
        after_file_path: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        self.with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT i.file_path, i.conversation_id FROM import_state i
                 WHERE i.file_path > ?1 AND i.conversation_id IS NOT NULL
                   AND NOT EXISTS (SELECT 1 FROM queued_prompt_scan s
                                   WHERE s.conversation_id = i.conversation_id)
                 ORDER BY i.file_path LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(params![after_file_path, limit as i64], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn queued_prompt_counts(&self) -> Result<QueuedPromptCounts> {
        self.with_connection(|conn| {
            let indexed = conn.query_row(
                "SELECT COUNT(*) FROM queued_prompt_chunks WHERE part = 0",
                [],
                |row| row.get(0),
            )?;
            let scanned = conn.query_row("SELECT COUNT(*) FROM queued_prompt_scan", [], |row| {
                row.get(0)
            })?;
            let pending = conn.query_row(
                "SELECT COUNT(*) FROM import_state i
                 WHERE i.conversation_id IS NOT NULL
                   AND NOT EXISTS (SELECT 1 FROM queued_prompt_scan s
                                   WHERE s.conversation_id = i.conversation_id)",
                [],
                |row| row.get(0),
            )?;
            Ok(QueuedPromptCounts {
                indexed,
                scanned,
                pending,
            })
        })
    }
}
