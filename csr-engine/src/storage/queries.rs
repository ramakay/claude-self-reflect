use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use chrono;
use rusqlite::{params, Connection, OptionalExtension};

use crate::import::{ConversationChunk, CsrSuppressionStats};
use crate::provenance::{ChunkEvidence, ChunkProvenance, ProvenanceEvent, Speaker, TrustTier};

/// Upsert provenance for a chunk (who authored it, source conv, supersession).
pub fn insert_chunk_provenance(
    conn: &Connection,
    chunk_id: &str,
    prov: &ChunkProvenance,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO chunk_provenance (chunk_id, author, source_conv_id, supersedes)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            chunk_id,
            prov.author.as_str(),
            prov.source_conv_id,
            prov.supersedes,
        ],
    )?;
    Ok(())
}

pub fn insert_provenance_event(conn: &Connection, event: &ProvenanceEvent) -> Result<()> {
    conn.execute(
        "INSERT INTO provenance_events
            (event_id, conversation_id, message_key, seq, channel, trust_tier,
             parent_event_id, receipt_kind, receipt_ref, observed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(event_id) DO UPDATE SET
            trust_tier = MIN(provenance_events.trust_tier, excluded.trust_tier),
            parent_event_id = COALESCE(provenance_events.parent_event_id,
                                       excluded.parent_event_id),
            receipt_ref = COALESCE(provenance_events.receipt_ref,
                                   excluded.receipt_ref),
            receipt_kind = CASE WHEN provenance_events.receipt_kind = 'unreconstructible'
                                THEN excluded.receipt_kind ELSE provenance_events.receipt_kind END,
            observed_at = CASE WHEN excluded.receipt_kind IN ('source_missing', 'source_unparsed', 'source_unmatched', 'plan_file')
                               THEN excluded.observed_at ELSE provenance_events.observed_at END",
        params![
            event.event_id,
            event.conversation_id,
            event.message_key,
            event.seq as i64,
            event.channel,
            event.trust_tier.as_i64(),
            event.parent_event_id,
            event.receipt_kind,
            event.receipt_ref,
            event.observed_at,
        ],
    )?;
    Ok(())
}

pub(crate) fn parent_provenance_context(
    conn: &Connection,
    conversation_id: &str,
    message_key: Option<&str>,
) -> Result<crate::import::ParentContext> {
    if let Some(message_key) = message_key {
        let matched = conn
            .query_row(
                "SELECT event_id, trust_tier
                   FROM provenance_events
                  WHERE conversation_id = ?1 AND message_key = ?2
                  ORDER BY trust_tier, CASE WHEN channel IN ('assistant_message', 'codex_assistant')
                                THEN 0 ELSE 1 END,
                           event_id
                  LIMIT 1",
                params![conversation_id, message_key],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .optional()?;
        if let Some((event_id, tier)) = matched {
            return Ok(crate::import::ParentContext {
                floor: TrustTier::from_db(tier),
                event_id: Some(event_id),
            });
        }
    }
    let floor = conn.query_row(
        "SELECT MIN(trust_tier) FROM provenance_events WHERE conversation_id = ?1",
        [conversation_id],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    Ok(crate::import::ParentContext {
        floor: TrustTier::from_db(floor),
        event_id: None,
    })
}

/// Replace one chunk's structural evidence and its cached trust fields in one
/// transaction. Re-importing a growing final chunk cannot leave stale spans.
pub fn replace_chunk_evidence(conn: &Connection, evidence: &ChunkEvidence) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let relink = evidence_needs_relink(&tx, evidence)?;
    replace_chunk_evidence_inner(&tx, evidence)?;
    if relink {
        super::sidechain_provenance::relink_conversations(
            &tx,
            &evidence
                .events
                .iter()
                .map(|e| e.conversation_id.clone())
                .collect::<Vec<_>>(),
        )?;
    } else {
        super::artifact_provenance::lower_from(&tx, &[("chunk", &evidence.chunk_id)], &[])?;
    }
    tx.commit()?;
    Ok(())
}

fn evidence_needs_relink(conn: &Connection, evidence: &ChunkEvidence) -> Result<bool> {
    for event in &evidence.events {
        if event.parent_event_id.is_some() {
            return Ok(true);
        }
        let (session,message):(Option<i64>,Option<i64>)=conn.query_row("SELECT MIN(trust_tier),MIN(CASE WHEN message_key=?2 THEN trust_tier END) FROM provenance_events WHERE conversation_id=?1",params![event.conversation_id,event.message_key],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if session.is_none()
            || session.is_some_and(|t| t > event.trust_tier.as_i64())
            || message.is_some_and(|t| t > event.trust_tier.as_i64())
        {
            return Ok(true);
        }
    }
    Ok(conn
        .query_row(
            "SELECT is_sidechain FROM chunks WHERE id=?1",
            [&evidence.chunk_id],
            |r| r.get::<_, bool>(0),
        )
        .optional()?
        .unwrap_or(false))
}

pub fn replace_chunk_evidence_batch(conn: &Connection, evidence: &[ChunkEvidence]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for row in evidence {
        replace_chunk_evidence_inner(&tx, row)
            .with_context(|| format!("persisting provenance for chunk {}", row.chunk_id))?;
    }
    super::sidechain_provenance::relink(&tx)?;
    tx.commit()?;
    Ok(())
}

/// Backfill cannot race a forward importer into replacing real JSONL receipts.
/// Equality is checked inside the transaction; unchanged retries issue no writes.
pub fn replace_backfill_evidence_batch(
    conn: &Connection,
    evidence: &[ChunkEvidence],
) -> Result<usize> {
    if evidence.is_empty() {
        return Ok(0);
    }
    let tx = conn.unchecked_transaction()?;
    let mut written = 0;
    for row in evidence {
        let (count, real): (usize, bool) = tx.query_row(
            "SELECT COUNT(*), COALESCE(MAX(e.receipt_kind='jsonl'),0) FROM chunk_spans s JOIN provenance_events e USING(event_id) WHERE s.chunk_id=?1",
            [&row.chunk_id], |r| Ok((r.get::<_, i64>(0)? as usize,r.get(1)?)))?;
        if real {
            continue;
        }
        let mut same = count == row.spans.len();
        if same {
            for (span, event) in row.spans.iter().zip(&row.events) {
                same &= tx.query_row("SELECT EXISTS(SELECT 1 FROM chunk_spans s JOIN provenance_events e USING(event_id) WHERE s.chunk_id=?1 AND s.event_id=?2 AND s.start_char=?3 AND s.end_char=?4 AND s.content_hash=?5 AND e.receipt_kind=?6 AND e.receipt_ref IS ?7 AND e.observed_at=?8)",
                    params![row.chunk_id,span.event_id,span.start_char as i64,span.end_char as i64,span.content_hash,event.receipt_kind,event.receipt_ref,event.observed_at], |r|r.get::<_,bool>(0))?;
            }
        }
        if !same {
            replace_chunk_evidence_inner(&tx, row)?;
            written += 1;
        }
    }
    if written > 0 {
        super::sidechain_provenance::relink(&tx)?;
    }
    tx.commit()?;
    Ok(written)
}

fn replace_chunk_evidence_inner(conn: &Connection, evidence: &ChunkEvidence) -> Result<()> {
    for event in &evidence.events {
        insert_provenance_event(conn, event)?;
    }
    conn.execute(
        "DELETE FROM chunk_spans WHERE chunk_id = ?1",
        [&evidence.chunk_id],
    )?;
    for span in &evidence.spans {
        anyhow::ensure!(
            span.chunk_id == evidence.chunk_id,
            "span chunk id does not match evidence chunk id"
        );
        conn.execute(
            "INSERT INTO chunk_spans
                (chunk_id, event_id, start_char, end_char, content_hash)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                span.chunk_id,
                span.event_id,
                span.start_char as i64,
                span.end_char as i64,
                span.content_hash,
            ],
        )?;
    }

    let (tier, tool_chars, span_count, unknown_share, chunk_content): (
        Option<i64>,
        i64,
        i64,
        i64,
        String,
    ) = conn.query_row(
        "SELECT MIN(pe.trust_tier),
                    COALESCE(SUM(CASE
                        WHEN pe.channel LIKE 'tool_result:%'
                          OR pe.channel LIKE 'codex_tool:%'
                        THEN cs.end_char - cs.start_char ELSE 0 END), 0),
                    COUNT(cs.event_id),
                    COALESCE(SUM(CASE WHEN pe.receipt_kind IN ('unreconstructible', 'source_missing', 'source_unparsed', 'source_unmatched')
                                      THEN 1 ELSE 0 END), 0),
                    c.content
               FROM chunks c
               LEFT JOIN chunk_spans cs ON cs.chunk_id = c.id
               LEFT JOIN provenance_events pe ON pe.event_id = cs.event_id
              WHERE c.id = ?1
              GROUP BY c.id",
        [&evidence.chunk_id],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?;
    let chunk_chars = chunk_content.chars().count() as i64;
    let min_trust = TrustTier::from_db(tier);
    let tool_result_share = if span_count == 0 || chunk_chars == 0 || unknown_share > 0 {
        None
    } else {
        Some(tool_chars as f64 / chunk_chars as f64)
    };
    anyhow::ensure!(
        min_trust <= evidence.min_trust,
        "persisted chunk floor exceeds the structurally observed floor"
    );
    anyhow::ensure!(
        match (tool_result_share, evidence.tool_result_share) {
            (None, None) => true,
            (Some(actual), Some(expected)) => (actual - expected).abs() < f64::EPSILON,
            _ => false,
        },
        "provided tool-result share does not match persisted spans: actual={tool_result_share:?}, expected={:?}, spans={span_count}, chars={chunk_chars}, tool_chars={tool_chars}",
        evidence.tool_result_share,
    );
    conn.execute(
        "UPDATE chunks SET min_trust = ?2, tool_result_share = ?3 WHERE id = ?1",
        params![evidence.chunk_id, min_trust.as_i64(), tool_result_share],
    )?;
    Ok(())
}

pub fn get_chunk_min_trust(conn: &Connection, chunk_id: &str) -> Result<TrustTier> {
    let encoded = conn
        .query_row(
            "SELECT min_trust FROM chunks WHERE id = ?1",
            [chunk_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten();
    Ok(TrustTier::from_db(encoded))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceBackfillRow {
    pub id: String,
    pub conversation_id: String,
    pub project_name: String,
    pub timestamp: String,
    pub content: String,
    pub source: String,
    pub is_sidechain: bool,
    pub source_path: Option<String>,
    pub failure_observed_at: Option<String>,
}

pub fn list_chunks_missing_spans(
    conn: &Connection,
    after_id: Option<&str>,
    limit: usize,
) -> Result<Vec<ProvenanceBackfillRow>> {
    let mut statement = conn.prepare(
        "SELECT c.id, c.conversation_id, c.project_name, c.timestamp, c.content,
                c.source, c.is_sidechain,
                (SELECT i.file_path FROM import_state i
                  WHERE i.conversation_id = c.conversation_id
                    AND i.file_path NOT LIKE 'plan:%'
                  ORDER BY i.file_path LIMIT 1)
           FROM chunks c
          WHERE c.id > COALESCE(?1, '')
            AND NOT EXISTS (SELECT 1 FROM chunk_spans s WHERE s.chunk_id = c.id)
          ORDER BY c.id
          LIMIT ?2",
    )?;
    let rows = statement.query_map(params![after_id, limit as i64], |row| {
        Ok(ProvenanceBackfillRow {
            id: row.get(0)?,
            conversation_id: row.get(1)?,
            project_name: row.get(2)?,
            timestamp: row.get(3)?,
            content: row.get(4)?,
            source: row.get(5)?,
            is_sidechain: row.get::<_, i64>(6)? != 0,
            source_path: row.get(7)?,
            failure_observed_at: None,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn list_provenance_backfill_candidates(
    conn: &Connection,
    after: Option<(&str, &str)>,
    limit: usize,
    retry: bool,
) -> Result<Vec<ProvenanceBackfillRow>> {
    let mut statement = conn.prepare(
        "SELECT c.id,c.conversation_id,c.project_name,c.timestamp,c.content,c.source,c.is_sidechain,
           COALESCE((SELECT i.file_path FROM import_state i WHERE i.conversation_id=c.conversation_id AND i.file_path NOT LIKE 'plan:%' ORDER BY i.file_path LIMIT 1),
                    (SELECT e.receipt_ref FROM chunk_spans s JOIN provenance_events e USING(event_id) WHERE s.chunk_id=c.id LIMIT 1)),
           (SELECT MIN(e.observed_at) FROM chunk_spans s JOIN provenance_events e USING(event_id) WHERE s.chunk_id=c.id)
         FROM chunks c
         WHERE (c.conversation_id,c.id) > (?1,?2)
           AND NOT EXISTS (SELECT 1 FROM chunk_spans s JOIN provenance_events e USING(event_id) WHERE s.chunk_id=c.id AND (?4=0 OR e.receipt_kind='jsonl'))
         ORDER BY c.conversation_id,c.id LIMIT ?3")?;
    let (conv, id) = after.unwrap_or(("", ""));
    let rows = statement.query_map(params![conv, id, limit as i64, retry], |r| {
        Ok(ProvenanceBackfillRow {
            id: r.get(0)?,
            conversation_id: r.get(1)?,
            project_name: r.get(2)?,
            timestamp: r.get(3)?,
            content: r.get(4)?,
            source: r.get(5)?,
            is_sidechain: r.get(6)?,
            source_path: r.get(7)?,
            failure_observed_at: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn provenance_coverage(conn: &Connection) -> Result<(i64, i64, i64, Option<f64>)> {
    conn.query_row(
        "SELECT COALESCE(SUM(CASE WHEN EXISTS (
                    SELECT 1 FROM chunk_spans s WHERE s.chunk_id = c.id
                ) THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN c.min_trust = 0 THEN 1 ELSE 0 END), 0),
                COUNT(*), AVG(c.tool_result_share)
           FROM chunks c",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )
    .map_err(Into::into)
}

pub fn provenance_tier_histogram(conn: &Connection) -> Result<Vec<(TrustTier, i64)>> {
    let mut statement = conn
        .prepare("SELECT min_trust, COUNT(*) FROM chunks GROUP BY min_trust ORDER BY min_trust")?;
    let rows = statement.query_map([], |row| {
        Ok((
            TrustTier::from_db(row.get::<_, Option<i64>>(0)?),
            row.get(1)?,
        ))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn provenance_failure_counts(conn: &Connection) -> Result<[i64; 3]> {
    let mut counts = [0; 3];
    let mut statement = conn.prepare("SELECT e.receipt_kind, COUNT(DISTINCT s.chunk_id) FROM provenance_events e JOIN chunk_spans s USING(event_id) WHERE e.receipt_kind IN ('source_missing','source_unparsed','source_unmatched') GROUP BY e.receipt_kind")?;
    for row in statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (kind, count) = row?;
        match kind.as_str() {
            "source_missing" => counts[0] = count,
            "source_unparsed" => counts[1] = count,
            "source_unmatched" => counts[2] = count,
            _ => {}
        }
    }
    Ok(counts)
}

/// Upsert a derivation-ledger entry (Pillar 1). `times_reused` is preserved on
/// conflict so a re-import never resets reuse counts.
pub fn upsert_ledger_entry(conn: &Connection, e: &crate::ledger::LedgerEntry) -> Result<()> {
    conn.execute(
        "INSERT INTO derivation_ledger
             (id, content, anchor, cost_bucket, inferability, confidence, times_reused, repo, branch, user)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(id, repo, branch, user) DO UPDATE SET
             content=excluded.content, anchor=excluded.anchor,
             cost_bucket=excluded.cost_bucket, inferability=excluded.inferability,
             confidence=excluded.confidence",
        rusqlite::params![
            e.id,
            e.content,
            e.anchor,
            e.cost_bucket.as_str(),
            e.inferability,
            e.confidence,
            e.times_reused as i64,
            e.scope.repo,
            e.scope.branch,
            e.scope.user,
        ],
    )?;
    Ok(())
}

/// Fetch ledger entries for an exact {repo, branch, user} scope, newest first.
pub fn get_ledger_entries(
    conn: &Connection,
    scope: &crate::ledger::Scope,
    limit: i64,
) -> Result<Vec<crate::ledger::LedgerEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, content, anchor, cost_bucket, inferability, confidence, times_reused,
                repo, branch, user
         FROM derivation_ledger
         WHERE repo = ?1 AND branch = ?2 AND user = ?3
         ORDER BY created_at DESC, id DESC LIMIT ?4",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![scope.repo, scope.branch, scope.user, limit],
        |r| {
            Ok(crate::ledger::LedgerEntry {
                id: r.get(0)?,
                content: r.get(1)?,
                anchor: r.get(2)?,
                cost_bucket: crate::ledger::CostBucket::from_str_lossy(&r.get::<_, String>(3)?),
                inferability: r.get(4)?,
                confidence: r.get(5)?,
                times_reused: r.get::<_, i64>(6)? as u32,
                scope: crate::ledger::Scope {
                    repo: r.get(7)?,
                    branch: r.get(8)?,
                    user: r.get(9)?,
                },
            })
        },
    )?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Increment the reuse counter for a ledger entry (governor signal, Pillar 4).
pub fn increment_ledger_reuse(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "UPDATE derivation_ledger SET times_reused = times_reused + 1 WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

/// Fetch provenance for a chunk, if any. Unknown author tokens degrade to
/// `ToolResult` (non-authoritative) rather than failing the read.
pub fn get_chunk_provenance(conn: &Connection, chunk_id: &str) -> Result<Option<ChunkProvenance>> {
    let mut stmt = conn.prepare(
        "SELECT author, source_conv_id, supersedes FROM chunk_provenance WHERE chunk_id = ?1",
    )?;
    let mut rows = stmt.query(params![chunk_id])?;
    if let Some(row) = rows.next()? {
        let author_str: String = row.get(0)?;
        Ok(Some(ChunkProvenance {
            author: author_str.parse().unwrap_or(Speaker::ToolResult),
            source_conv_id: row.get(1)?,
            supersedes: row.get(2)?,
        }))
    } else {
        Ok(None)
    }
}

/// The session that ran the conversation `conversation_id`, as its chunks'
/// provenance records it (`source_conv_id`): for a subagent's `agent-` id, the
/// parent session. `None` when no chunk of it carries one.
pub fn parent_session_of_conversation(
    conn: &Connection,
    conversation_id: &str,
) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT p.source_conv_id
         FROM chunks c JOIN chunk_provenance p ON p.chunk_id = c.id
         WHERE c.conversation_id = ?1 AND p.source_conv_id <> ''
         LIMIT 1",
    )?;
    let mut rows = stmt.query(params![conversation_id])?;
    Ok(match rows.next()? {
        Some(row) => Some(row.get(0)?),
        None => None,
    })
}

/// A reflection row: (id, content, tags, timestamp).
pub type ReflectionRow = (String, String, Vec<String>, String);

/// A session_registry row projection: (project, first_ts, last_ts). Timestamps are
/// nullable in the schema (registry rows can be created before a session closes).
pub type SessionWindowRow = (String, Option<String>, Option<String>);

/// Aggregated session info for timeline display.
/// JOINs chunk data with enrichment reflections for rich context.
pub struct SessionInfo {
    pub conversation_id: String,
    pub project_name: String,
    pub timestamp: String,
    pub total_messages: usize,
    pub chunk_count: usize,
    pub summary: Option<String>,
    pub enrichment: Option<String>,
}

/// Serialize a f32 vector to bytes (little-endian).
fn vec_to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Deserialize bytes back to f32 vector.
fn bytes_to_vec(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect()
}

pub fn insert_chunk(conn: &Connection, chunk: &ConversationChunk, embedding: &[f32]) -> Result<()> {
    insert_chunk_with_source(conn, chunk, embedding, "conversation")
}

/// `source` is a storage-level attribute ('conversation' | 'plan'), deliberately NOT a
/// `ConversationChunk` field: ~45 construction sites would churn for a value only the
/// aux-source importers set. Readers that need it distinguish plan chunks by the
/// `conversation_id` prefix `plan:` instead.
pub fn insert_chunk_with_source(
    conn: &Connection,
    chunk: &ConversationChunk,
    embedding: &[f32],
    source: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO chunks (id, conversation_id, project_name, timestamp, content, message_count, summary, seq, is_sidechain, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(id) DO UPDATE SET
             conversation_id = excluded.conversation_id,
             project_name = excluded.project_name,
             timestamp = excluded.timestamp,
             content = excluded.content,
             message_count = excluded.message_count,
             summary = excluded.summary,
             seq = excluded.seq,
             is_sidechain = excluded.is_sidechain,
             source = excluded.source",
        params![
            chunk.id,
            chunk.conversation_id,
            chunk.project_name,
            chunk.timestamp,
            chunk.content,
            chunk.message_count as i64,
            chunk.summary,
            chunk.seq as i64,
            chunk.is_sidechain as i64,
            source,
        ],
    )?;

    conn.execute(
        "INSERT INTO chunk_embeddings (chunk_id, embedding) VALUES (?1, ?2)
         ON CONFLICT(chunk_id) DO UPDATE SET embedding = excluded.embedding",
        params![chunk.id, vec_to_bytes(embedding)],
    )?;

    Ok(())
}

/// Read back a chunk's storage-level `source` ('conversation' | 'plan' | ...). The
/// column is deliberately absent from `ConversationChunk` (see `insert_chunk_with_source`),
/// so aux-source adapter tests need an explicit read path to assert on it.
pub fn get_chunk_source(conn: &Connection, id: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT source FROM chunks WHERE id = ?1",
        params![id],
        |r| r.get(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Repair sidechain rows imported by the recursive watcher before it understood
/// the nested path layout. Additive-safe and idempotent; called only by explicit
/// sidechain discovery/reimport, never by the normal storage startup path.
pub fn rescope_sidechain_conversation(
    conn: &mut Connection,
    conversation_id: &str,
    project_name: &str,
    parent_conversation_id: &str,
) -> Result<()> {
    let needs_repair = conn.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM chunks c
             LEFT JOIN chunk_provenance p ON p.chunk_id = c.id
             WHERE c.conversation_id = ?1
               AND (c.project_name IS NOT ?2
                    OR c.source IS NOT 'sidechain'
                    OR c.is_sidechain != 1
                    OR p.source_conv_id IS NOT ?3)
         )",
        params![conversation_id, project_name, parent_conversation_id],
        |row| row.get::<_, i64>(0),
    )? != 0;
    if !needs_repair {
        let tx = conn.transaction()?;
        super::sidechain_provenance::relink_conversations(&tx, &[conversation_id.into()])?;
        tx.commit()?;
        return Ok(());
    }
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE chunks SET project_name = ?2, source = 'sidechain', is_sidechain = 1
         WHERE conversation_id = ?1",
        params![conversation_id, project_name],
    )?;
    tx.execute(
        "INSERT INTO chunk_provenance (chunk_id, author, source_conv_id, supersedes)
         SELECT c.id, COALESCE(p.author, 'assistant'), ?2, p.supersedes
         FROM chunks c LEFT JOIN chunk_provenance p ON p.chunk_id = c.id
         WHERE c.conversation_id = ?1
         ON CONFLICT(chunk_id) DO UPDATE SET source_conv_id = excluded.source_conv_id",
        params![conversation_id, parent_conversation_id],
    )?;
    super::sidechain_provenance::relink_conversations(&tx, &[conversation_id.into()])?;
    tx.commit()?;
    Ok(())
}

/// Rewrite a reflection's cached `project_<old>` tag to `project_<new>` for
/// every reflection tied to `conversation_id` (via an exact `conv_<id>` tag)
/// that still carries the old value — the derived-narrative counterpart to
/// `rescope_sidechain_conversation`'s chunk-row rewrite. Reflections are
/// looked up by a `LIKE` prefilter (no index on the tags JSON blob) but only
/// ever mutated on an exact tag match, so a conversation id or project name
/// that happens to be a substring of another's never causes a false rewrite.
pub fn repair_reflection_project_tag(
    conn: &Connection,
    conversation_id: &str,
    old_project: &str,
    new_project: &str,
) -> Result<usize> {
    let conv_tag = format!("conv_{conversation_id}");
    let old_tag = format!("project_{old_project}");
    let new_tag = format!("project_{new_project}");
    let mut stmt = conn.prepare(
        "SELECT id, tags FROM reflections
         WHERE tags LIKE '%' || ?1 || '%' AND tags LIKE '%' || ?2 || '%'",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map(params![conv_tag, old_tag], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut repaired = 0usize;
    for (id, tags_json) in rows {
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        if !tags.iter().any(|t| t == &conv_tag) {
            continue; // the LIKE prefilter matched a substring, not the tag itself
        }
        let mut changed = false;
        let rewritten: Vec<String> = tags
            .into_iter()
            .map(|t| {
                if t == old_tag {
                    changed = true;
                    new_tag.clone()
                } else {
                    t
                }
            })
            .collect();
        if !changed {
            continue;
        }
        conn.execute(
            "UPDATE reflections SET tags = ?1 WHERE id = ?2",
            params![serde_json::to_string(&rewritten)?, id],
        )?;
        repaired += 1;
    }
    Ok(repaired)
}

/// Delete a whole conversation's chunks + their embeddings, FTS rows, and provenance
/// edges. Aux-source adapters (plans, tasks, ...) reimport a whole document on content
/// change, and the document can shrink — overwriting by deterministic chunk id alone
/// would leave stale tail chunks (and their embeddings) orphaned in search forever, so
/// a full wipe-then-rebuild is required for idempotent reimport.
pub fn delete_chunks_for_conversation(conn: &Connection, conversation_id: &str) -> Result<()> {
    let mut stmt = conn.prepare("SELECT id FROM chunks WHERE conversation_id = ?1")?;
    let ids: Vec<String> = stmt
        .query_map(params![conversation_id], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);

    for id in &ids {
        conn.execute(
            "DELETE FROM chunk_embeddings WHERE chunk_id = ?1",
            params![id],
        )?;
        conn.execute(
            "DELETE FROM chunk_provenance WHERE chunk_id = ?1",
            params![id],
        )?;
    }
    conn.execute(
        "DELETE FROM chunks WHERE conversation_id = ?1",
        params![conversation_id],
    )?;
    Ok(())
}

/// Remove the state of a conversation that should never have been corpus (a
/// CSR agent transcript): its chunks with their embeddings, FTS rows, spans and
/// provenance edges, and its enrichment and ratification rows with the
/// ratification receipts, which are keyed by conversation id. Reflections and
/// their receipts go separately (`purge_reflections`), only once nothing kept
/// cites them. Append-only history (`provenance_events`, retrieval and exposure
/// logs) stays as recorded. Returns the number of chunks removed.
pub fn purge_conversation(conn: &Connection, conversation_id: &str) -> Result<usize> {
    let chunks: i64 = conn.query_row(
        "SELECT COUNT(*) FROM chunks WHERE conversation_id = ?1",
        params![conversation_id],
        |row| row.get(0),
    )?;
    delete_chunks_for_conversation(conn, conversation_id)?;
    conn.execute(
        "DELETE FROM artifact_derivations WHERE artifact_kind = 'ratification' AND artifact_id = ?1",
        params![conversation_id],
    )?;
    conn.execute(
        "DELETE FROM ratification_scores WHERE conversation_id = ?1",
        params![conversation_id],
    )?;
    conn.execute(
        "DELETE FROM enrichment_state WHERE conversation_id = ?1",
        params![conversation_id],
    )?;
    Ok(chunks as usize)
}

/// `(conversation id, reflection id)` for every enrichment row that produced
/// a reflection.
pub fn enrichment_reflection_refs(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT conversation_id, reflection_id FROM enrichment_state WHERE reflection_id IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("{}", e))
}

/// `(reflection id, conversation ids)` for every reflection carrying at least
/// one `conv_<id>` tag, the tag each conversation-derived reflection is stored
/// with (episodes, V3 extractions, heuristics, stories).
pub fn reflection_conversation_tags(conn: &Connection) -> Result<Vec<(String, Vec<String>)>> {
    let mut stmt = conn.prepare("SELECT id, tags FROM reflections WHERE tags LIKE '%\"conv_%'")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut tagged = Vec::new();
    for row in rows {
        let (id, tags) = row?;
        let conversations: Vec<String> = serde_json::from_str::<Vec<String>>(&tags)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|tag| tag.strip_prefix("conv_").map(str::to_string))
            .collect();
        if !conversations.is_empty() {
            tagged.push((id, conversations));
        }
    }
    Ok(tagged)
}

/// Delete reflections with their embeddings and derivation receipts. A
/// reflection some enrichment row still cites is left alone: the caller removes
/// the rows of the conversations it purges first, so a surviving citation
/// belongs to a conversation that is being kept. Returns the number deleted.
pub fn purge_reflections(conn: &Connection, ids: &[String]) -> Result<usize> {
    let cited = enrichment_reflection_refs(conn)?
        .into_iter()
        .map(|(_, reflection_id)| reflection_id)
        .collect::<std::collections::HashSet<_>>();
    let mut deleted = 0;
    for id in ids.iter().filter(|id| !cited.contains(*id)) {
        conn.execute(
            "DELETE FROM artifact_derivations WHERE artifact_kind = 'reflection' AND artifact_id = ?1",
            params![id],
        )?;
        delete_reflection(conn, id)?;
        deleted += 1;
    }
    Ok(deleted)
}

pub fn delete_chunk(conn: &Connection, chunk_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM chunk_embeddings WHERE chunk_id = ?1",
        params![chunk_id],
    )?;
    conn.execute(
        "DELETE FROM chunk_provenance WHERE chunk_id = ?1",
        params![chunk_id],
    )?;
    conn.execute("DELETE FROM chunks WHERE id = ?1", params![chunk_id])?;
    Ok(())
}

pub fn load_all_chunk_vectors(conn: &Connection) -> Result<Vec<(String, Vec<f32>)>> {
    let mut stmt = conn.prepare("SELECT chunk_id, embedding FROM chunk_embeddings")?;
    let rows = stmt.query_map([], |row| {
        let id: String = row.get(0)?;
        let bytes: Vec<u8> = row.get(1)?;
        Ok((id, bytes_to_vec(&bytes)))
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

pub fn get_chunks_by_ids(conn: &Connection, ids: &[String]) -> Result<Vec<ConversationChunk>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, project_name, timestamp, content, message_count, summary, is_sidechain
         FROM chunks WHERE id = ?1",
    )?;
    let mut chunks = Vec::new();
    for id in ids {
        let mut rows = stmt.query_map(params![id], row_to_chunk)?;
        if let Some(row) = rows.next() {
            chunks.push(row?);
        }
    }
    Ok(chunks)
}

/// Like [`get_chunks_by_ids`], but resolves each chunk's true author via a
/// `LEFT JOIN` on `chunk_provenance` instead of defaulting every chunk to
/// `Speaker::ToolResult`. `get_chunks_by_ids` intentionally does not carry
/// author (it's cheaper and most callers don't need it — reinstatement graph
/// walks, MCP display, eval metadata); this variant exists for callers that
/// filter on `ConversationChunk::author`, e.g. ratification's `build_digest`,
/// which prioritizes `Speaker::User` turns. Chunks with no provenance row, or
/// an unrecognized author token, degrade to `Speaker::ToolResult` — same
/// fallback as [`get_chunk_provenance`].
pub fn get_chunks_by_ids_with_provenance(
    conn: &Connection,
    ids: &[String],
) -> Result<Vec<ConversationChunk>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT c.id, c.conversation_id, c.project_name, c.timestamp, c.content,
                c.message_count, c.summary, c.is_sidechain, cp.author
         FROM chunks c LEFT JOIN chunk_provenance cp ON cp.chunk_id = c.id
         WHERE c.id = ?1",
    )?;
    let mut chunks = Vec::new();
    for id in ids {
        let mut rows = stmt.query_map(params![id], row_to_chunk_with_author)?;
        if let Some(row) = rows.next() {
            chunks.push(row?);
        }
    }
    Ok(chunks)
}

pub fn insert_reflection(
    conn: &Connection,
    id: &str,
    content: &str,
    tags: &[String],
    embedding: &[f32],
) -> Result<()> {
    let tags_json = serde_json::to_string(tags)?;
    let now = chrono::Utc::now().to_rfc3339();

    conn.execute(
        "INSERT OR REPLACE INTO reflections (id, content, tags, timestamp) VALUES (?1, ?2, ?3, ?4)",
        params![id, content, tags_json, now],
    )?;

    conn.execute(
        "INSERT OR REPLACE INTO reflection_embeddings (reflection_id, embedding) VALUES (?1, ?2)",
        params![id, vec_to_bytes(embedding)],
    )?;

    let message_key = crate::provenance::content_hash(content);
    let event_id = blake3::hash(format!("reflection\0{id}\0{message_key}").as_bytes())
        .to_hex()
        .to_string();
    insert_provenance_event(
        conn,
        &ProvenanceEvent {
            event_id: format!("reflection:{event_id}"),
            conversation_id: format!("reflection:{id}"),
            message_key,
            seq: 0,
            channel: "reflection".to_string(),
            trust_tier: TrustTier::Unknown,
            parent_event_id: None,
            receipt_kind: "reflection_row".to_string(),
            receipt_ref: Some(id.to_string()),
            observed_at: now,
        },
    )?;

    Ok(())
}

pub fn load_all_reflection_ids(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT reflection_id FROM reflection_embeddings")?;
    let rows = stmt.query_map([], |row| row.get(0))?;
    rows.collect::<std::result::Result<Vec<String>, _>>()
        .map_err(|e| anyhow::anyhow!("{}", e))
}

/// Load just the chunk IDs (no vectors) — cheap probe for incremental backfill.
pub fn load_all_chunk_ids(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT chunk_id FROM chunk_embeddings")?;
    let rows = stmt.query_map([], |row| row.get(0))?;
    rows.collect::<std::result::Result<Vec<String>, _>>()
        .map_err(|e| anyhow::anyhow!("{}", e))
}

pub fn load_all_reflection_vectors(conn: &Connection) -> Result<Vec<(String, Vec<f32>)>> {
    let mut stmt = conn.prepare("SELECT reflection_id, embedding FROM reflection_embeddings")?;
    let rows = stmt.query_map([], |row| {
        let id: String = row.get(0)?;
        let bytes: Vec<u8> = row.get(1)?;
        Ok((id, bytes_to_vec(&bytes)))
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

pub fn get_reflection_by_id(
    conn: &Connection,
    id: &str,
) -> Result<Option<(String, Vec<String>, String)>> {
    let mut stmt =
        conn.prepare("SELECT content, tags, timestamp FROM reflections WHERE id = ?1")?;
    let mut rows = stmt.query_map(params![id], |row| {
        let content: String = row.get(0)?;
        let tags_json: String = row.get(1)?;
        let timestamp: String = row.get(2)?;
        Ok((content, tags_json, timestamp))
    })?;

    if let Some(row) = rows.next() {
        let (content, tags_json, timestamp) = row?;
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        Ok(Some((content, tags, timestamp)))
    } else {
        Ok(None)
    }
}

// ─── Project filtering queries ───

/// Get all chunk IDs belonging to a specific project.
pub fn get_chunk_ids_for_project(conn: &Connection, project: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT id FROM chunks WHERE project_name = ?1")?;
    let rows = stmt.query_map(params![project], |row| row.get::<_, String>(0))?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row?);
    }
    Ok(ids)
}

/// Get chunks belonging to a specific project, ordered by timestamp descending.
pub fn get_chunks_by_project(
    conn: &Connection,
    project: &str,
    limit: usize,
) -> Result<Vec<ConversationChunk>> {
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, project_name, timestamp, content, message_count, summary, is_sidechain
         FROM chunks WHERE project_name = ?1 ORDER BY timestamp DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![project, limit as i64], row_to_chunk)?;
    let mut chunks = Vec::new();
    for row in rows {
        chunks.push(row?);
    }
    Ok(chunks)
}

// ─── Temporal queries ───

/// True for a project namespace that is a headless workflow cwd namespace —
/// a scratchpad/cleanroom directory a subagent or workflow harness worked
/// in, never a session a person typed into. These share the family's
/// hyphenated prefix (same repo, different working directory), so
/// `same_project_family` legitimately puts them in the family, but recent
/// work should never surface them ahead of a real interactive session.
/// Substring match, not a suffix check: the namespace can carry more path
/// after the marker (e.g. a nested worktree id).
fn is_headless_scratchpad_namespace(project_name: &str) -> bool {
    project_name.contains("-scratchpad-")
}

/// Project namespaces in the same family as `project` (per
/// `cross_project::same_project_family_at_root`), plus `project` itself,
/// resolved against `projects_root`. Hermetic seam for tests only — the
/// production entry point is `recent_work_family_projects`, which goes
/// through the crate's cached `same_project_family` instead of this
/// uncached, explicit-root variant (see that function's doc comment).
/// Mirrors the pattern in `storage::codegraph::family_projects`.
#[cfg(test)]
fn recent_work_family_projects_at_root(
    conn: &Connection,
    project: &str,
    projects_root: &Path,
) -> Result<Vec<String>> {
    let mut projects = list_project_names(conn, "", i64::MAX as usize)?;
    projects.retain(|candidate| {
        crate::search::cross_project::same_project_family_at_root(project, candidate, projects_root)
            && !is_headless_scratchpad_namespace(candidate)
    });
    if !projects.iter().any(|p| p == project) {
        projects.push(project.to_string());
    }
    Ok(projects)
}

/// `home`-parameterized double of `recent_work_family_projects`. Uses the
/// cached-`is_dir`, explicit-root `same_project_family_at_cached_root` (same
/// process-lifetime memo, same performance characteristics as the crate's
/// global `same_project_family`) instead of delegating to the global
/// `dirs::home_dir()`-reading `same_project_family` directly, so the
/// no-resolvable-home branch can be exercised deterministically in a test
/// regardless of this machine's real `$HOME`. If there is no resolvable home
/// directory, an explicit `projects_root` can't be formed at all, so this
/// returns exact scope only — never falls back to a CWD-relative "projects"
/// directory, which would let an unrelated directory in the process's
/// current working directory leak into the family.
fn recent_work_family_projects_with_home(
    conn: &Connection,
    project: &str,
    home: Option<&Path>,
) -> Result<Vec<String>> {
    if home.is_none() {
        return Ok(vec![project.to_string()]);
    }
    let projects_root = home.unwrap().join("projects");
    let mut projects = list_project_names(conn, "", i64::MAX as usize)?;
    projects.retain(|candidate| {
        crate::search::cross_project::same_project_family_at_cached_root(
            project,
            candidate,
            &projects_root,
        ) && !is_headless_scratchpad_namespace(candidate)
    });
    if !projects.iter().any(|p| p == project) {
        projects.push(project.to_string());
    }
    Ok(projects)
}

/// Production entry point. Threads the real `dirs::home_dir()` into
/// `recent_work_family_projects_with_home` — see that function's doc for why
/// the no-home-dir case is a seam parameter rather than an inline check.
fn recent_work_family_projects(conn: &Connection, project: &str) -> Result<Vec<String>> {
    recent_work_family_projects_with_home(conn, project, dirs::home_dir().as_deref())
}

/// Row shape for `get_recent_chunks_in_projects`: the representative (first)
/// chunk of a conversation, its source adapter, and the raw first-chunk
/// timestamp (used only for headless codex burst detection — display/ordering
/// use `chunk.timestamp`, which this sets to the conversation's
/// *last-activity* timestamp, not the first chunk's).
struct RecentChunkRow {
    chunk: ConversationChunk,
    source: String,
    first_chunk_ts: String,
}

/// UTC-second bucket of an RFC3339-ish timestamp (`YYYY-MM-DDTHH:MM:SS`).
/// Fallback grouping key when a timestamp doesn't parse as RFC3339.
fn burst_second(ts: &str) -> &str {
    if ts.len() >= 19 {
        &ts[..19]
    } else {
        ts
    }
}

/// Same-UTC-second grouping under-collapses real headless bursts: parallel
/// codex spawns commonly land a few seconds apart, not in the same second.
/// Anything within this many seconds of the burst's head timestamp counts as
/// the same burst.
const CODEX_BURST_WINDOW_SECS: i64 = 10;

fn within_burst_window(head_ts: &str, candidate_ts: &str) -> bool {
    match (
        head_ts.parse::<chrono::DateTime<chrono::Utc>>(),
        candidate_ts.parse::<chrono::DateTime<chrono::Utc>>(),
    ) {
        (Ok(head), Ok(candidate)) => {
            (head - candidate).num_seconds().abs() <= CODEX_BURST_WINDOW_SECS
        }
        // Unparsable timestamps: fall back to the old same-second prefix
        // rather than silently never collapsing.
        _ => burst_second(head_ts) == burst_second(candidate_ts),
    }
}

/// Best-effort spawn time embedded in a UUIDv7-shaped Codex conversation id
/// (`codex:<uuid>`) — the first 48 bits of the UUID, which UUIDv7 defines as
/// a big-endian Unix millisecond timestamp. `None` for a legacy or malformed
/// id (not a well-formed 32-hex-digit UUID, or the version nibble isn't
/// `7`), so callers fall back to a chunk-derived timestamp instead.
fn codex_spawn_time_from_id(conversation_id: &str) -> Option<String> {
    let uuid_part = conversation_id.strip_prefix("codex:")?;
    let hex: String = uuid_part.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    // UUIDv7 version nibble: 13th hex digit (0-indexed 12) is always `7`.
    if hex.as_bytes()[12] != b'7' {
        return None;
    }
    let ms = u64::from_str_radix(&hex[..12], 16).ok()?;
    let secs = (ms / 1000) as i64;
    let nanos = ((ms % 1000) * 1_000_000) as u32;
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, nanos).map(|dt| dt.to_rfc3339())
}

/// Effective burst-grouping timestamp for a codex conversation: the id's own
/// embedded UUIDv7 spawn time when available, falling back to
/// `first_chunk_ts` for a legacy/malformed id.
///
/// The id's timestamp is used in preference to `first_chunk_ts` because
/// `restamp_codex_chunk_timestamps` (`codex_rollout_chunk_ts_restamp_v1`)
/// rewrites stored chunk timestamps to the *message* time, not the
/// rollout's spawn time. On a restamped corpus, `first_chunk_ts`
/// (`MIN(timestamp)` per conversation) no longer approximates when parallel
/// spawns actually happened, and two rollouts spawned 100ms apart via
/// `thread_spawn` can show first messages minutes apart. A UUIDv7's
/// embedded timestamp survives the restamp untouched.
fn codex_burst_key_ts(conversation_id: &str, first_chunk_ts: &str) -> String {
    codex_spawn_time_from_id(conversation_id).unwrap_or_else(|| first_chunk_ts.to_string())
}

/// Exact count of `codex_rollout` conversations (non-sidechain, in scope)
/// whose `codex_burst_key_ts` falls within `CODEX_BURST_WINDOW_SECS` of
/// `head_ts` (inclusive, looking backward only — matching `within_burst_window`
/// when scanning a descending-timestamp list). Used only as a fallback when
/// the fetched row window ends mid-burst and the window-based scan in
/// `collapse_codex_bursts` can't see past its own edge. `Ok(None)`
/// (unparsable `head_ts`) leaves the caller's window-based count as-is
/// rather than reporting a bogus one.
fn count_codex_burst_members(
    conn: &Connection,
    projects: &[String],
    head_ts: &str,
) -> Result<Option<usize>> {
    if head_ts.parse::<chrono::DateTime<chrono::Utc>>().is_err() {
        return Ok(None);
    }
    // Fetch each in-scope codex conversation's id and first-chunk timestamp
    // (the fallback when the id isn't a UUIDv7) and filter by *parsed*
    // comparison in Rust, not a SQL string BETWEEN. Timestamps in this table
    // are RFC3339 text written by different adapters/call sites that don't
    // agree on a UTC-offset suffix ("Z" vs "+00:00"): a string BETWEEN
    // against a chrono-formatted bound silently excludes the exact-equality
    // case whenever the suffixes differ (was observed undercounting the
    // burst's own head row by one). This set is bounded by the in-scope
    // project's own codex conversation count, not the corpus, so an
    // in-memory filter is cheap.
    let sql = if projects.is_empty() {
        "SELECT conversation_id, MIN(timestamp) AS first_ts FROM chunks \
         WHERE source = 'codex_rollout' AND is_sidechain = 0 GROUP BY conversation_id"
            .to_string()
    } else {
        format!(
            "SELECT conversation_id, MIN(timestamp) AS first_ts FROM chunks \
             WHERE project_name IN ({}) AND source = 'codex_rollout' AND is_sidechain = 0 \
             GROUP BY conversation_id",
            string_placeholders(projects.len()),
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<(String, String)> = if projects.is_empty() {
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let qparams: Vec<&dyn rusqlite::ToSql> =
            projects.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
        stmt.query_map(qparams.as_slice(), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let count = rows
        .iter()
        .filter(|(id, first_ts)| within_burst_window(head_ts, &codex_burst_key_ts(id, first_ts)))
        .count();
    Ok(Some(count))
}

/// Collapse consecutive `codex_rollout` conversations whose first-chunk
/// timestamps land within `CODEX_BURST_WINDOW_SECS` of the burst's head row
/// into a single row annotated with `+N parallel`, honouring `limit` on the
/// *collapsed* row count. Rows must already be ordered by last-activity
/// descending, so siblings in a burst are adjacent.
///
/// `rows` is a bounded fetch window, not the whole corpus. When a burst's
/// member scan runs all the way to the end of `rows` without hitting a
/// non-member (i.e. the fetch window itself, not the burst, is what stopped
/// the scan), the window-based `extra` count is a lower bound, not the true
/// count — the burst may have more siblings past the window edge. This can
/// happen even when the collapsed output already satisfies `limit` (a solo
/// row ahead of an oversized burst fills `limit` before the burst is fully
/// walked), so the caller's widening retry never gets a chance to fetch a
/// wider window. Run an exact follow-up count in that case rather than
/// reporting a partial one.
fn collapse_codex_bursts(
    conn: &Connection,
    projects: &[String],
    rows: Vec<RecentChunkRow>,
    limit: usize,
) -> Result<Vec<ConversationChunk>> {
    const CODEX_SOURCE: &str = "codex_rollout";
    let mut out = Vec::new();
    let mut i = 0;
    while i < rows.len() && out.len() < limit {
        let row = &rows[i];
        if row.source == CODEX_SOURCE {
            let head_ts = codex_burst_key_ts(&row.chunk.conversation_id, &row.first_chunk_ts);
            let mut j = i + 1;
            let mut extra = 0usize;
            while j < rows.len()
                && rows[j].source == CODEX_SOURCE
                && within_burst_window(
                    &head_ts,
                    &codex_burst_key_ts(&rows[j].chunk.conversation_id, &rows[j].first_chunk_ts),
                )
            {
                extra += 1;
                j += 1;
            }
            if j == rows.len() {
                if let Some(exact) = count_codex_burst_members(conn, projects, &head_ts)? {
                    extra = extra.max(exact.saturating_sub(1));
                }
            }
            let mut chunk = row.chunk.clone();
            if extra > 0 {
                chunk.content = format!("[+{extra} parallel] {}", chunk.content);
            }
            out.push(chunk);
            i = j;
        } else {
            out.push(row.chunk.clone());
            i += 1;
        }
    }
    Ok(out)
}

/// Distinct non-sidechain conversation ids, ordered by last-activity
/// (`MAX(timestamp)`) descending. Dispatches on whether the call is scoped to
/// a project family (`projects` non-empty) — the two shapes want different
/// indexes and neither is a good plan for the other:
///
/// - Unscoped (`projects` empty): drives entirely off `idx_chunks_timestamp`
///   via `recent_conversation_ids_by_activity_unscoped`, widening the window
///   until enough distinct conversations are found. A `GROUP BY` here would
///   force a full-table aggregation (553k+ rows on the live DB) for every
///   call.
/// - Scoped: a `GROUP BY ... WHERE project_name IN (...)` driven off
///   `idx_chunks_project` (`recent_conversation_ids_by_activity_scoped`).
///   This used to also go through the timestamp-index widening scan, which
///   is fast for the dominant project but forces a full
///   `idx_chunks_timestamp` walk (doubling on every retry) for any
///   non-dominant one — measured up to 1.95s cold for a 1-chunk project vs
///   0.0009s for the old `GROUP BY`/`idx_chunks_project` plan. `GROUP BY`
///   costs proportional to the project's own row count, not the whole
///   corpus, and needs no widening loop: the aggregate already found every
///   matching conversation's true last-activity timestamp in one pass.
fn recent_conversation_ids_by_activity(
    conn: &Connection,
    projects: &[String],
    target: usize,
) -> Result<Vec<(String, String)>> {
    if projects.is_empty() {
        recent_conversation_ids_by_activity_unscoped(conn, target)
    } else {
        recent_conversation_ids_by_activity_scoped(conn, projects, target)
    }
}

/// Scoped branch of `recent_conversation_ids_by_activity`: exact,
/// single-pass `GROUP BY` over `idx_chunks_project`. See that function's doc
/// comment for why this and the unscoped branch use different plans.
fn recent_conversation_ids_by_activity_scoped(
    conn: &Connection,
    projects: &[String],
    target: usize,
) -> Result<Vec<(String, String)>> {
    let sql = format!(
        "SELECT conversation_id, MAX(timestamp) AS last_ts \
         FROM chunks INDEXED BY idx_chunks_project \
         WHERE project_name IN ({}) AND is_sidechain = 0 \
         GROUP BY conversation_id ORDER BY last_ts DESC LIMIT ?{}",
        string_placeholders(projects.len()),
        projects.len() + 1,
    );
    let mut stmt = conn.prepare(&sql)?;
    let target_i64 = target as i64;
    let mut qparams: Vec<&dyn rusqlite::ToSql> =
        projects.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
    qparams.push(&target_i64);
    let ids = stmt
        .query_map(qparams.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}

/// Unscoped branch of `recent_conversation_ids_by_activity`: adaptive
/// `idx_chunks_timestamp` scan, widening until enough distinct conversations
/// are found or the table is exhausted. See that function's doc comment.
fn recent_conversation_ids_by_activity_unscoped(
    conn: &Connection,
    target: usize,
) -> Result<Vec<(String, String)>> {
    let mut window = target.max(64);
    loop {
        let sql = "SELECT conversation_id, timestamp FROM chunks INDEXED BY idx_chunks_timestamp \
             WHERE is_sidechain = 0 ORDER BY timestamp DESC LIMIT ?1";
        let mut stmt = conn.prepare(sql)?;
        let window_i64 = window as i64;
        let raw_rows: Vec<(String, String)> = stmt
            .query_map(params![window_i64], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let fetched = raw_rows.len();
        let mut seen = std::collections::HashSet::with_capacity(target);
        let mut ids = Vec::with_capacity(target.min(fetched));
        for (conv_id, ts) in raw_rows {
            if seen.insert(conv_id.clone()) {
                ids.push((conv_id, ts));
            }
        }

        let exhausted = fetched < window;
        if ids.len() >= target || exhausted {
            ids.truncate(target);
            return Ok(ids);
        }
        window = window.saturating_mul(2);
    }
}

/// How many of a conversation's earliest-by-rowid chunks are considered as
/// preview candidates in `recent_chunk_rows_for`. Bounded so the query stays
/// cheap regardless of conversation size. Raised from 8 (W3-B1 review#2,
/// MEDIUM): a still-polluted Codex rollout's harness block routinely spans
/// well past 8 stored chunks (CSR's chunker has no idea the source text was
/// boilerplate, so it keeps splitting it at its normal char budget) — a live
/// case measured the real prompt landing at seq 12. 32 covers that with
/// margin while staying a small, bounded `rowid <` range.
const PREVIEW_CANDIDATE_WINDOW: i64 = 32;

/// True when `content` is harness/plumbing noise that must never win as a
/// conversation's recent-work preview, even when it is the technically
/// first-by-rowid chunk:
/// - a Codex harness-boilerplate item (`is_harness_context_item`) — the
///   pre-strip `<recommended_plugins>` etc. block a still-polluted rollout
///   carries as chunk 0 until it's reimported through the harness-marker-
///   stripping adapter.
/// - a Claude Code local-command wrapper (`<local-command-caveat>`,
///   `<local-command-stdout>`, `<command-name>`) — the harness's own echo of
///   a typed `/login`, `/model`, etc., never anything a person wrote.
fn is_noisy_preview(source: &str, content: &str) -> bool {
    if source == "codex_rollout" && crate::import::codex_rollout::is_harness_context_item(content) {
        return true;
    }
    content.starts_with("<local-command-caveat>")
        || content.starts_with("<local-command-stdout>")
        || content.starts_with("<command-name>")
}

/// Open/close tag pairs for the harness blocks `import::codex_rollout`'s
/// (private) `CODEX_HARNESS_MARKERS` can open, observed on real rollouts.
/// Used to track a running *nesting depth* across a Codex rollout's harness
/// preamble rather than a plain "have we seen any closing tag yet" flag: on
/// the live corpus, multiple blocks are packed back-to-back inside ONE
/// original message with no chunk-boundary alignment — a live case has
/// `<recommended_plugins>`'s closing tag and the *next* block's
/// (`<INSTRUCTIONS>`, the body of "# AGENTS.md instructions") opening tag
/// landing in the very same stored chunk. A "first closing tag ends the
/// span" rule reopens the pick right there, on that next block's own
/// boilerplate body, instead of staying closed until IT closes too. Net
/// depth (total opens minus total closes, accumulated chunk by chunk) stays
/// open across exactly that case, however many blocks are chained.
const CODEX_HARNESS_TAG_PAIRS: &[(&str, &str)] = &[
    ("<recommended_plugins>", "</recommended_plugins>"),
    ("<INSTRUCTIONS>", "</INSTRUCTIONS>"),
    ("<environment_context>", "</environment_context>"),
    ("<user_instructions>", "</user_instructions>"),
    ("<permissions instructions>", "</permissions instructions>"),
    ("<codex_internal_context>", "</codex_internal_context>"),
    ("<skills_instructions>", "</skills_instructions>"),
];

/// Net harness-tag depth contributed by one chunk's content: total opening
/// occurrences minus total closing occurrences, summed across every pair in
/// `CODEX_HARNESS_TAG_PAIRS`. Can be negative (a chunk holding only a
/// closing tag with no open of its own — the common case for the chunk that
/// finally ends a multi-chunk span).
fn harness_tag_depth_delta(content: &str) -> i32 {
    CODEX_HARNESS_TAG_PAIRS
        .iter()
        .map(|(open, close)| {
            content.matches(open).count() as i32 - content.matches(close).count() as i32
        })
        .sum()
}

/// Index of the first candidate in `candidates` (ordered ascending by rowid,
/// i.e. import order) that is a real recent-work preview — skipping not just
/// individually-noisy chunks (`is_noisy_preview`) but a whole open Codex
/// harness span once one starts, tracked as a running tag-nesting `depth`
/// (see `harness_tag_depth_delta`) so a span that closes one block and opens
/// the next within the same chunk stays open, not reset. Falls back to `0`
/// (the group's true first chunk) when every candidate in the window is
/// noise/span, so a preview is never lost outright.
fn pick_preview_index(candidates: &[(ConversationChunk, String, String)]) -> usize {
    let mut depth: i32 = 0;
    for (i, (chunk, source, _)) in candidates.iter().enumerate() {
        let is_codex = source == "codex_rollout";
        let opens_here =
            is_codex && crate::import::codex_rollout::is_harness_context_item(&chunk.content);
        if depth > 0 || opens_here {
            if is_codex {
                depth = (depth + harness_tag_depth_delta(&chunk.content)).max(0);
            }
            continue;
        }
        if is_noisy_preview(source, &chunk.content) {
            continue;
        }
        return i;
    }
    0
}

/// Resolve each conversation's representative chunk plus its source
/// adapter, keyed off already-resolved `(conversation_id,
/// last_activity_timestamp)` pairs. One bounded `IN (...)` query rather than
/// a point query per id; ordering of the input pairs is preserved on output.
///
/// The representative chunk is the first (by `seq`, i.e. the conversation's
/// true message order — see the `ROW_NUMBER()` window below) among the
/// first `PREVIEW_CANDIDATE_WINDOW` chunks that isn't `is_noisy_preview` —
/// not simply the earliest chunk — so a harness-marker-polluted Codex
/// rollout or a Claude session that opens with `/login` previews its first
/// real content instead of boilerplate. Falls back to the group's very
/// first chunk when every candidate in the window is noise, so a preview is
/// never lost outright. `first_chunk_ts` (used only for headless codex
/// burst detection) always stays the conversation's true first-chunk
/// timestamp regardless of which chunk wins as the preview.
///
/// W3-B1 review#2 (LOW): bounds by `seq < N`, not a `rowid <` range.
/// `rowid` is insertion order, not conversation order — the watcher imports
/// incrementally and interleaves conversations, so two chunks of the *same*
/// conversation can land dozens of rows apart, and a `rowid <` window sized
/// for contiguous rowids can end up holding a single chunk of a session that
/// legitimately has more. `seq` (written on every insert — see
/// `insert_chunk_with_source`) is the session's own message order and has
/// none of that interleaving; every conversation's true first chunk is
/// always `seq = 0` (`push_chunk` in `import/mod.rs` assigns it as the
/// chunk's own index within a full from-scratch re-parse of the file, never
/// a running total across incremental passes), so — unlike `rowid`, whose
/// per-conversation minimum genuinely varies and has to be looked up — the
/// bound is the plain literal `?`, no correlated subquery needed on that
/// branch. The `rowid` branch (and its `MIN(rowid)` lookup) stays only as
/// the fallback for the sliver of rows written before the nullable `seq`
/// column existed and were never backfilled (see that migration's comment);
/// per-row short-circuit evaluation means the subquery only ever runs for
/// those legacy rows, so this keeps the original query's performance
/// characteristics for every row that already has `seq`.
fn recent_chunk_rows_for(
    conn: &Connection,
    ids: &[(String, String)],
) -> Result<Vec<RecentChunkRow>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = string_placeholders(ids.len());
    let sql = format!(
        "SELECT c.id, c.conversation_id, c.project_name, c.content, c.message_count, \
                c.summary, c.is_sidechain, c.source, c.timestamp \
         FROM chunks c \
         WHERE c.conversation_id IN ({placeholders}) \
           AND ( \
                 (c.seq IS NOT NULL AND c.seq < ?{}) \
              OR (c.seq IS NULL AND c.rowid < (SELECT MIN(rowid) FROM chunks INDEXED BY idx_chunks_conversation \
                                                 WHERE conversation_id = c.conversation_id) + ?{}) \
           ) \
         ORDER BY c.conversation_id, COALESCE(c.seq, c.rowid)",
        ids.len() + 1,
        ids.len() + 2,
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut qparams: Vec<&dyn rusqlite::ToSql> = ids
        .iter()
        .map(|(id, _)| id as &dyn rusqlite::ToSql)
        .collect();
    qparams.push(&PREVIEW_CANDIDATE_WINDOW);
    qparams.push(&PREVIEW_CANDIDATE_WINDOW);
    let candidate_rows = stmt
        .query_map(qparams.as_slice(), |row| {
            Ok((
                row.get::<_, String>(1)?,
                ConversationChunk {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    project_name: row.get(2)?,
                    timestamp: String::new(), // patched below to last-activity
                    content: row.get(3)?,
                    message_count: row.get::<_, i64>(4)? as usize,
                    summary: row.get(5)?,
                    author: crate::provenance::Speaker::ToolResult,
                    seq: 0,
                    is_sidechain: row.get::<_, i64>(6)? != 0,
                },
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // Group candidate rows by conversation, preserving the SQL's `ORDER BY
    // rowid` (ascending, so index 0 is always the true first chunk).
    let mut by_id: HashMap<String, Vec<(ConversationChunk, String, String)>> = HashMap::new();
    for (conv_id, chunk, source, ts) in candidate_rows {
        by_id.entry(conv_id).or_default().push((chunk, source, ts));
    }

    let mut out = Vec::with_capacity(ids.len());
    for (conv_id, last_ts) in ids {
        let Some(mut candidates) = by_id.remove(conv_id) else {
            continue;
        };
        let first_chunk_ts = candidates[0].2.clone();
        let pick = pick_preview_index(&candidates);
        let (mut chunk, source, _) = candidates.swap_remove(pick);
        chunk.timestamp = last_ts.clone();
        out.push(RecentChunkRow {
            chunk,
            source,
            first_chunk_ts,
        });
    }
    Ok(out)
}

/// Entrypoint values (`sdk-cli`, `sdk-headless`, any other `sdk-*` variant)
/// that are a *positive* signal a conversation was never driven by a typed
/// prompt — the harness only ever stamps these on a headless `claude -p`
/// (SDK) invocation, never on an interactive session. `cli` and
/// `claude-desktop` (and anything else) are real, human-facing entrypoints
/// and must never be treated as headless here.
fn is_headless_entrypoint(entrypoint: &str) -> bool {
    entrypoint.starts_with("sdk-")
}

/// `conversation_id -> entrypoint` for every id in `candidates` that has a
/// row in `conversation_entrypoint` (see that table's migration comment).
/// Ids absent from the map are "unknown", not "headless" — callers must
/// fail open on absence, never treat it as the positive signal.
fn conversation_entrypoints(
    conn: &Connection,
    candidates: &[String],
) -> Result<HashMap<String, String>> {
    if candidates.is_empty() {
        return Ok(HashMap::new());
    }
    // Batched ≤500 candidates per statement — see `known_session_ids`'s doc
    // for why (SQLITE_MAX_VARIABLE_NUMBER on large transcript dirs).
    const BATCH: usize = 500;
    let mut out = HashMap::new();
    for batch in candidates.chunks(BATCH) {
        let placeholders = string_placeholders(batch.len());
        let sql = format!(
            "SELECT conversation_id, entrypoint FROM conversation_entrypoint \
             WHERE conversation_id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let qparams: Vec<&dyn rusqlite::ToSql> =
            batch.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(qparams.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, ep) = row?;
            out.insert(id, ep);
        }
    }
    Ok(out)
}

/// The ids in `candidates` whose recorded entrypoint is headless (`sdk-*`).
/// Ids with no recorded entrypoint are unknown and never returned.
pub fn headless_conversation_ids(
    conn: &Connection,
    candidates: &[String],
) -> Result<std::collections::HashSet<String>> {
    Ok(conversation_entrypoints(conn, candidates)?
        .into_iter()
        .filter(|(_, entrypoint)| is_headless_entrypoint(entrypoint))
        .map(|(id, _)| id)
        .collect())
}

/// Persist (or overwrite) `conversation_id`'s transcript `entrypoint`. Called
/// once per import (live watcher + batch importer) right after parsing —
/// see `import::ParsedConversation::entrypoint`'s doc comment for where the
/// value comes from.
pub fn set_conversation_entrypoint(
    conn: &Connection,
    conversation_id: &str,
    entrypoint: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO conversation_entrypoint (conversation_id, entrypoint) VALUES (?1, ?2)
         ON CONFLICT(conversation_id) DO UPDATE SET entrypoint = excluded.entrypoint",
        params![conversation_id, entrypoint],
    )?;
    Ok(())
}

/// The recorded `conversation_entrypoint` value for one conversation, or
/// `None` when never recorded.
pub fn get_conversation_entrypoint(
    conn: &Connection,
    conversation_id: &str,
) -> Result<Option<String>> {
    conn.query_row(
        "SELECT entrypoint FROM conversation_entrypoint WHERE conversation_id = ?1",
        params![conversation_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Batch of `(file_path, conversation_id)` pairs from `import_state` that
/// have never had a `conversation_entrypoint` row recorded, ordered by
/// `file_path` past `after_file_path` (exclusive) — the resumable-cursor
/// shape `import::entrypoint_backfill` walks in bounded batches. A `LEFT
/// JOIN ... WHERE entrypoint IS NULL` rather than a `NOT IN` subquery so a
/// growing `conversation_entrypoint` table never turns this into a
/// correlated-subquery-per-row scan.
pub fn pending_entrypoint_backfill_batch(
    conn: &Connection,
    after_file_path: &str,
    limit: usize,
) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT i.file_path, i.conversation_id
         FROM import_state i
         LEFT JOIN conversation_entrypoint e ON e.conversation_id = i.conversation_id
         WHERE e.conversation_id IS NULL
           AND i.conversation_id IS NOT NULL
           AND i.file_path > ?1
         ORDER BY i.file_path
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![after_file_path, limit as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Drop `source = 'conversation'` rows that carry a *positive* headless
/// signal — never rows that merely lack one. Two signals, checked in order:
///
/// 1. `conversation_entrypoint` (recorded at import from the transcript's
///    own `"entrypoint"` field): `is_headless_entrypoint` on it is
///    conclusive either way — a real `cli`/`claude-desktop` entrypoint keeps
///    the row even when the conversation is missing from `session_registry`
///    entirely, and an `sdk-*` entrypoint drops it even when the row is
///    otherwise indistinguishable from a real session.
/// 2. When no entrypoint was ever recorded (pre-dates this column, or the
///    file no longer exists for a backfill to read), fall back to
///    `provenance::is_csr_emission` on the row's own preview content — CSR's
///    own formatter output/headers echoed back is excluded on that basis
///    alone, exactly as it would be for any other extraction path.
///
/// Anything left is unknown, not headless, and is kept — the previous
/// mechanism (treating absence from `session_registry`, which is built from
/// `history.jsonl` and does not record every interactive session, as proof
/// of headlessness) hid real sessions whenever the registry missed them.
/// Every other source (`codex_rollout`, `plan`, ...) has its own provenance
/// and is left untouched here.
fn filter_unregistered_conversation_rows(
    conn: &Connection,
    rows: Vec<RecentChunkRow>,
) -> Result<Vec<RecentChunkRow>> {
    const CONVERSATION_SOURCE: &str = "conversation";
    let candidates: Vec<String> = rows
        .iter()
        .filter(|row| row.source == CONVERSATION_SOURCE)
        .map(|row| row.chunk.conversation_id.clone())
        .collect();
    if candidates.is_empty() {
        return Ok(rows);
    }
    let entrypoints = conversation_entrypoints(conn, &candidates)?;
    Ok(rows
        .into_iter()
        .filter(|row| {
            if row.source != CONVERSATION_SOURCE {
                return true;
            }
            match entrypoints.get(&row.chunk.conversation_id) {
                Some(ep) => !is_headless_entrypoint(ep),
                None => !crate::extraction::provenance::is_csr_emission(&row.chunk.content),
            }
        })
        .collect())
}

/// Get recent chunks ordered by last-activity timestamp descending, scoped to
/// an already-resolved list of project namespaces (empty = unscoped/all
/// projects — same convention as `get_chunk_ids_for_projects`). Returns one
/// representative chunk per unique conversation (the first chunk, for a
/// stable preview), excludes sidechain conversations, excludes headless
/// `source='conversation'` rows that never registered as an interactive
/// session (`filter_unregistered_conversation_rows`), and collapses headless
/// codex sibling bursts into one row each.
///
/// Unscoped calls (`projects` empty) drive off `idx_chunks_timestamp` with
/// adaptive window widening; scoped calls drive off a `GROUP BY` on
/// `idx_chunks_project`. See `recent_conversation_ids_by_activity`'s doc
/// comment for why the two shapes need different plans.
pub fn get_recent_chunks_in_projects(
    conn: &Connection,
    limit: usize,
    projects: &[String],
) -> Result<Vec<ConversationChunk>> {
    // Overfetch so headless bursts can be collapsed without shrinking the
    // number of distinct slots returned; widen further below if collapsing
    // still leaves fewer than `limit` rows and more conversations exist.
    let mut target = limit.saturating_mul(4).max(limit + 8);
    const MAX_WIDENINGS: usize = 8;
    for _ in 0..MAX_WIDENINGS {
        let ids = recent_conversation_ids_by_activity(conn, projects, target)?;
        let exhausted = ids.len() < target;
        let rows = recent_chunk_rows_for(conn, &ids)?;
        let rows = filter_unregistered_conversation_rows(conn, rows)?;
        let collapsed = collapse_codex_bursts(conn, projects, rows, limit)?;
        if collapsed.len() >= limit || exhausted {
            return Ok(collapsed);
        }
        target = target.saturating_mul(2);
    }
    // Fell through the widening cap (pathological burst density) — return the
    // best effort from the last, widest fetch rather than looping forever.
    let ids = recent_conversation_ids_by_activity(conn, projects, target)?;
    let rows = recent_chunk_rows_for(conn, &ids)?;
    let rows = filter_unregistered_conversation_rows(conn, rows)?;
    collapse_codex_bursts(conn, projects, rows, limit)
}

/// Get recent chunks ordered by last-activity timestamp descending, scoped to
/// the project *family* (see `cross_project::same_project_family`) rather
/// than an exact project-name match. `project = None` or `Some("all")` is
/// unscoped. See `get_recent_chunks_in_projects` for the query shape.
pub fn get_recent_chunks(
    conn: &Connection,
    limit: usize,
    project: Option<&str>,
) -> Result<Vec<ConversationChunk>> {
    let projects = match project.filter(|p| *p != "all") {
        Some(p) => recent_work_family_projects(conn, p)?,
        None => Vec::new(),
    };
    get_recent_chunks_in_projects(conn, limit, &projects)
}

/// Get chunks within a time range, optionally filtered by project.
pub fn get_chunks_in_timerange(
    conn: &Connection,
    start: &str,
    end: &str,
    project: Option<&str>,
) -> Result<Vec<ConversationChunk>> {
    let chunks = if let Some(p) = project.filter(|p| *p != "all") {
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, project_name, timestamp, content, message_count, summary, is_sidechain
             FROM chunks WHERE timestamp BETWEEN ?1 AND ?2 AND project_name = ?3
             ORDER BY timestamp DESC",
        )?;
        let rows = stmt.query_map(params![start, end, p], row_to_chunk)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, project_name, timestamp, content, message_count, summary, is_sidechain
             FROM chunks WHERE timestamp BETWEEN ?1 AND ?2
             ORDER BY timestamp DESC",
        )?;
        let rows = stmt.query_map(params![start, end], row_to_chunk)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(chunks)
}

/// Get chunk IDs within a time range (for filtered HNSW search).
pub fn get_chunk_ids_in_timerange(
    conn: &Connection,
    start: &str,
    end: &str,
    project: Option<&str>,
) -> Result<Vec<String>> {
    let ids = if let Some(p) = project.filter(|p| *p != "all") {
        let mut stmt = conn.prepare(
            "SELECT id FROM chunks WHERE timestamp BETWEEN ?1 AND ?2 AND project_name = ?3",
        )?;
        let rows = stmt.query_map(params![start, end, p], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let mut stmt = conn.prepare("SELECT id FROM chunks WHERE timestamp BETWEEN ?1 AND ?2")?;
        let rows = stmt.query_map(params![start, end], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(ids)
}

// ─── FTS5 full-text search ───

/// Search chunks using FTS5 full-text search (for file path lookup etc.).
/// FTS5 MATCH expression for a free-text query: OR-joined quoted words, so
/// "Apify runaway cost" becomes '"apify" OR "runaway" OR "cost"' (any word
/// matches) and hyphens/special characters never parse as FTS5 operators.
/// `None` when no word survives sanitizing.
pub fn fts5_match_expr(query: &str) -> Option<String> {
    let words: Vec<String> = query
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .collect::<String>()
        })
        .filter(|w| w.len() >= 2)
        .collect();
    if words.is_empty() {
        return None;
    }
    Some(
        words
            .iter()
            .map(|w| format!("\"{}\"", w))
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

/// Keyword hits over `reflections_fts`: `(reflection_id, ordinal, bm25)` in
/// bm25 order (more negative is better), the same shape `fts5_search` returns
/// for chunks so both lists fuse in one RRF pass.
pub fn fts5_search_reflections(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<(String, usize, f64)>> {
    let Some(fts_query) = fts5_match_expr(query) else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare(
        "SELECT id, bm25(reflections_fts)
         FROM reflections_fts
         WHERE reflections_fts MATCH ?1
         ORDER BY rank
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![fts_query, limit as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
    })?;
    let hits = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(hits
        .into_iter()
        .enumerate()
        .map(|(ordinal, (id, bm25))| (id, ordinal, bm25))
        .collect())
}

/// A prefix shorter than this many characters, or with fewer than this many
/// whitespace-separated tokens, is not specific enough to promote: a common
/// single word (`we`, `Session`) matches hundreds of unrelated rows — see
/// [`fts5_reflection_prefix_matches`]'s doc.
const MIN_PROMOTABLE_PREFIX_CHARS: usize = 12;
const MIN_PROMOTABLE_PREFIX_TOKENS: usize = 2;

/// A prefix matching more rows than this is not specific enough to force to
/// the front of the page either — let bm25/RRF fusion decide instead.
const MAX_PROMOTED_PREFIX_MATCHES: usize = 3;

/// Ids of reflections whose stored content starts with `query` verbatim
/// (case-insensitive) at a word boundary, newest first. An exact-prefix hit
/// is a stronger keyword signal than an OR-of-terms bm25 match — bm25
/// compares corpus statistics across the whole `reflections` table, so a
/// project name that recurs in many rows (a common shape for a
/// `MEMORY-ARCHIVE <slug>` memory: the slug also appears in unrelated
/// stories/extractions) can bury the literal prefix match under rows that
/// merely mention one of its words. Callers merge this ahead of the
/// OR-of-terms ranking so an exact opening match always wins the keyword
/// channel, not just competes on bm25.
///
/// Guarded on both ends against over-promotion (Opus review, W3-K spec (e)):
/// a short or single-token prefix (`we`, `Session`) matches hundreds of
/// unrelated rows, and even a specific prefix that still matches more than
/// [`MAX_PROMOTED_PREFIX_MATCHES`] rows is not promoted — both return an
/// empty list rather than let `promote_exact_prefix_reflections` shove every
/// hit to the front of the page. LIKE metacharacters (`%`, `_`) in `query`
/// are escaped so they match literally, and a hit only counts if the
/// character immediately after the prefix is non-alphanumeric or the
/// content ends there (a query for "...campaign" must not match
/// "...campaigns").
pub fn fts5_reflection_prefix_matches(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<String>> {
    let trimmed = query.trim();
    if trimmed.chars().count() < MIN_PROMOTABLE_PREFIX_CHARS
        || trimmed.split_whitespace().count() < MIN_PROMOTABLE_PREFIX_TOKENS
    {
        return Ok(Vec::new());
    }
    let escaped = trimmed
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    // Fetch one row past the promotion cap so "exactly at the cap" can be
    // told apart from "over the cap" without a second COUNT query, and at
    // least `limit` rows so a legitimately small match set is never cut
    // short before the boundary filter below runs.
    let fetch_limit = (MAX_PROMOTED_PREFIX_MATCHES + 1).max(limit);
    let mut stmt = conn.prepare(
        "SELECT id, content FROM reflections WHERE content LIKE ?1 || '%' ESCAPE '\\' \
         ORDER BY timestamp DESC LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![escaped, fetch_limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let prefix_char_len = trimmed.chars().count();
    let matches: Vec<String> = rows
        .into_iter()
        .filter(|(_, content)| {
            content
                .chars()
                .nth(prefix_char_len)
                .is_none_or(|c| !c.is_alphanumeric())
        })
        .map(|(id, _)| id)
        .collect();
    if matches.len() > MAX_PROMOTED_PREFIX_MATCHES {
        return Ok(Vec::new());
    }
    Ok(matches.into_iter().take(limit).collect())
}

pub fn fts5_search(
    conn: &Connection,
    query: &str,
    limit: usize,
    project: Option<&str>,
) -> Result<Vec<(ConversationChunk, usize, f64)>> {
    let Some(fts_query) = fts5_match_expr(query) else {
        return Ok(Vec::new());
    };

    let chunks_with_bm25 = if let Some(p) = project.filter(|p| *p != "all") {
        let mut stmt = conn.prepare(
            "SELECT c.id, c.conversation_id, c.project_name, c.timestamp, c.content, c.message_count, c.summary, c.is_sidechain,
                    bm25(chunks_fts)
             FROM chunks c
             JOIN chunks_fts fts ON fts.rowid = c.rowid
             WHERE chunks_fts MATCH ?1 AND c.project_name = ?2
             ORDER BY fts.rank
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![fts_query, p, limit as i64], |row| {
            Ok((row_to_chunk(row)?, row.get::<_, f64>(8)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let mut stmt = conn.prepare(
            "SELECT c.id, c.conversation_id, c.project_name, c.timestamp, c.content, c.message_count, c.summary, c.is_sidechain,
                    bm25(chunks_fts)
             FROM chunks c
             JOIN chunks_fts fts ON fts.rowid = c.rowid
             WHERE chunks_fts MATCH ?1
             ORDER BY fts.rank
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![fts_query, limit as i64], |row| {
            Ok((row_to_chunk(row)?, row.get::<_, f64>(8)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(chunks_with_bm25
        .into_iter()
        .enumerate()
        .map(|(ordinal, (chunk, bm25))| (chunk, ordinal, bm25))
        .collect())
}

// ─── Reflection tag queries ───

/// Get reflections matching a specific tag substring (for session learnings).
pub fn get_reflections_by_tag(
    conn: &Connection,
    tag: &str,
    limit: usize,
) -> Result<Vec<ReflectionRow>> {
    // Escape LIKE wildcards to prevent injection
    let escaped_tag = tag.replace('%', "\\%").replace('_', "\\_");
    let pattern = format!("%{}%", escaped_tag);
    // The match runs on `idx_reflections_tags_timestamp` alone (a covering
    // scan of ~2 MB): `tags` sits after `content` in the row, so filtering the
    // table itself walks every reflection's overflow pages. Only the rows that
    // win are read whole. Same filter, order and limit as before.
    let mut stmt = conn.prepare(
        "SELECT id, content, tags, timestamp FROM reflections
         WHERE rowid IN (SELECT rowid FROM reflections
                         WHERE tags LIKE ?1 ESCAPE '\\'
                         ORDER BY timestamp DESC LIMIT ?2)
         ORDER BY timestamp DESC",
    )?;
    let rows = stmt.query_map(params![pattern, limit as i64], |row| {
        let id: String = row.get(0)?;
        let content: String = row.get(1)?;
        let tags_json: String = row.get(2)?;
        let timestamp: String = row.get(3)?;
        Ok((id, content, tags_json, timestamp))
    })?;

    let mut results = Vec::new();
    for row in rows {
        let (id, content, tags_json, timestamp) = row?;
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        results.push((id, content, tags, timestamp));
    }
    Ok(results)
}

/// For each conversation id, the `session_episode` rows among what
/// [`get_reflections_by_tag`]`(&format!("conv_{id}"), window)` returns, as
/// (content, timestamp) newest first — for every id in one covering scan of
/// `idx_reflections_tags_timestamp` instead of one scan per id (the prompt
/// render looks up to ~40 ids per prompt). Matching mirrors that LIKE: the
/// tags text contains `conv_<id>`, ASCII-case-insensitively; an id holding a
/// backslash (LIKE's escape character there) takes that exact SQL path.
pub fn episode_rows_by_conversation(
    conn: &Connection,
    conv_ids: &[&str],
    window: usize,
) -> Result<std::collections::HashMap<String, Vec<(String, String)>>> {
    let mut out: std::collections::HashMap<String, Vec<(String, String)>> = Default::default();
    let mut needles: Vec<(&str, String)> = Vec::new();
    for id in conv_ids {
        if id.contains('\\') {
            let rows = get_reflections_by_tag(conn, &format!("conv_{id}"), window)?;
            out.insert(
                id.to_string(),
                rows.into_iter()
                    .filter(|(_, _, tags, _)| tags.iter().any(|t| t == "session_episode"))
                    .map(|(_, content, _, ts)| (content, ts))
                    .collect(),
            );
        } else if !needles.iter().any(|(seen, _)| seen == id) {
            needles.push((id, format!("conv_{id}").to_ascii_lowercase()));
        }
    }
    if needles.is_empty() {
        return Ok(out);
    }
    // (timestamp, rowid, tags) of every match, per needle.
    let mut hits: Vec<Vec<(String, i64, String)>> = vec![Vec::new(); needles.len()];
    let mut stmt =
        conn.prepare("SELECT rowid, tags, timestamp FROM reflections WHERE tags IS NOT NULL")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let tags: String = row.get(1)?;
        let lower = tags.to_ascii_lowercase();
        let mut matched: Vec<usize> = Vec::new();
        for (at, _) in lower.match_indices("conv_") {
            for (i, (_, needle)) in needles.iter().enumerate() {
                if lower[at..].starts_with(needle.as_str()) && !matched.contains(&i) {
                    matched.push(i);
                }
            }
        }
        if matched.is_empty() {
            continue;
        }
        let rowid: i64 = row.get(0)?;
        let timestamp: String = row.get(2)?;
        for i in matched {
            hits[i].push((timestamp.clone(), rowid, tags.clone()));
        }
    }
    // Newest `window` per id, as ORDER BY timestamp DESC LIMIT window; keep the
    // episode rows and read their bodies in one pass.
    let mut wanted: Vec<(usize, i64, String)> = Vec::new();
    for (i, list) in hits.iter_mut().enumerate() {
        list.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        for (timestamp, rowid, tags) in list.iter().take(window) {
            let parsed: Vec<String> = serde_json::from_str(tags).unwrap_or_default();
            if parsed.iter().any(|t| t == "session_episode") {
                wanted.push((i, *rowid, timestamp.clone()));
            }
        }
    }
    let mut bodies: std::collections::HashMap<i64, String> = Default::default();
    if !wanted.is_empty() {
        let mut ids: Vec<i64> = wanted.iter().map(|w| w.1).collect();
        ids.sort_unstable();
        ids.dedup();
        let placeholders = vec!["?"; ids.len()].join(",");
        let mut stmt = conn.prepare(&format!(
            "SELECT rowid, content FROM reflections WHERE rowid IN ({placeholders})"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (rowid, content) = row?;
            bodies.insert(rowid, content);
        }
    }
    for (id, _) in &needles {
        out.entry(id.to_string()).or_default();
    }
    for (i, rowid, timestamp) in wanted {
        if let Some(content) = bodies.get(&rowid) {
            out.entry(needles[i].0.to_string())
                .or_default()
                .push((content.clone(), timestamp));
        }
    }
    Ok(out)
}

/// Like `get_reflections_by_tag` but requires BOTH tags to be present. Used by the
/// session-briefing hook to fetch a project's recent episodes directly
/// (`project_<name>` AND `session_episode`) so the LIMIT applies AFTER both filters
/// — a project can't be starved by its own non-episode reflections crowding a
/// pre-filter window. Caller should still exact-match the project tag (LIKE can
/// substring-match `project_foo` inside `project_foo-bar`).
pub fn get_reflections_by_two_tags(
    conn: &Connection,
    tag_a: &str,
    tag_b: &str,
    limit: usize,
) -> Result<Vec<ReflectionRow>> {
    // Escape backslash FIRST (it's the ESCAPE char), then LIKE wildcards. Wrap the
    // tag in quotes so the pattern matches a whole JSON-array element — otherwise
    // `session_episode` would substring-match a tag like `not_session_episode`.
    let esc = |t: &str| {
        t.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    };
    let pat_a = format!("%\"{}\"%", esc(tag_a));
    let pat_b = format!("%\"{}\"%", esc(tag_b));
    let mut stmt = conn.prepare(
        "SELECT id, content, tags, timestamp FROM reflections
         WHERE tags LIKE ?1 ESCAPE '\\' AND tags LIKE ?2 ESCAPE '\\'
         ORDER BY timestamp DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![pat_a, pat_b, limit as i64], |row| {
        let id: String = row.get(0)?;
        let content: String = row.get(1)?;
        let tags_json: String = row.get(2)?;
        let timestamp: String = row.get(3)?;
        Ok((id, content, tags_json, timestamp))
    })?;

    let mut results = Vec::new();
    for row in rows {
        let (id, content, tags_json, timestamp) = row?;
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        results.push((id, content, tags, timestamp));
    }
    Ok(results)
}

/// Helper: map a row to ConversationChunk.
/// Expects columns: id, conversation_id, project_name, timestamp, content,
/// message_count, summary, is_sidechain.
fn row_to_chunk(row: &rusqlite::Row) -> rusqlite::Result<ConversationChunk> {
    Ok(ConversationChunk {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        project_name: row.get(2)?,
        timestamp: row.get(3)?,
        content: row.get(4)?,
        message_count: row.get::<_, i64>(5)? as usize,
        summary: row.get(6)?,
        // Author is stored separately in chunk_provenance, not the chunks table;
        // reconstructed chunks default to non-authoritative. Live recall reads
        // provenance explicitly via get_chunk_provenance, or use
        // get_chunks_by_ids_with_provenance / row_to_chunk_with_author below.
        author: crate::provenance::Speaker::ToolResult,
        // Sequence is not needed on these read paths; sidechain identity is live
        // search metadata and must survive hydration for parent-origin dedupe.
        seq: 0,
        is_sidechain: row.get::<_, i64>(7)? != 0,
    })
}

/// Helper: map a row to `ConversationChunk` with a resolved author, for queries
/// that `LEFT JOIN chunk_provenance` (8th column: nullable `author` TEXT).
/// See `get_chunks_by_ids_with_provenance`.
fn row_to_chunk_with_author(row: &rusqlite::Row) -> rusqlite::Result<ConversationChunk> {
    let author_str: Option<String> = row.get(8)?;
    let author = author_str
        .and_then(|s| s.parse::<Speaker>().ok())
        .unwrap_or(Speaker::ToolResult);
    Ok(ConversationChunk {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        project_name: row.get(2)?,
        timestamp: row.get(3)?,
        content: row.get(4)?,
        message_count: row.get::<_, i64>(5)? as usize,
        summary: row.get(6)?,
        author,
        // Sequence is not needed on these read paths.
        seq: 0,
        is_sidechain: row.get::<_, i64>(7)? != 0,
    })
}

// ─── Enrichment state queries ───

/// Check if a conversation has been enriched with a specific type.
pub fn is_conversation_enriched(
    conn: &Connection,
    conversation_id: &str,
    enrichment_type: &str,
) -> Result<bool> {
    let mut stmt = conn.prepare(
        "SELECT 1 FROM enrichment_state
         WHERE conversation_id = ?1 AND enrichment_type = ?2 AND status = 'completed'",
    )?;
    Ok(stmt.exists(params![conversation_id, enrichment_type])?)
}

/// Mark enrichment as completed and link to the stored reflection.
pub fn mark_enrichment_completed(
    conn: &Connection,
    conversation_id: &str,
    enrichment_type: &str,
    reflection_id: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO enrichment_state (conversation_id, enrichment_type, status, reflection_id, updated_at)
         VALUES (?1, ?2, 'completed', ?3, datetime('now'))
         ON CONFLICT(conversation_id, enrichment_type) DO UPDATE SET
             status = 'completed', reflection_id = ?3, updated_at = datetime('now')",
        params![conversation_id, enrichment_type, reflection_id],
    )?;
    Ok(())
}

/// How many messages a conversation had when its story was last written. None when it has no
/// story, or one written before the count was kept.
pub fn story_messages_covered(conn: &Connection, conversation_id: &str) -> Result<Option<usize>> {
    let covered = conn
        .query_row(
            "SELECT chunk_count FROM enrichment_state
             WHERE conversation_id = ?1 AND enrichment_type = 'session_story' AND status = 'completed'",
            params![conversation_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten();
    Ok(covered.map(|n| n.max(0) as usize))
}

/// Record a story written over the conversation's first `messages` messages.
pub fn mark_story_written(
    conn: &Connection,
    conversation_id: &str,
    reflection_id: &str,
    messages: usize,
) -> Result<()> {
    conn.execute(
        "INSERT INTO enrichment_state
             (conversation_id, enrichment_type, status, reflection_id, chunk_count, updated_at)
         VALUES (?1, 'session_story', 'completed', ?2, ?3, datetime('now'))
         ON CONFLICT(conversation_id, enrichment_type) DO UPDATE SET
             status = 'completed', reflection_id = ?2, chunk_count = ?3, updated_at = datetime('now')",
        params![conversation_id, reflection_id, messages as i64],
    )?;
    Ok(())
}

/// Mark enrichment as failed with an error message.
pub fn mark_enrichment_failed(
    conn: &Connection,
    conversation_id: &str,
    enrichment_type: &str,
    error: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO enrichment_state (conversation_id, enrichment_type, status, error_message, updated_at)
         VALUES (?1, ?2, 'failed', ?3, datetime('now'))
         ON CONFLICT(conversation_id, enrichment_type) DO UPDATE SET
             status = 'failed', error_message = ?3, updated_at = datetime('now')",
        params![conversation_id, enrichment_type, error],
    )?;
    Ok(())
}

/// Mark enrichment as permanently unavailable (e.g. source JSONL deleted/rotated).
/// Unlike `mark_enrichment_failed`, status `'unavailable'` is NOT re-queued by
/// `get_unenriched_conversations`, so a missing source file stops the retry storm.
pub fn mark_enrichment_unavailable(
    conn: &Connection,
    conversation_id: &str,
    enrichment_type: &str,
    reason: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO enrichment_state (conversation_id, enrichment_type, status, error_message, updated_at)
         VALUES (?1, ?2, 'unavailable', ?3, datetime('now'))
         ON CONFLICT(conversation_id, enrichment_type) DO UPDATE SET
             status = 'unavailable', error_message = ?3, updated_at = datetime('now')",
        params![conversation_id, enrichment_type, reason],
    )?;
    Ok(())
}

/// Delete the enrichment_state row for a conversation + type, making it
/// eligible again for `get_unenriched_conversations` (status IS NULL).
/// No-op (Ok) if no matching row exists.
pub fn reset_enrichment(
    conn: &Connection,
    conversation_id: &str,
    enrichment_type: &str,
) -> Result<()> {
    conn.execute(
        "DELETE FROM enrichment_state WHERE conversation_id = ?1 AND enrichment_type = ?2",
        params![conversation_id, enrichment_type],
    )?;
    Ok(())
}

/// Get conversations that need enrichment of a given type.
/// Returns (conversation_id, file_path) pairs.
pub fn get_unenriched_conversations(
    conn: &Connection,
    enrichment_type: &str,
    limit: usize,
) -> Result<Vec<(String, String)>> {
    // Find conversations that have been imported (in import_state)
    // but don't have completed enrichment of this type.
    // Uses equality JOIN on conversation_id (not LIKE) for correctness and performance.
    // Ordered by most recent chunk timestamp so recent conversations are enriched first.
    let mut stmt = conn.prepare(
        "SELECT c.conversation_id, i.file_path
         FROM chunks c
         JOIN import_state i ON i.conversation_id = c.conversation_id
         LEFT JOIN enrichment_state e
             ON e.conversation_id = c.conversation_id AND e.enrichment_type = ?1
         WHERE e.status IS NULL OR e.status = 'failed'
         GROUP BY c.conversation_id, i.file_path
         ORDER BY MAX(c.timestamp) DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![enrichment_type, limit as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Set the Anthropic batch ID and prompt hash for a conversation's AI narrative enrichment.
pub fn set_batch_id(
    conn: &Connection,
    conversation_id: &str,
    batch_id: &str,
    prompt_hash: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO enrichment_state (conversation_id, enrichment_type, status, batch_id, prompt_hash, updated_at)
         VALUES (?1, 'ai_narrative', 'processing', ?2, ?3, datetime('now'))
         ON CONFLICT(conversation_id, enrichment_type) DO UPDATE SET
             status = 'processing', batch_id = ?2, prompt_hash = ?3, updated_at = datetime('now')",
        params![conversation_id, batch_id, prompt_hash],
    )?;
    Ok(())
}

/// Get conversations with a specific batch ID (for result retrieval).
pub fn get_conversations_by_batch(conn: &Connection, batch_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT conversation_id FROM enrichment_state
         WHERE batch_id = ?1 AND enrichment_type = 'ai_narrative'",
    )?;
    let rows = stmt.query_map(params![batch_id], |row| row.get::<_, String>(0))?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Delete a reflection by ID (for layer supersession — Layer 2 replaces Layer 1).
pub fn delete_reflection(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM reflection_embeddings WHERE reflection_id = ?1",
        params![id],
    )?;
    conn.execute("DELETE FROM reflections WHERE id = ?1", params![id])?;
    Ok(())
}

/// Replace all episode anchors for a session (delete-then-insert upsert).
pub fn replace_session_anchors(
    conn: &Connection,
    session_id: &str,
    project: &str,
    anchors: &[crate::extraction::anchors::FunctionAnchor],
) -> Result<()> {
    conn.execute(
        "DELETE FROM episode_anchors WHERE session_id = ?1",
        params![session_id],
    )?;
    let mut stmt = conn.prepare(
        "INSERT INTO episode_anchors (session_id, project, file, node_kind, name, body_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for a in anchors {
        stmt.execute(params![
            session_id,
            project,
            a.file,
            a.node_kind,
            a.name,
            a.body_hash
        ])?;
    }
    Ok(())
}

/// Most-recent-first anchors for a project: `(session_id, anchor)`.
pub fn get_project_anchors(
    conn: &Connection,
    project: &str,
    limit: i64,
) -> Result<Vec<(String, crate::extraction::anchors::FunctionAnchor)>> {
    let mut stmt = conn.prepare(
        "SELECT session_id, file, node_kind, name, body_hash
         FROM episode_anchors WHERE project = ?1
         ORDER BY created_at DESC, id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![project, limit], |r| {
        Ok((
            r.get::<_, String>(0)?,
            crate::extraction::anchors::FunctionAnchor {
                file: r.get(1)?,
                node_kind: r.get(2)?,
                name: r.get(3)?,
                body_hash: r.get(4)?,
            },
        ))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Get the reflection ID for a conversation's enrichment (for supersession).
pub fn get_enrichment_reflection_id(
    conn: &Connection,
    conversation_id: &str,
    enrichment_type: &str,
) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT reflection_id FROM enrichment_state
         WHERE conversation_id = ?1 AND enrichment_type = ?2 AND status = 'completed'",
    )?;
    let mut rows = stmt.query_map(params![conversation_id, enrichment_type], |row| {
        row.get::<_, Option<String>>(0)
    })?;
    if let Some(row) = rows.next() {
        Ok(row?)
    } else {
        Ok(None)
    }
}

// ─── Session aggregate queries (for timeline display) ───

/// Get recent sessions with aggregated message counts and enrichment data.
/// Returns one entry per conversation, ordered by last activity.
pub fn get_recent_sessions(
    conn: &Connection,
    limit: usize,
    project: Option<&str>,
) -> Result<Vec<SessionInfo>> {
    // Enrichment subquery: COALESCE across enrichment types with priority.
    // Heuristic reflection may be deleted by v3 supersession, so fall through.
    let enrichment_subquery = "COALESCE(
                    (SELECT r.content FROM enrichment_state e
                     JOIN reflections r ON r.id = e.reflection_id
                     WHERE e.conversation_id = c.conversation_id
                       AND e.enrichment_type = 'heuristic' AND e.status = 'completed'
                     LIMIT 1),
                    (SELECT r.content FROM enrichment_state e
                     JOIN reflections r ON r.id = e.reflection_id
                     WHERE e.conversation_id = c.conversation_id
                       AND e.enrichment_type = 'extracted_v3' AND e.status = 'completed'
                     LIMIT 1),
                    (SELECT r.content FROM enrichment_state e
                     JOIN reflections r ON r.id = e.reflection_id
                     WHERE e.conversation_id = c.conversation_id
                       AND e.enrichment_type = 'ai_narrative' AND e.status = 'completed'
                     LIMIT 1)
                ) as enrichment";

    let sql = if project.filter(|p| *p != "all").is_some() {
        format!(
            "SELECT c.conversation_id, c.project_name,
                MAX(c.timestamp) as last_active,
                SUM(c.message_count) as total_messages,
                COUNT(*) as chunk_count,
                (SELECT c2.summary FROM chunks c2
                 WHERE c2.conversation_id = c.conversation_id
                 ORDER BY c2.rowid ASC LIMIT 1) as summary,
                {enrichment_subquery}
         FROM chunks c
         WHERE c.project_name = ?1
         GROUP BY c.conversation_id
         ORDER BY MAX(c.timestamp) DESC LIMIT ?2"
        )
    } else {
        format!(
            "SELECT c.conversation_id, c.project_name,
                MAX(c.timestamp) as last_active,
                SUM(c.message_count) as total_messages,
                COUNT(*) as chunk_count,
                (SELECT c2.summary FROM chunks c2
                 WHERE c2.conversation_id = c.conversation_id
                 ORDER BY c2.rowid ASC LIMIT 1) as summary,
                {enrichment_subquery}
         FROM chunks c
         GROUP BY c.conversation_id
         ORDER BY MAX(c.timestamp) DESC LIMIT ?1"
        )
    };

    let mut stmt = conn.prepare(&sql)?;

    let rows = if let Some(p) = project.filter(|p| *p != "all") {
        stmt.query_map(params![p, limit as i64], row_to_session)?
    } else {
        stmt.query_map(params![limit as i64], row_to_session)?
    };

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Helper: map a row to SessionInfo.
fn row_to_session(row: &rusqlite::Row) -> rusqlite::Result<SessionInfo> {
    Ok(SessionInfo {
        conversation_id: row.get(0)?,
        project_name: row.get(1)?,
        timestamp: row.get(2)?,
        total_messages: row.get::<_, i64>(3)? as usize,
        chunk_count: row.get::<_, i64>(4)? as usize,
        summary: row.get(5)?,
        enrichment: row.get(6)?,
    })
}

/// The newest chunk of a project: its conversation and timestamp, which is the
/// `conversation_id` and `timestamp` [`get_most_recent_session`] reports (the
/// conversation whose newest chunk is newest). Served by
/// `idx_chunks_project_timestamp` instead of grouping every chunk of the
/// project with three subqueries per conversation (live 2026-10-11: half of
/// all prompt-render time, ~300 ms for 173k chunks at background priority).
/// A tie on the timestamp goes to the smallest conversation id.
pub fn latest_chunk_in_project(
    conn: &Connection,
    project: &str,
) -> Result<Option<(String, String)>> {
    let sql = if project == "all" {
        "SELECT conversation_id, timestamp FROM chunks
         ORDER BY timestamp DESC, conversation_id ASC LIMIT 1"
    } else {
        "SELECT conversation_id, timestamp FROM chunks WHERE project_name = ?1
         ORDER BY timestamp DESC, conversation_id ASC LIMIT 1"
    };
    let mut stmt = conn.prepare(sql)?;
    let row = if project == "all" {
        stmt.query_row([], |r| Ok((r.get(0)?, r.get(1)?)))
    } else {
        stmt.query_row([project], |r| Ok((r.get(0)?, r.get(1)?)))
    };
    match row {
        Ok(found) => Ok(Some(found)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Get the single most recent session for a project.
/// Used by SessionStart for "CONTINUED FROM" detection and by PromptSubmit for recency boost.
pub fn get_most_recent_session(conn: &Connection, project: &str) -> Result<Option<SessionInfo>> {
    let mut sessions = get_recent_sessions(conn, 1, Some(project))?;
    Ok(sessions.pop())
}

// ─── Import state queries ───

/// Check if a file has been imported AND hasn't changed since.
/// Compares stored file_mtime against current mtime to detect grown conversations.
pub fn is_file_imported(conn: &Connection, path: &Path) -> Result<bool> {
    let path_str = path.to_string_lossy().to_string();
    let mut stmt = conn.prepare("SELECT file_mtime FROM import_state WHERE file_path = ?1")?;

    let stored_mtime: Option<String> = stmt.query_row(params![path_str], |row| row.get(0)).ok();

    let Some(stored) = stored_mtime else {
        return Ok(false); // Never imported
    };

    // Compare against current file modification time
    let current_mtime = file_mtime_str(path);
    if stored != current_mtime {
        return Ok(false); // File has changed — needs re-import
    }

    Ok(true)
}

/// Get file modification time as a string for comparison.
fn file_mtime_str(path: &Path) -> String {
    path.metadata()
        .and_then(|m| m.modified())
        .map(|t| {
            let dt: chrono::DateTime<chrono::Utc> = t.into();
            dt.to_rfc3339()
        })
        .unwrap_or_default()
}

pub fn mark_file_imported(conn: &Connection, path: &Path, chunks: usize) -> Result<()> {
    let path_str = path.to_string_lossy().to_string();
    // Extract conversation_id from filename (stem of the JSONL file)
    let conv_id = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let mtime = file_mtime_str(path);
    conn.execute(
        "INSERT INTO import_state (
             file_path, conversation_id, chunks_imported, file_mtime,
             csr_tool_blocks_suppressed, csr_hook_wrappers_scrubbed
         )
         VALUES (?1, ?2, ?3, ?4, 0, 0)
         ON CONFLICT(file_path) DO UPDATE SET
             conversation_id = excluded.conversation_id,
             chunks_imported = excluded.chunks_imported,
             file_mtime = excluded.file_mtime",
        params![path_str, conv_id, chunks as i64, mtime],
    )?;
    Ok(())
}

/// Atomically persist import state and apply only newly observed per-file CSR
/// suppression totals. The import-state row is written before counter deltas;
/// any failure rolls both back.
pub(crate) fn mark_file_imported_with_suppression(
    conn: &mut Connection,
    path: &Path,
    chunks: usize,
    suppression: CsrSuppressionStats,
) -> Result<()> {
    let tx = conn.transaction()?;
    let path_str = path.to_string_lossy().to_string();
    let previous = tx
        .query_row(
            "SELECT csr_tool_blocks_suppressed, csr_hook_wrappers_scrubbed
             FROM import_state WHERE file_path = ?1",
            params![path_str],
            |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, Option<i64>>(1)?)),
        )
        .optional()?
        .unwrap_or((Some(0), Some(0)));
    let current_tool = suppression.csr_tool_blocks_suppressed as i64;
    let current_wrappers = suppression.csr_hook_wrappers_scrubbed as i64;
    let (persisted_tool, tool_delta) = previous.0.map_or((current_tool, 0), |previous| {
        let persisted = previous.max(current_tool);
        (persisted, persisted - previous)
    });
    let (persisted_wrappers, wrapper_delta) =
        previous.1.map_or((current_wrappers, 0), |previous| {
            let persisted = previous.max(current_wrappers);
            (persisted, persisted - previous)
        });
    let conv_id = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_default();
    let mtime = file_mtime_str(path);

    tx.execute(
        "INSERT OR REPLACE INTO import_state
         (file_path, conversation_id, chunks_imported, file_mtime,
          csr_tool_blocks_suppressed, csr_hook_wrappers_scrubbed)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            path_str,
            conv_id,
            chunks as i64,
            mtime,
            persisted_tool,
            persisted_wrappers
        ],
    )?;

    if tool_delta > 0 {
        let split_value = get_meta(&tx, "csr_tool_blocks_suppressed")?;
        let current = match split_value {
            Some(value) => Some(value),
            None => get_meta(&tx, "csr_self_suppressed")?,
        }
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
        set_meta(
            &tx,
            "csr_tool_blocks_suppressed",
            &(current + tool_delta).to_string(),
        )?;
    }
    if wrapper_delta > 0 {
        let current = get_meta(&tx, "csr_hook_wrappers_scrubbed")?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        set_meta(
            &tx,
            "csr_hook_wrappers_scrubbed",
            &(current + wrapper_delta).to_string(),
        )?;
    }

    tx.commit()?;
    Ok(())
}

/// The `import_state.file_path` rows recorded for each of `conversation_ids`,
/// for ids that have at most `max_paths_per_id` of them. An id with no row, or
/// with more rows than that, is absent from the map. The id is a transcript
/// file stem, which is not unique (`journal.jsonl`, a copied `agent-*.jsonl`):
/// one id can carry several paths and the caller has to account for all of
/// them, while a stem shared by millions of files must not cost a prompt
/// millions of rows. Each id reads at most `max_paths_per_id + 1` index
/// entries: no count, no sort, so the work is bounded by the id list alone.
pub fn import_paths_for_conversations(
    conn: &Connection,
    conversation_ids: &[String],
    max_paths_per_id: usize,
) -> Result<HashMap<String, Vec<String>>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    let mut stmt =
        conn.prepare("SELECT file_path FROM import_state WHERE conversation_id = ?1 LIMIT ?2")?;
    let mut asked: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for conversation_id in conversation_ids {
        if !asked.insert(conversation_id.as_str()) {
            continue;
        }
        let mut paths = stmt
            .query_map(
                params![conversation_id, (max_paths_per_id + 1) as i64],
                |row| row.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if paths.is_empty() || paths.len() > max_paths_per_id {
            continue;
        }
        paths.sort();
        out.insert(conversation_id.clone(), paths);
    }
    Ok(out)
}

/// Read the stored mtime for an import_state row keyed by an arbitrary `file_path`
/// string. Aux-source adapters (plans, tasks, ...) key `import_state` by a synthetic
/// id (e.g. `"plan:<slug>"`) that isn't a real filesystem path, so `is_file_imported`
/// (which stats the path itself) can't be reused — this never touches the filesystem.
pub fn get_import_state_mtime(conn: &Connection, file_path: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT file_mtime FROM import_state WHERE file_path = ?1",
        params![file_path],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Upsert an import_state row with a caller-supplied mtime, for aux-source adapters
/// whose "file_path" key is synthetic. Mirrors `mark_file_imported`'s INSERT OR
/// REPLACE shape but never derives mtime from `Path::metadata` (see
/// `get_import_state_mtime`).
pub fn upsert_import_state_explicit(
    conn: &Connection,
    file_path: &str,
    conversation_id: &str,
    chunks: usize,
    mtime: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO import_state (file_path, conversation_id, chunks_imported, file_mtime) VALUES (?1, ?2, ?3, ?4)",
        params![file_path, conversation_id, chunks as i64, mtime],
    )?;
    Ok(())
}

/// Distinct `codex_rollout` conversation ids whose stored `summary` starts
/// with `predicate` on any content it holds (checked by the caller — this
/// just hands back every distinct `(conversation_id, summary)` pair so the
/// caller can apply its own marker check without duplicating it in SQL).
/// `summary` is set once per conversation (from the first classified user
/// message) and stamped identically onto every chunk, so one row per
/// conversation is representative.
pub fn codex_conversation_summaries(conn: &Connection) -> Result<Vec<(String, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT conversation_id, summary FROM chunks WHERE source = 'codex_rollout'",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// Distinct `codex_rollout` conversation ids where *any* stored chunk's
/// `content` (not just the conversation's cached `summary`, which is only
/// ever the first chunk's content) starts with one of `markers`. A resumed
/// or multi-turn Codex session can have a real, non-harness first user
/// message but still carry a harness-injected block
/// (`<recommended_plugins>`/`<environment_context>`/etc.) re-injected on a
/// later turn — a `summary`-only scan is blind to that row entirely. One
/// full pass over the same `chunks` slice `codex_conversation_summaries`
/// already scans (`source = 'codex_rollout'`), so this costs the same order
/// of work. `markers` is caller-supplied (not hardcoded here) so this
/// storage-layer helper stays independent of `CODEX_HARNESS_MARKERS`'s
/// definition in the import layer.
pub fn codex_conversations_with_harness_content(
    conn: &Connection,
    markers: &[&str],
) -> Result<Vec<String>> {
    if markers.is_empty() {
        return Ok(Vec::new());
    }
    let clause = markers
        .iter()
        .map(|_| "content LIKE ? ESCAPE '\\'")
        .collect::<Vec<_>>()
        .join(" OR ");
    let sql = format!(
        "SELECT DISTINCT conversation_id FROM chunks
         WHERE source = 'codex_rollout' AND ({clause})"
    );
    let patterns: Vec<String> = markers
        .iter()
        .map(|marker| format!("{}%", escape_like(marker)))
        .collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(patterns.iter()), |row| {
        row.get::<_, String>(0)
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// Force the next `discover_rollouts` pass to re-import `file_path` by
/// clearing its stored mtime — an empty string can never equal a real
/// filesystem mtime, so the next scan's `stored_mtime(key) == current_mtime`
/// check always fails for this row. Leaves `conversation_id`/`chunks_imported`
/// untouched; `import_rollout`'s delete+reinsert (keyed off the parsed
/// conversation id, not this row) overwrites them on the next successful
/// import. A no-op (0 rows affected, not an error) if the row doesn't exist.
pub fn invalidate_import_state_mtime(conn: &Connection, file_path: &str) -> Result<()> {
    conn.execute(
        "UPDATE import_state SET file_mtime = '' WHERE file_path = ?1",
        params![file_path],
    )?;
    Ok(())
}

/// Mark every stored chunk of `conversation_id` as a sidechain
/// (`is_sidechain = 1`). Used by the Codex subagent-sidechain backfill (see
/// `import::codex_rollout::backfill_codex_subagent_sidechain`): a
/// `thread_spawn` (or other subagent) rollout stored before the forward-fix
/// has all its chunks at `is_sidechain = 0`, which lets it take its own
/// recent-work slot next to its parent instead of being excluded the way
/// every other sidechain conversation is. Returns the number of rows
/// actually flipped (0 for an already-correct or unknown conversation).
pub fn mark_conversation_chunks_sidechain(
    conn: &Connection,
    conversation_id: &str,
) -> Result<usize> {
    let changed = conn.execute(
        "UPDATE chunks SET is_sidechain = 1 WHERE conversation_id = ?1 AND is_sidechain = 0",
        params![conversation_id],
    )?;
    Ok(changed)
}

// ─── Session registry queries (multi-source corpus, v9.4) ───
// The registry itself is written elsewhere (lane/p4-registry); these are read-only
// lookups for correlating an aux-source document (e.g. a plan) to the session whose
// time window contains it.

/// (first_ts, last_ts) for one session, if the registry has a row for it. The registry
/// can lag import, so callers must treat "no row" as "unknown," not "no match."
pub fn get_session_registry_window(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<(Option<String>, Option<String>)>> {
    conn.query_row(
        "SELECT first_ts, last_ts FROM session_registry WHERE session_id = ?1",
        params![session_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(Into::into)
}

/// All (project, first_ts, last_ts) rows in the registry. Small table (one row per
/// session), so callers filter by time window in Rust rather than pushing per-call
/// date arithmetic into SQL.
pub fn list_session_registry_windows(conn: &Connection) -> Result<Vec<SessionWindowRow>> {
    let mut stmt = conn.prepare("SELECT project, first_ts, last_ts FROM session_registry")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

// ─── Backfill queries (for enrichment pipeline repair) ───

/// Get conversation IDs that exist in chunks but have no import_state row.
pub fn get_conversations_missing_import_state(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT conversation_id FROM chunks
         WHERE conversation_id NOT IN (
             SELECT conversation_id FROM import_state WHERE conversation_id IS NOT NULL
         )",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Get conversations that need heuristic enrichment, NOT gated by import_state.
/// Returns (conversation_id, project_name) pairs.
pub fn get_conversations_needing_heuristic(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT c.conversation_id, c.project_name FROM chunks c
         LEFT JOIN enrichment_state e
             ON e.conversation_id = c.conversation_id AND e.enrichment_type = 'heuristic'
         WHERE e.status IS NULL OR e.status = 'failed'",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Get conversations that have V3 or heuristic enrichment but no session_story.
/// Returns (conversation_id, enrichment_type, reflection_id) for each candidate.
pub fn get_conversations_missing_stories(
    conn: &Connection,
    min_messages: usize,
) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT e.conversation_id, e.enrichment_type, e.reflection_id
         FROM enrichment_state e
         WHERE e.enrichment_type IN ('extracted_v3', 'heuristic')
           AND e.status = 'completed'
           AND e.conversation_id NOT IN (
               SELECT conversation_id FROM enrichment_state
               WHERE enrichment_type = 'session_story' AND status = 'completed'
           )
           AND e.conversation_id IN (
               SELECT conversation_id FROM chunks
               GROUP BY conversation_id
               HAVING SUM(message_count) >= ?1
           )
         ORDER BY e.updated_at DESC",
    )?;
    let rows = stmt.query_map(params![min_messages as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Get the project name for a conversation from its chunks.
pub fn get_project_for_conversation(
    conn: &Connection,
    conversation_id: &str,
) -> Result<Option<String>> {
    let mut stmt =
        conn.prepare("SELECT project_name FROM chunks WHERE conversation_id = ?1 LIMIT 1")?;
    let mut rows = stmt.query_map(params![conversation_id], |row| row.get::<_, String>(0))?;
    match rows.next() {
        Some(Ok(name)) => Ok(Some(name)),
        _ => Ok(None),
    }
}

// ─── Status queries (for csr-engine status) ───

/// Count distinct conversations in the database.
/// Total messages across a conversation's imported chunks. Returns 0 when the
/// conversation has no chunks.
pub fn conversation_message_count(conn: &Connection, conversation_id: &str) -> Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COALESCE(SUM(message_count), 0) FROM chunks WHERE conversation_id = ?1",
        params![conversation_id],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

pub fn count_conversations(conn: &Connection) -> Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT conversation_id) FROM chunks",
        [],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

/// Count distinct projects in the database.
pub fn count_projects(conn: &Connection) -> Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT project_name) FROM chunks",
        [],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

/// Count imported files.
pub fn count_imported_files(conn: &Connection) -> Result<usize> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM import_state", [], |row| row.get(0))?;
    Ok(count as usize)
}

/// Get enrichment breakdown: count of conversations per enrichment type/status.
pub fn get_enrichment_breakdown(conn: &Connection) -> Result<Vec<(String, String, usize)>> {
    let mut stmt = conn.prepare(
        "SELECT enrichment_type, status, COUNT(*) FROM enrichment_state GROUP BY enrichment_type, status",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)? as usize,
        ))
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Get the newest chunk timestamp (for staleness reporting).
pub fn get_newest_chunk_timestamp(conn: &Connection) -> Result<Option<String>> {
    let result: Option<String> = conn
        .query_row("SELECT MAX(timestamp) FROM chunks", [], |row| row.get(0))
        .ok();
    Ok(result)
}

/// Get the database file size in bytes.
pub fn get_db_size(conn: &Connection) -> Result<u64> {
    let page_count: i64 = conn.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let page_size: i64 = conn.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    Ok((page_count * page_size) as u64)
}

// ─── Incremental import queries ───

/// Get the number of chunks previously imported for a file (for incremental import).
/// Returns 0 if never imported.
pub fn get_imported_chunk_count(conn: &Connection, path: &Path) -> Result<usize> {
    let path_str = path.to_string_lossy();
    let result: rusqlite::Result<i64> = conn
        .prepare("SELECT chunks_imported FROM import_state WHERE file_path = ?1")?
        .query_row(params![path_str.as_ref()], |row| row.get(0));
    match result {
        Ok(count) => Ok(count as usize),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
        Err(e) => Err(e.into()),
    }
}

/// Get chunk content by chunk ID (for post-compact context injection).
pub fn get_chunk_content(conn: &Connection, id: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT content FROM chunks WHERE id = ?1")?;
    let result: rusqlite::Result<String> = stmt.query_row(params![id], |row| row.get(0));
    match result {
        Ok(content) => Ok(Some(content)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

// ─── Count queries (for HNSW persistence staleness detection) ───

/// Fast O(1) count of chunk embeddings for HNSW cache staleness check.
pub fn count_chunk_embeddings(conn: &Connection) -> Result<usize> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM chunk_embeddings", [], |row| {
        row.get(0)
    })?;
    Ok(count as usize)
}

/// Fast O(1) count of reflection embeddings for HNSW cache staleness check.
pub fn count_reflection_embeddings(conn: &Connection) -> Result<usize> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM reflection_embeddings", [], |row| {
        row.get(0)
    })?;
    Ok(count as usize)
}

// ─── Meta KV ───

pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT value FROM meta WHERE key = ?1")?;
    let mut rows = stmt.query([key])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        [key, value],
    )?;
    Ok(())
}

// ─── Narrative usage accounting ───

pub struct NarrativeUsageRow {
    pub call_site: String, // "briefing" | "story"
    pub model: String,     // resolved model id or "unknown"
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub duration_ms: i64,
    pub success: bool,
}

#[derive(Default, serde::Serialize)]
pub struct NarrativeUsageSummary {
    pub calls_today: i64,
    pub tokens_today: i64, // input + output, today (fresh tokens only)
    pub calls_total: i64,
    pub tokens_total: i64,
    pub cache_tokens_today: i64, // cache_read + cache_creation, today
    pub cache_tokens_total: i64, // cache_read + cache_creation, all time
    pub last_model: Option<String>,
}

pub fn record_narrative_usage(conn: &Connection, row: &NarrativeUsageRow) -> Result<()> {
    record_narrative_usage_for(conn, row, None)
}

/// Record one usage row, optionally tagged with the convergence hash of the
/// work that caused it (Journal v4 P4, locked decision 13).
///
/// `ref_id` is what makes a per-dream spend figure *evidence* rather than a
/// timestamp-window guess: the producer writes the hash it is working under,
/// and `narrative_usage_for_refs` sums only rows carrying that exact hash.
/// Passing `None` (what every pre-existing call site does through
/// [`record_narrative_usage`]) leaves the row unattributed forever — which
/// is the honest outcome, since nothing recorded which dream it belonged to.
pub fn record_narrative_usage_for(
    conn: &Connection,
    row: &NarrativeUsageRow,
    ref_id: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO narrative_usage
         (call_site, model, input_tokens, output_tokens, cache_read_tokens,
          cache_creation_tokens, duration_ms, success, ref_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            row.call_site,
            row.model,
            row.input_tokens,
            row.output_tokens,
            row.cache_read_tokens,
            row.cache_creation_tokens,
            row.duration_ms,
            row.success as i64,
            ref_id,
        ],
    )?;
    Ok(())
}

/// One model's measured token totals under a set of `ref_id`s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NarrativeUsageByModel {
    pub model: String,
    pub calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
}

/// Per-model token totals for every `narrative_usage` row tagged with one of
/// `refs`. An empty `refs`, or refs that match no row, yields an empty vec —
/// the caller renders nothing rather than a zero.
///
/// The `IN` list is built from bound parameters only; `refs` values are never
/// interpolated into the SQL text.
pub fn narrative_usage_for_refs(
    conn: &Connection,
    refs: &[String],
) -> Result<Vec<NarrativeUsageByModel>> {
    if refs.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; refs.len()].join(",");
    let sql = format!(
        "SELECT model, COUNT(*),
                COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(output_tokens), 0),
                COALESCE(SUM(cache_read_tokens), 0),
                COALESCE(SUM(cache_creation_tokens), 0)
         FROM narrative_usage
         WHERE ref_id IN ({placeholders})
         GROUP BY model
         ORDER BY model"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params = rusqlite::params_from_iter(refs.iter());
    let rows = stmt
        .query_map(params, |row| {
            Ok(NarrativeUsageByModel {
                model: row.get(0)?,
                calls: row.get(1)?,
                input_tokens: row.get(2)?,
                output_tokens: row.get(3)?,
                cache_read_tokens: row.get(4)?,
                cache_creation_tokens: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Per-model token totals for every `narrative_usage` row whose `call_site`
/// is in `call_sites`. Used by `status` for dreaming's spend-to-date. An
/// empty list, or call sites with no rows, yields an empty vec — the caller
/// renders nothing rather than a zero it never measured.
///
/// The `IN` list is built from bound parameters only; values are never
/// interpolated into the SQL text.
pub fn narrative_usage_for_call_sites(
    conn: &Connection,
    call_sites: &[&str],
) -> Result<Vec<NarrativeUsageByModel>> {
    if call_sites.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; call_sites.len()].join(",");
    let sql = format!(
        "SELECT model, COUNT(*),
                COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(output_tokens), 0),
                COALESCE(SUM(cache_read_tokens), 0),
                COALESCE(SUM(cache_creation_tokens), 0)
         FROM narrative_usage
         WHERE call_site IN ({placeholders})
         GROUP BY model
         ORDER BY model"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params = rusqlite::params_from_iter(call_sites.iter());
    let rows = stmt
        .query_map(params, |row| {
            Ok(NarrativeUsageByModel {
                model: row.get(0)?,
                calls: row.get(1)?,
                input_tokens: row.get(2)?,
                output_tokens: row.get(3)?,
                cache_read_tokens: row.get(4)?,
                cache_creation_tokens: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn narrative_usage_summary(conn: &Connection) -> Result<NarrativeUsageSummary> {
    let (
        calls_total,
        tokens_total,
        cache_tokens_total,
        calls_today,
        tokens_today,
        cache_tokens_today,
    ) = conn.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(input_tokens + output_tokens), 0),
                COALESCE(SUM(cache_read_tokens + cache_creation_tokens), 0),
                COALESCE(SUM(CASE WHEN ts >= date('now') THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN ts >= date('now') THEN input_tokens + output_tokens ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN ts >= date('now') THEN cache_read_tokens + cache_creation_tokens ELSE 0 END), 0)
         FROM narrative_usage",
        [],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
            ))
        },
    )?;
    let last_model = conn
        .query_row(
            "SELECT model FROM narrative_usage ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    Ok(NarrativeUsageSummary {
        calls_today,
        tokens_today,
        calls_total,
        tokens_total,
        cache_tokens_today,
        cache_tokens_total,
        last_model,
    })
}

// ─── Ratification scores ───

pub struct RatificationScoreRow {
    pub conversation_id: String,
    pub score: f32,
    pub acts_json: String,
    pub ledger_refs: Option<String>,
    pub extractor_version: String,
}

pub fn upsert_ratification_score(conn: &Connection, row: &RatificationScoreRow) -> Result<()> {
    conn.execute(
        "INSERT INTO ratification_scores
         (conversation_id, score, acts_json, ledger_refs, extractor_version, extracted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, strftime('%s','now'))
         ON CONFLICT(conversation_id) DO UPDATE SET
             score=?2, acts_json=?3, ledger_refs=?4, extractor_version=?5,
             extracted_at=strftime('%s','now')",
        params![
            row.conversation_id,
            row.score,
            row.acts_json,
            row.ledger_refs,
            row.extractor_version,
        ],
    )?;
    Ok(())
}

pub fn get_ratification_score(conn: &Connection, conversation_id: &str) -> Result<Option<f32>> {
    conn.query_row(
        "SELECT score FROM ratification_scores WHERE conversation_id = ?1",
        params![conversation_id],
        |r| r.get::<_, f32>(0),
    )
    .optional()
    .map_err(Into::into)
}

pub fn get_ratification_scores(conn: &Connection, ids: &[String]) -> Result<HashMap<String, f32>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    const BATCH: usize = 500;
    let mut results = HashMap::new();
    for batch in ids.chunks(BATCH) {
        if batch.is_empty() {
            continue;
        }
        let placeholders = std::iter::repeat_n("?", batch.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT conversation_id, score FROM ratification_scores WHERE conversation_id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let bound: Vec<&dyn rusqlite::ToSql> =
            batch.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(bound.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f32>(1)?))
        })?;
        for row in rows {
            let (id, score) = row?;
            results.insert(id, score);
        }
    }
    Ok(results)
}

pub fn ratification_summary(conn: &Connection) -> Result<(i64, f64)> {
    conn.query_row(
        "SELECT COUNT(*), COALESCE(AVG(score), 0.0) FROM ratification_scores",
        [],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)),
    )
    .map_err(Into::into)
}

// ─── TAD: Retrieval Event Tracking ───

/// Log a retrieval event (memory was surfaced during a hook).
pub fn log_retrieval_event(
    conn: &Connection,
    memory_id: &str,
    memory_type: &str,
    hook_phase: &str,
    session_id: &str,
) -> Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let id = format!(
        "ret_{}_{}_{}",
        &session_id[..8.min(session_id.len())],
        chrono::Utc::now().timestamp_millis(),
        seq
    );
    conn.execute(
        "INSERT INTO retrieval_events (id, memory_id, memory_type, retrieved_at, hook_phase, session_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id,
            memory_id,
            memory_type,
            chrono::Utc::now().to_rfc3339(),
            hook_phase,
            session_id,
        ],
    )?;
    Ok(())
}

/// Update session outcome for all retrieval events in a session.
pub fn update_session_outcome(conn: &Connection, session_id: &str, outcome: &str) -> Result<usize> {
    let updated = conn.execute(
        "UPDATE retrieval_events SET session_outcome = ?1 WHERE session_id = ?2",
        params![outcome, session_id],
    )?;
    Ok(updated)
}

/// Get retrieval events for a specific memory (for TAD scoring).
pub fn get_retrieval_events_for_memory(
    conn: &Connection,
    memory_id: &str,
) -> Result<Vec<(String, String, String)>> {
    // Returns (retrieved_at, session_outcome, hook_phase)
    let mut stmt = conn.prepare(
        "SELECT retrieved_at, session_outcome, hook_phase
         FROM retrieval_events
         WHERE memory_id = ?1
         ORDER BY retrieved_at DESC
         LIMIT 20",
    )?;
    let rows = stmt.query_map(params![memory_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Batch-fetch typed retrieval events for TAD scoring.
pub fn get_retrieval_events_batch(
    conn: &Connection,
    memory_ids: &[&str],
) -> Result<HashMap<String, Vec<crate::search::decay::RetrievalEvent>>> {
    use crate::search::decay::{RetrievalEvent, SessionOutcome};

    if memory_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let placeholders: Vec<String> = (1..=memory_ids.len()).map(|i| format!("?{}", i)).collect();
    let sql = format!(
        "SELECT memory_id, retrieved_at, session_outcome
         FROM retrieval_events
         WHERE memory_id IN ({})
         ORDER BY retrieved_at DESC",
        placeholders.join(", ")
    );

    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::types::ToSql> = memory_ids
        .iter()
        .map(|id| id as &dyn rusqlite::types::ToSql)
        .collect();
    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    let mut map: HashMap<String, Vec<RetrievalEvent>> = HashMap::new();
    for row in rows {
        let (mid, ts_str, outcome_str) = row?;
        let retrieved_at = match ts_str.parse::<chrono::DateTime<chrono::Utc>>() {
            Ok(ts) => ts,
            Err(_) => continue,
        };
        let session_outcome = match outcome_str.as_str() {
            "success" => SessionOutcome::Success,
            "failed" => SessionOutcome::Failed,
            _ => SessionOutcome::Neutral,
        };
        map.entry(mid).or_default().push(RetrievalEvent {
            retrieved_at,
            session_outcome,
        });
    }
    Ok(map)
}

// ─── Completion queries (v8.3.0) ───

/// Escape SQLite LIKE wildcards (`%` and `_`) in a prefix string.
fn escape_like(prefix: &str) -> String {
    prefix.replace('%', "\\%").replace('_', "\\_")
}

/// List distinct project names matching a prefix (for MCP completions).
/// Returns up to `limit` results, sorted alphabetically.
pub fn list_project_names(conn: &Connection, prefix: &str, limit: usize) -> Result<Vec<String>> {
    let pattern = format!("{}%", escape_like(prefix));
    let mut stmt = conn.prepare(
        "SELECT DISTINCT project_name FROM chunks
         WHERE project_name LIKE ?1 ESCAPE '\\'
         ORDER BY project_name
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![pattern, limit as i64], |row| row.get(0))?;
    rows.collect::<std::result::Result<Vec<String>, _>>()
        .map_err(|e| anyhow::anyhow!("{}", e))
}

/// `(file_path, conversation_id)` pairs from `import_state` whose
/// `conversation_id` starts with `prefix` (e.g. `"codex:"`). Unbounded — this
/// feeds the codex-rollout chunk-timestamp restamp, a single deliberate pass
/// over the whole adapter's corpus, not a hot-path query.
pub fn import_state_rows_for_conversation_prefix(
    conn: &Connection,
    prefix: &str,
) -> Result<Vec<(String, String)>> {
    let pattern = format!("{}%", escape_like(prefix));
    let mut stmt = conn.prepare(
        "SELECT file_path, conversation_id FROM import_state
         WHERE conversation_id LIKE ?1 ESCAPE '\\'
         ORDER BY file_path",
    )?;
    let rows = stmt.query_map(params![pattern], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// List distinct file paths from import_state matching a prefix.
pub fn list_file_paths(conn: &Connection, prefix: &str, limit: usize) -> Result<Vec<String>> {
    let pattern = format!("%{}%", escape_like(prefix));
    let mut stmt = conn.prepare(
        "SELECT DISTINCT file_path FROM import_state
         WHERE file_path LIKE ?1 ESCAPE '\\'
         ORDER BY file_path
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![pattern, limit as i64], |row| {
        row.get::<_, String>(0)
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

// ─── Outcome scoring: retrieval stats rollup ───

/// Compute retrieval stats rollup from events for a specific session.
pub fn update_retrieval_stats_for_session(conn: &Connection, session_id: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO retrieval_stats (memory_id, success_count, failure_count, neutral_count, last_updated)
         SELECT memory_id,
                COALESCE(SUM(CASE WHEN session_outcome = 'success' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN session_outcome IN ('stuck', 'abandoned', 'failed') THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN session_outcome = 'neutral' THEN 1 ELSE 0 END), 0),
                datetime('now')
         FROM retrieval_events
         WHERE memory_id IN (SELECT DISTINCT memory_id FROM retrieval_events WHERE session_id = ?1)
         GROUP BY memory_id",
        params![session_id],
    )?;
    Ok(())
}

/// Batch-fetch outcome stats for scoring. Returns HashMap<memory_id, (success_count, failure_count)>.
/// Only returns entries with total events >= 3 (minimum signal gate).
pub fn get_outcome_stats_batch(
    conn: &Connection,
    memory_ids: &[&str],
) -> Result<HashMap<String, (i64, i64)>> {
    let mut map = HashMap::new();
    if memory_ids.is_empty() {
        return Ok(map);
    }
    // Build IN clause
    let placeholders: Vec<String> = memory_ids
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 1))
        .collect();
    let sql = format!(
        "SELECT memory_id, success_count, failure_count FROM retrieval_stats WHERE memory_id IN ({}) AND (success_count + failure_count) >= 3",
        placeholders.join(",")
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::types::ToSql> = memory_ids
        .iter()
        .map(|id| id as &dyn rusqlite::types::ToSql)
        .collect();
    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (id, s, f) = row?;
        map.insert(id, (s, f));
    }
    Ok(map)
}

// ─── Code evolution queries (v9) ───

/// A row from `get_recent_code_evolution`: (session_id, timestamp, functions_added, functions_removed, tool_name).
pub type CodeEvolutionRow = (String, String, String, String, String);

/// A row from `get_session_code_evolution`: (file_path, functions_added, functions_removed, types_added, types_removed, imports_added).
pub type SessionCodeEvolutionRow = (String, String, String, String, String, String);

/// Insert a code evolution record.
#[allow(clippy::too_many_arguments)]
pub fn insert_code_evolution(
    conn: &Connection,
    session_id: &str,
    project_name: &str,
    file_path: &str,
    language: &str,
    tool_name: &str,
    functions_added: &str,
    functions_removed: &str,
    types_added: &str,
    types_removed: &str,
    imports_added: &str,
    imports_removed: &str,
    repo_root: Option<&str>,
) -> Result<()> {
    let id = format!(
        "evo_{}_{}",
        &uuid::Uuid::new_v4().to_string()[..8],
        chrono::Utc::now().timestamp_millis()
    );
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO code_evolution (id, session_id, project_name, file_path, language, tool_name, functions_added, functions_removed, types_added, types_removed, imports_added, imports_removed, repo_root, timestamp)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![id, session_id, project_name, file_path, language, tool_name, functions_added, functions_removed, types_added, types_removed, imports_added, imports_removed, repo_root, now],
    )?;
    Ok(())
}

/// Insert a backfilled code-evolution row with an explicit deterministic id
/// and historical timestamp (`csr-engine backfill-coedit`). `functions_added`
/// / `functions_removed` / `types_added` / `types_removed` / `imports_added`
/// / `imports_removed` are left at their schema default (`'[]'`) — backfill
/// reconstructs co-edit *ledger* signal only, not AST diffs.
///
/// `id` is the caller's deterministic id (`bf-<sha256 prefix>`); `INSERT OR
/// IGNORE` against the `id` PRIMARY KEY makes repeated runs idempotent —
/// never touches an existing row. Returns `true` if a new row was inserted,
/// `false` if it already existed (no-op).
#[allow(clippy::too_many_arguments)]
pub fn insert_code_evolution_backfill(
    conn: &Connection,
    id: &str,
    session_id: &str,
    project_name: &str,
    file_path: &str,
    language: &str,
    tool_name: &str,
    timestamp: &str,
    repo_root: Option<&str>,
) -> Result<bool> {
    let changed = conn.execute(
        "INSERT OR IGNORE INTO code_evolution
             (id, session_id, project_name, file_path, language, tool_name, timestamp, repo_root)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            id,
            session_id,
            project_name,
            file_path,
            language,
            tool_name,
            timestamp,
            repo_root,
        ],
    )?;
    Ok(changed > 0)
}

/// Distinct `code_evolution.file_path` values still missing `repo_root` —
/// feeds the WP2 Stage 1 backfill (`import::backfill::backfill_repo_root`).
pub fn code_evolution_files_missing_repo_root(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT file_path FROM code_evolution WHERE repo_root IS NULL AND file_path != ''",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Set `repo_root` on every `code_evolution` row matching `file_path` that
/// is currently NULL. Never overwrites an already-resolved value
/// (idempotent, re-runnable). Returns the number of rows changed.
pub fn set_repo_root_for_evolution_file(
    conn: &Connection,
    file_path: &str,
    repo_root: &str,
) -> Result<usize> {
    let n = conn.execute(
        "UPDATE code_evolution SET repo_root = ?1 WHERE file_path = ?2 AND repo_root IS NULL",
        params![repo_root, file_path],
    )?;
    Ok(n)
}

/// A row from `all_code_evolution_events_ordered`:
/// (rowid, session_id, file_path, timestamp, functions_added, types_added).
pub type CodeEvolutionEventRow = (i64, String, String, String, String, String);

/// Every `code_evolution` row, oldest-first with a `rowid` tiebreak (WP2
/// Stage 2 transcript-channel attribution backfill —
/// `import::backfill::backfill_attribution`, receipt R2). Feeds the
/// earliest-by-(timestamp, rowid) join: for a symbol named in more than one
/// event, the FIRST row returned here is the one that wins. Loaded once into
/// memory — the corpus is thousands of rows, not millions, so this stays
/// cheap even at the backfill's documented "minutes-scale over ~6.7k
/// symbols" cost.
pub fn all_code_evolution_events_ordered(conn: &Connection) -> Result<Vec<CodeEvolutionEventRow>> {
    // `timestamp` holds two shapes: the schema default `datetime('now')`
    // (`YYYY-MM-DD HH:MM:SS`, legacy rows) and RFC 3339 (`YYYY-MM-DDT...`,
    // `insert_code_evolution` / backfill rows). Lexically, ' ' < 'T', so a
    // plain ORDER BY would put every legacy row of a given day before every
    // RFC 3339 row of that day regardless of the real instant — and the
    // attribution channel takes the FIRST row per symbol as the winner.
    // Normalizing the separator makes the two shapes prefix-comparable.
    let mut stmt = conn.prepare(
        "SELECT rowid, session_id, file_path, timestamp, functions_added, types_added
         FROM code_evolution
         ORDER BY replace(timestamp, 'T', ' ') ASC, rowid ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// True if a `code_evolution` row with this `id` already exists. Used by
/// `backfill-coedit --dry-run` to preview accurate would-insert counts
/// without writing anything.
pub fn code_evolution_id_exists(conn: &Connection, id: &str) -> Result<bool> {
    conn.query_row(
        "SELECT 1 FROM code_evolution WHERE id = ?1 LIMIT 1",
        params![id],
        |_| Ok(()),
    )
    .optional()
    .map(|r| r.is_some())
    .map_err(Into::into)
}

/// Get recent code evolution for a file (most recent N entries).
/// Scoped by project_name to prevent cross-project leakage (Codex M-3).
/// Returns (session_id, timestamp, functions_added, functions_removed, tool_name).
pub fn get_recent_code_evolution(
    conn: &Connection,
    file_path: &str,
    project_name: &str,
    limit: usize,
) -> Result<Vec<CodeEvolutionRow>> {
    let row_mapper = |row: &rusqlite::Row| -> rusqlite::Result<CodeEvolutionRow> {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    };

    if project_name.is_empty() {
        let mut stmt = conn.prepare(
            "SELECT session_id, timestamp, functions_added, functions_removed, tool_name
             FROM code_evolution WHERE file_path = ?1
             ORDER BY timestamp DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![file_path, limit as i64], row_mapper)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    } else {
        let mut stmt = conn.prepare(
            "SELECT session_id, timestamp, functions_added, functions_removed, tool_name
             FROM code_evolution WHERE file_path = ?1 AND project_name = ?2
             ORDER BY timestamp DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![file_path, project_name, limit as i64], row_mapper)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

/// Get code evolution summary across all files for a session.
/// Returns (file_path, functions_added, functions_removed, types_added, types_removed, imports_added).
pub fn get_session_code_evolution(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<SessionCodeEvolutionRow>> {
    let mut stmt = conn.prepare(
        "SELECT file_path, functions_added, functions_removed, types_added, types_removed, imports_added
         FROM code_evolution WHERE session_id = ?1
         ORDER BY timestamp",
    )?;
    let rows = stmt.query_map(params![session_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

// ─── Saga Phase 1 (WS1): capture columns + provenance-walk storage helpers ───

/// All chunk ids for a conversation, insertion order (no seq dependency — old rows may
/// have seq=NULL). Used by WS2's exact per-conversation scoring (never
/// SearchEngine::search_chunks_filtered — see spec).
pub fn get_chunk_ids_for_conversation(
    conn: &Connection,
    conversation_id: &str,
) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT id FROM chunks WHERE conversation_id = ?1 ORDER BY rowid")?;
    let rows = stmt.query_map(params![conversation_id], |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Distinct project namespaces participating in reinstatement. The corpus can
/// be split across chunk, co-edit, and code-graph tables, so resolving the
/// family from only one table silently loses valid aliases.
pub fn reinstatement_project_names(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT project_name FROM chunks WHERE project_name <> ''
         UNION SELECT project_name FROM code_evolution WHERE project_name <> ''
         UNION SELECT project FROM code_nodes WHERE project <> ''
         ORDER BY 1",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn string_placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(",")
}

/// Chunk ids from every namespace in an already-resolved project family.
/// An empty family means unscoped/all projects.
pub fn get_chunk_ids_for_projects(conn: &Connection, projects: &[String]) -> Result<Vec<String>> {
    if projects.is_empty() {
        let mut stmt = conn.prepare("SELECT id FROM chunks ORDER BY rowid")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        return rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
    }
    let sql = format!(
        "SELECT id FROM chunks WHERE project_name IN ({}) ORDER BY rowid",
        string_placeholders(projects.len())
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::ToSql> = projects
        .iter()
        .map(|project| project as &dyn rusqlite::ToSql)
        .collect();
    let rows = stmt.query_map(params.as_slice(), |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Conversation chunk ids constrained to an already-resolved project family.
pub fn get_chunk_ids_for_conversation_in_projects(
    conn: &Connection,
    conversation_id: &str,
    projects: &[String],
) -> Result<Vec<String>> {
    if projects.is_empty() {
        return get_chunk_ids_for_conversation(conn, conversation_id);
    }
    let sql = format!(
        "SELECT id FROM chunks WHERE conversation_id = ? AND project_name IN ({}) ORDER BY rowid",
        string_placeholders(projects.len())
    );
    let mut values = Vec::with_capacity(projects.len() + 1);
    values.push(conversation_id.to_string());
    values.extend(projects.iter().cloned());
    let params: Vec<&dyn rusqlite::ToSql> = values
        .iter()
        .map(|value| value as &dyn rusqlite::ToSql)
        .collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params.as_slice(), |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Lean exact-symbol row used by query-aware reinstatement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReinstatementSymbolNode {
    pub id: String,
    pub project: String,
    pub name: String,
    pub fqname: String,
}

/// Validate identifier candidates through exact node name/fqname matches in
/// the already-resolved project family.
pub fn exact_code_nodes_in_projects(
    conn: &Connection,
    identifiers: &[String],
    projects: &[String],
    preferred_project: &str,
    limit: usize,
) -> Result<Vec<ReinstatementSymbolNode>> {
    if identifiers.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let identifiers_sql = string_placeholders(identifiers.len());
    let project_clause = if projects.is_empty() {
        String::new()
    } else {
        format!(" AND project IN ({})", string_placeholders(projects.len()))
    };
    let sql = format!(
        "SELECT id, project, name, fqname
         FROM code_nodes
         WHERE (name IN ({identifiers_sql}) OR fqname IN ({identifiers_sql})){project_clause}
         ORDER BY CASE WHEN project = ? THEN 0 ELSE 1 END, project, name, id
         LIMIT ?"
    );
    let mut values = Vec::with_capacity(identifiers.len() * 2 + projects.len() + 1);
    values.extend(identifiers.iter().cloned());
    values.extend(identifiers.iter().cloned());
    values.extend(projects.iter().cloned());
    values.push(preferred_project.to_string());
    let mut params: Vec<&dyn rusqlite::ToSql> = values
        .iter()
        .map(|value| value as &dyn rusqlite::ToSql)
        .collect();
    let limit_value = limit as i64;
    params.push(&limit_value);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok(ReinstatementSymbolNode {
            id: row.get(0)?,
            project: row.get(1)?,
            name: row.get(2)?,
            fqname: row.get(3)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Trusted transcript-channel symbol attribution. Legacy first/last node
/// sightings are deliberately excluded: they are file-level projections, not
/// per-symbol provenance.
pub fn transcript_attribution_conversations(
    conn: &Connection,
    node_ids: &[String],
) -> Result<Vec<String>> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT source_id FROM code_node_attribution
         WHERE channel = 'transcript' AND node_id IN ({})
         ORDER BY node_id",
        string_placeholders(node_ids.len())
    );
    let params: Vec<&dyn rusqlite::ToSql> = node_ids
        .iter()
        .map(|node_id| node_id as &dyn rusqlite::ToSql)
        .collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params.as_slice(), |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Conversation/session provenance carried by edges incident to matched
/// symbols. The node ids are already family-scoped by
/// [`exact_code_nodes_in_projects`].
pub fn incident_edge_conversations(
    conn: &Connection,
    node_ids: &[String],
    limit: usize,
) -> Result<Vec<String>> {
    if node_ids.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let placeholders = string_placeholders(node_ids.len());
    let sql = format!(
        "SELECT conv_id, session_id FROM code_edges
         WHERE src_id IN ({placeholders}) OR dst_id IN ({placeholders})
         ORDER BY src_id, dst_id, kind LIMIT ?"
    );
    let mut values = Vec::with_capacity(node_ids.len() * 2);
    values.extend(node_ids.iter().cloned());
    values.extend(node_ids.iter().cloned());
    let mut params: Vec<&dyn rusqlite::ToSql> = values
        .iter()
        .map(|value| value as &dyn rusqlite::ToSql)
        .collect();
    let limit_value = limit as i64;
    params.push(&limit_value);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut conversations = Vec::new();
    for row in rows {
        let (conv_id, session_id) = row?;
        if !conv_id.is_empty() {
            conversations.push(conv_id);
        }
        if !session_id.is_empty() {
            conversations.push(session_id);
        }
    }
    Ok(conversations)
}

/// Embeddings for a specific set of chunk ids, chunked into ~500-id IN-clauses to stay
/// under SQLite's default parameter limit. Decodes the same little-endian f32 blob format
/// as `load_all_chunk_vectors`.
pub fn get_chunk_vectors_by_ids(
    conn: &Connection,
    ids: &[String],
) -> Result<Vec<(String, Vec<f32>)>> {
    const BATCH: usize = 500;
    let mut results = Vec::new();
    for batch in ids.chunks(BATCH) {
        if batch.is_empty() {
            continue;
        }
        let placeholders = std::iter::repeat_n("?", batch.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT chunk_id, embedding FROM chunk_embeddings WHERE chunk_id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let bound: Vec<&dyn rusqlite::ToSql> =
            batch.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(bound.as_slice(), |row| {
            let id: String = row.get(0)?;
            let bytes: Vec<u8> = row.get(1)?;
            Ok((id, bytes_to_vec(&bytes)))
        })?;
        for row in rows {
            results.push(row?);
        }
    }
    Ok(results)
}

/// Reflection counterpart of [`get_chunk_vectors_by_ids`] — same chunked-IN-clause
/// approach and blob decoding, scoped to `reflection_embeddings`. Lets the engine
/// reconciliation path (engine.rs) fetch only the reflection ids missing from the
/// HNSW cache instead of `load_all_reflection_vectors` (O(corpus)).
pub fn get_reflection_vectors_by_ids(
    conn: &Connection,
    ids: &[String],
) -> Result<Vec<(String, Vec<f32>)>> {
    const BATCH: usize = 500;
    let mut results = Vec::new();
    for batch in ids.chunks(BATCH) {
        if batch.is_empty() {
            continue;
        }
        let placeholders = std::iter::repeat_n("?", batch.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT reflection_id, embedding FROM reflection_embeddings WHERE reflection_id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let bound: Vec<&dyn rusqlite::ToSql> =
            batch.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(bound.as_slice(), |row| {
            let id: String = row.get(0)?;
            let bytes: Vec<u8> = row.get(1)?;
            Ok((id, bytes_to_vec(&bytes)))
        })?;
        for row in rows {
            results.push(row?);
        }
    }
    Ok(results)
}

/// Most-touched files for a session (code_evolution), highest-frequency first. Lifted from
/// the Phase 0 spike (examples/saga_spike.rs::files_for_session), now with a caller-supplied
/// limit instead of a hardcoded 4.
pub fn files_for_session(conn: &Connection, session_id: &str, limit: usize) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT file_path FROM code_evolution WHERE session_id = ?1
         GROUP BY file_path ORDER BY COUNT(*) DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![session_id, limit as i64], |row| {
        row.get::<_, String>(0)
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Family-scoped variant for reinstatement. An empty family is the explicit
/// all-project escape hatch.
pub fn files_for_session_in_projects(
    conn: &Connection,
    session_id: &str,
    projects: &[String],
    limit: usize,
) -> Result<Vec<String>> {
    if projects.is_empty() {
        return files_for_session(conn, session_id, limit);
    }
    let sql = format!(
        "SELECT file_path FROM code_evolution
         WHERE session_id = ? AND project_name IN ({})
         GROUP BY file_path ORDER BY COUNT(*) DESC LIMIT ?",
        string_placeholders(projects.len())
    );
    let mut values = Vec::with_capacity(projects.len() + 1);
    values.push(session_id.to_string());
    values.extend(projects.iter().cloned());
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<&dyn rusqlite::ToSql> = values
        .iter()
        .map(|value| value as &dyn rusqlite::ToSql)
        .collect();
    let limit_value = limit as i64;
    params.push(&limit_value);
    let rows = stmt.query_map(params.as_slice(), |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Other sessions that touched the same file (code_evolution), excluding one session.
/// Optionally scoped to a project (saga project-scoping fix — prevents cross-project
/// graph spread in the reinstatement walk).
/// Lifted from the Phase 0 spike (examples/saga_spike.rs::sessions_for_file), now with a
/// caller-supplied limit instead of a hardcoded 12.
pub fn sessions_for_file(
    conn: &Connection,
    file_path: &str,
    exclude_session: &str,
    project: Option<&str>,
    limit: usize,
) -> Result<Vec<String>> {
    match project {
        Some(p) => {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT session_id FROM code_evolution
                 WHERE file_path = ?1 AND session_id <> ?2 AND project_name = ?3 LIMIT ?4",
            )?;
            let rows = stmt.query_map(
                params![file_path, exclude_session, p, limit as i64],
                |row| row.get::<_, String>(0),
            )?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        }
        None => {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT session_id FROM code_evolution
                 WHERE file_path = ?1 AND session_id <> ?2 LIMIT ?3",
            )?;
            let rows = stmt
                .query_map(params![file_path, exclude_session, limit as i64], |row| {
                    row.get::<_, String>(0)
                })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        }
    }
}

/// Other sessions touching a file anywhere inside an already-resolved project
/// family. An empty family is unscoped.
pub fn sessions_for_file_in_projects(
    conn: &Connection,
    file_path: &str,
    exclude_session: &str,
    projects: &[String],
    limit: usize,
) -> Result<Vec<String>> {
    if projects.is_empty() {
        return sessions_for_file(conn, file_path, exclude_session, None, limit);
    }
    let sql = format!(
        "SELECT DISTINCT session_id FROM code_evolution
         WHERE file_path = ? AND session_id <> ? AND project_name IN ({}) LIMIT ?",
        string_placeholders(projects.len())
    );
    let mut values = Vec::with_capacity(projects.len() + 2);
    values.push(file_path.to_string());
    values.push(exclude_session.to_string());
    values.extend(projects.iter().cloned());
    let params_strings: Vec<&dyn rusqlite::ToSql> = values
        .iter()
        .map(|value| value as &dyn rusqlite::ToSql)
        .collect();
    let limit_value = limit as i64;
    let mut params = params_strings;
    params.push(&limit_value);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params.as_slice(), |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Backfill UPDATE: re-stamp one chunk's `timestamp` in place, gated on the
/// chunk's conversation id AND content still matching what the caller just
/// re-derived from the source file. The content guard is what makes this
/// safe against drift — a rollout that changed on disk since the original
/// import (different content at the same seq) is silently left alone rather
/// than cross-stamped. Returns whether a row was actually touched.
pub fn restamp_chunk_timestamp_if_content_matches(
    conn: &Connection,
    chunk_id: &str,
    conversation_id: &str,
    content: &str,
    new_timestamp: &str,
) -> Result<bool> {
    let updated = conn.execute(
        "UPDATE chunks SET timestamp = ?1 WHERE id = ?2 AND conversation_id = ?3 AND content = ?4",
        params![new_timestamp, chunk_id, conversation_id, content],
    )?;
    Ok(updated > 0)
}

/// Batched form of [`restamp_chunk_timestamp_if_content_matches`]: every
/// `(chunk_id, conversation_id, content, new_timestamp)` row runs in ONE
/// transaction instead of autocommitting per row. Each timestamp-only UPDATE
/// still fires `chunks_fts_au` once per row (see the migration that scopes
/// that trigger to `UPDATE OF content` — with the trigger fixed the batching
/// here is purely a transaction-count/latency win, not an FTS-bloat fix by
/// itself); batching is about not paying SQLite's fsync-per-autocommit cost
/// 280,775 times on a corpus-wide backfill. Returns
/// `(rows_matched, rows_unmatched)`. On error the whole batch rolls back —
/// callers doing a best-effort backfill should call this per-conversation
/// (a few thousand rows at most) rather than for the whole corpus in one
/// call, so one drifted file can't roll back every other conversation's
/// otherwise-clean restamp.
pub fn restamp_chunk_timestamps_batch(
    conn: &Connection,
    updates: &[(String, String, String, String)],
) -> Result<(usize, usize)> {
    if updates.is_empty() {
        return Ok((0, 0));
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<(usize, usize)> {
        let mut matched = 0usize;
        let mut unmatched = 0usize;
        {
            let mut stmt = conn.prepare(
                "UPDATE chunks SET timestamp = ?1 WHERE id = ?2 AND conversation_id = ?3 AND content = ?4",
            )?;
            for (chunk_id, conversation_id, content, new_timestamp) in updates {
                let updated =
                    stmt.execute(params![new_timestamp, chunk_id, conversation_id, content])?;
                if updated > 0 {
                    matched += 1;
                } else {
                    unmatched += 1;
                }
            }
        }
        Ok((matched, unmatched))
    })();
    match result {
        Ok(counts) => {
            conn.execute_batch("COMMIT")?;
            Ok(counts)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

/// Backfill UPDATE: set seq/is_sidechain on an existing chunk row by id.
pub fn set_chunk_saga_columns(
    conn: &Connection,
    chunk_id: &str,
    seq: usize,
    is_sidechain: bool,
) -> Result<()> {
    conn.execute(
        "UPDATE chunks SET seq = ?1, is_sidechain = ?2 WHERE id = ?3",
        params![seq as i64, is_sidechain as i64, chunk_id],
    )?;
    Ok(())
}

/// All file paths recorded in import_state (for the saga-columns backfill to re-parse).
pub fn list_all_import_state_file_paths(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT file_path FROM import_state")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// `(file_path, file_mtime)` for every `import_state` row with a recorded
/// mtime — the reconciliation set `import::catchup::catchup_sweep` diffs
/// against the live filesystem. Rows with a NULL `file_mtime` (synthetic
/// aux-source paths that predate a real filesystem file) are excluded: they
/// have no on-disk mtime to compare against and are not this sweep's concern.
pub fn list_import_state_entries(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn
        .prepare("SELECT file_path, file_mtime FROM import_state WHERE file_mtime IS NOT NULL")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Distinct sessions that touched files matching a path suffix (hook-observed edits) —
/// ground truth for the provenance eval (`eval --provenance`). Lifted from the Phase 0
/// spike's `ground_truth`. Empty target -> empty set (judged-only queries have no GT).
pub fn ground_truth_sessions_for_target(
    conn: &Connection,
    target: &str,
) -> Result<std::collections::HashSet<String>> {
    if target.is_empty() {
        return Ok(std::collections::HashSet::new());
    }
    let mut stmt = conn
        .prepare("SELECT DISTINCT session_id FROM code_evolution WHERE file_path LIKE '%' || ?1")?;
    let rows = stmt.query_map(params![target], |r| r.get::<_, String>(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

// ─── Consolidation queries (v9 Dreamer) ───

/// Get conversations with V3 extraction or AI narrative but no consolidation.
/// Returns (conversation_id, narrative_content) pairs for the Dreamer to process.
pub fn get_unconsolidated_conversations(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<(String, String)>> {
    // Find conversations that have completed V3 extraction or AI narrative
    // but have no 'consolidated_fact' enrichment row yet.
    // Prefer AI narrative (Layer 3) over V3 extraction (Layer 2) when both exist.
    let mut stmt = conn.prepare(
        "SELECT e.conversation_id, r.content
         FROM enrichment_state e
         INNER JOIN reflections r ON r.id = e.reflection_id
         WHERE e.enrichment_type IN ('extracted_v3', 'ai_narrative')
         AND e.status = 'completed'
         AND e.conversation_id NOT IN (
             SELECT conversation_id FROM enrichment_state
             WHERE enrichment_type = 'consolidated_fact' AND status IN ('completed', 'skipped')
         )
         ORDER BY e.updated_at DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Mark a conversation as consolidated (with or without facts).
pub fn mark_consolidated(
    conn: &Connection,
    conversation_id: &str,
    reflection_id: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO enrichment_state (conversation_id, enrichment_type, status, reflection_id, updated_at)
         VALUES (?1, 'consolidated_fact', 'completed', ?2, datetime('now'))
         ON CONFLICT(conversation_id, enrichment_type) DO UPDATE SET
             status = 'completed', reflection_id = ?2, updated_at = datetime('now')",
        params![conversation_id, reflection_id],
    )?;
    Ok(())
}

/// Mark a conversation as consolidated but skipped (no facts extracted).
pub fn mark_consolidated_skipped(conn: &Connection, conversation_id: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO enrichment_state (conversation_id, enrichment_type, status, updated_at)
         VALUES (?1, 'consolidated_fact', 'skipped', datetime('now'))
         ON CONFLICT(conversation_id, enrichment_type) DO UPDATE SET
             status = 'skipped', updated_at = datetime('now')",
        params![conversation_id],
    )?;
    Ok(())
}

/// Search consolidated facts by tag type, scoped by project (Codex M-4).
/// Returns (content, fact_type). Filters by project_ tag to prevent cross-project leakage.
pub fn search_consolidated_facts(
    conn: &Connection,
    project_name: &str,
    limit: usize,
) -> Result<Vec<(String, String)>> {
    let extract_fact_type = |row: &rusqlite::Row| -> rusqlite::Result<(String, String)> {
        let content: String = row.get(0)?;
        let tags_str: String = row.get(1)?;
        let fact_type = tags_str
            .split("fact_type_")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap_or("unknown")
            .to_string();
        Ok((content, fact_type))
    };

    if project_name.is_empty() {
        let mut stmt = conn.prepare(
            "SELECT content, tags FROM reflections
             WHERE tags LIKE '%consolidated_fact%'
             ORDER BY timestamp DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], extract_fact_type)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    } else {
        // Filter by project_ tag embedded in the fact's tags array
        let project_pattern = format!("%project_{}%", escape_like(project_name));
        let mut stmt = conn.prepare(
            "SELECT content, tags FROM reflections
             WHERE tags LIKE '%consolidated_fact%'
             AND tags LIKE ?1 ESCAPE '\\'
             ORDER BY timestamp DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![project_pattern, limit as i64], extract_fact_type)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

/// List distinct session IDs from iteration_learnings matching a prefix.
/// Returns empty vec if the table doesn't exist (created on first stop hook).
pub fn list_session_ids(conn: &Connection, prefix: &str, limit: usize) -> Result<Vec<String>> {
    let pattern = format!("{}%", escape_like(prefix));
    let result = conn.prepare(
        "SELECT DISTINCT session_id FROM iteration_learnings
         WHERE session_id LIKE ?1 ESCAPE '\\'
         ORDER BY session_id DESC
         LIMIT ?2",
    );
    let mut stmt = match result {
        Ok(s) => s,
        Err(e) => {
            // Only tolerate "no such table" — surface real SQL errors
            if e.to_string().contains("no such table") {
                return Ok(Vec::new());
            }
            return Err(anyhow::anyhow!("{}", e));
        }
    };
    let rows = stmt.query_map(params![pattern, limit as i64], |row| row.get(0))?;
    rows.collect::<std::result::Result<Vec<String>, _>>()
        .map_err(|e| anyhow::anyhow!("{}", e))
}

// ─── Resolution ledger ───

pub const RESOLUTION_SOURCE_AGENT: &str = "agent";
pub const RESOLUTION_SOURCE_USER_CONFIRMED: &str = "user_confirmed";

/// Latest explicit verdict for a chunk (or reflection) id.
#[derive(Debug, Clone)]
pub struct ResolutionEntry {
    pub status: String,
    pub evidence: String,
    pub claim: Option<String>,
    pub source: String,
    pub created_at: String,
}

/// Append resolution verdicts for one or more chunk ids in a single transaction.
/// Returns the number of rows inserted.
pub fn insert_resolutions(
    conn: &Connection,
    chunk_ids: &[String],
    status: &str,
    evidence: &str,
    claim: Option<&str>,
    source: &str,
) -> Result<usize> {
    if chunk_ids.is_empty() {
        return Ok(0);
    }
    if !matches!(
        source,
        RESOLUTION_SOURCE_AGENT | RESOLUTION_SOURCE_USER_CONFIRMED
    ) {
        anyhow::bail!(
            "invalid resolution source '{}': must be agent or user_confirmed",
            source
        );
    }

    let now = chrono::Utc::now().to_rfc3339();
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO resolution_ledger (chunk_id, status, evidence, claim, source, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for chunk_id in chunk_ids {
            stmt.execute(params![chunk_id, status, evidence, claim, source, now])?;
        }
    }
    tx.commit()?;
    Ok(chunk_ids.len())
}

/// Caller owns the transaction containing the immutable local confirmation,
/// ledger payload, derivation edges and (for the journal) UI audit row.
pub(crate) fn append_resolutions_with_confirmation(
    conn: &Connection,
    chunk_ids: &[String],
    status: &str,
    evidence: &str,
    claim: Option<&str>,
    confirmation: Option<&crate::provenance::ResolutionConfirmation>,
) -> Result<(usize, &'static str)> {
    use crate::storage::artifact_provenance::{
        record_inputs, ArtifactInput, ArtifactKind, InputEnvelope,
    };
    let payload =
        crate::provenance::ResolutionConfirmationPayload::new(chunk_ids, status, claim, evidence);
    let confirmation = confirmation.filter(|c| c.matches(&payload));
    let source = if confirmation.is_some() {
        RESOLUTION_SOURCE_USER_CONFIRMED
    } else {
        RESOLUTION_SOURCE_AGENT
    };
    let now = chrono::Utc::now().to_rfc3339();
    let event = confirmation
        .filter(|_| !chunk_ids.is_empty())
        .map(|confirmation| {
            let id = format!("confirmation:{}", uuid::Uuid::new_v4());
            ProvenanceEvent {
                event_id: id.clone(),
                conversation_id: id,
                message_key: payload.digest().into(),
                seq: 0,
                channel: "user_confirmation".into(),
                trust_tier: TrustTier::UserConfirmed,
                parent_event_id: None,
                receipt_kind: confirmation.receipt_kind().into(),
                receipt_ref: Some(payload.digest().into()),
                observed_at: now.clone(),
            }
        });
    if let Some(event) = &event {
        insert_provenance_event(conn, event)?;
    }
    for chunk_id in chunk_ids {
        conn.execute("INSERT INTO resolution_ledger(chunk_id,status,evidence,claim,source,created_at,event_id) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![chunk_id,status,evidence,claim,source,now,event.as_ref().map(|e|&e.event_id)])?;
        let id = conn.last_insert_rowid().to_string();
        if let Some(event) = &event {
            let input = ArtifactInput::observed(
                event.clone(),
                payload.canonical_json(),
                0,
                payload.canonical_json().chars().count(),
                crate::provenance::content_hash(payload.canonical_json()),
            );
            let body = serde_json::json!([claim, evidence]).to_string();
            record_inputs(
                conn,
                ArtifactKind::Resolution,
                &id,
                &body,
                &InputEnvelope::new(vec![input]),
            )?;
        }
    }
    Ok((chunk_ids.len(), source))
}

/// Batch-fetch the latest user-confirmed resolution entry per chunk id.
/// Agent observations remain in the append-only audit ledger but are not
/// authoritative search state and are absent from the returned map.
pub fn get_resolutions_batch(
    conn: &Connection,
    chunk_ids: &[String],
) -> Result<HashMap<String, ResolutionEntry>> {
    if chunk_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let placeholders: Vec<String> = (1..=chunk_ids.len()).map(|i| format!("?{}", i)).collect();
    let sql = format!(
        "SELECT chunk_id, status, evidence, claim, source, created_at
         FROM resolution_ledger
         WHERE source = 'user_confirmed'
           AND id IN (
             SELECT MAX(id) FROM resolution_ledger
             WHERE chunk_id IN ({}) AND source = 'user_confirmed'
             GROUP BY chunk_id
         )",
        placeholders.join(", ")
    );

    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::types::ToSql> = chunk_ids
        .iter()
        .map(|id| id as &dyn rusqlite::types::ToSql)
        .collect();
    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok((
            row.get::<_, String>(0)?,
            ResolutionEntry {
                status: row.get(1)?,
                evidence: row.get(2)?,
                claim: row.get(3)?,
                source: row.get(4)?,
                created_at: row.get(5)?,
            },
        ))
    })?;

    let mut map = HashMap::new();
    for row in rows {
        let (chunk_id, entry) = row?;
        map.insert(chunk_id, entry);
    }
    Ok(map)
}

// ─── Session registry (history.jsonl spine — never embedded / never injected) ───

/// One row per session to upsert: (session_id, project, first_prompt,
/// first_ts_rfc3339, last_ts_rfc3339, prompt_count_delta).
#[derive(Debug, Clone)]
pub struct SessionRegistryRow {
    pub session_id: String,
    pub project: String,
    pub first_prompt: Option<String>,
    pub first_ts: String,
    pub last_ts: String,
    pub prompt_count_delta: i64,
}

/// Upsert session_registry rows within an existing transaction. On conflict:
/// first_prompt/first_ts kept from whichever row has the EARLIER first_ts
/// (existing row wins unless the existing row's first_ts is null/absent or
/// later than the new row's), last_ts = MAX(existing, new), prompt_count =
/// existing + delta.
///
/// Does NOT open a new transaction — caller already holds one (see
/// `Storage::with_transaction` for atomic upsert+checkpoint).
pub fn upsert_session_registry_batch(
    conn: &Connection,
    rows: &[SessionRegistryRow],
) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    let mut stmt = conn.prepare(
        "INSERT INTO session_registry (session_id, project, first_prompt, first_ts, last_ts, prompt_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(session_id) DO UPDATE SET
           first_prompt = CASE
             WHEN excluded.first_ts < session_registry.first_ts
                  OR session_registry.first_ts IS NULL
             THEN excluded.first_prompt
             ELSE session_registry.first_prompt
           END,
           first_ts = CASE
             WHEN excluded.first_ts < session_registry.first_ts
                  OR session_registry.first_ts IS NULL
             THEN excluded.first_ts
             ELSE session_registry.first_ts
           END,
           last_ts = CASE
             WHEN session_registry.last_ts IS NULL
                  OR excluded.last_ts > session_registry.last_ts
             THEN excluded.last_ts
             ELSE session_registry.last_ts
           END,
           prompt_count = session_registry.prompt_count + excluded.prompt_count",
    )?;
    for row in rows {
        stmt.execute(params![
            row.session_id,
            row.project,
            row.first_prompt,
            row.first_ts,
            row.last_ts,
            row.prompt_count_delta,
        ])?;
    }
    Ok(rows.len())
}

/// (sessions_seen, sessions_imported, gap).
/// sessions_seen = COUNT(*) FROM session_registry.
/// sessions_imported = registry sessions that appear in non-plan chunks.
/// gap = sessions_seen - sessions_imported (never negative).
pub fn coverage_stats(conn: &Connection) -> Result<(i64, i64, i64)> {
    let (seen, imported): (i64, i64) = conn.query_row(
        "SELECT
           (SELECT COUNT(*) FROM session_registry) AS seen,
           (SELECT COUNT(*) FROM session_registry sr
              WHERE EXISTS (
                SELECT 1 FROM chunks c
                WHERE c.conversation_id = sr.session_id
                  AND c.conversation_id NOT LIKE 'plan:%'
              )
           ) AS imported",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let gap = (seen - imported).max(0);
    Ok((seen, imported, gap))
}

/// Subset of `candidates` present in either session_registry or chunks.conversation_id.
pub fn known_session_ids(
    conn: &Connection,
    candidates: &[String],
) -> Result<std::collections::HashSet<String>> {
    use std::collections::HashSet;
    if candidates.is_empty() {
        return Ok(HashSet::new());
    }

    // Batched ≤500 candidates per statement: one flat ?1..?N list would hit
    // SQLITE_MAX_VARIABLE_NUMBER on large transcript dirs, and the caller's
    // unwrap_or_default would silently report every session as unknown
    // (CodeRabbit). Numbered placeholders are reused on both UNION sides —
    // each candidate binds once.
    const BATCH: usize = 500;
    let mut out = HashSet::new();
    for batch in candidates.chunks(BATCH) {
        let placeholders: Vec<String> = (1..=batch.len()).map(|i| format!("?{i}")).collect();
        let in_list = placeholders.join(", ");
        let sql = format!(
            "SELECT session_id FROM session_registry WHERE session_id IN ({in_list})
             UNION
             SELECT conversation_id FROM chunks WHERE conversation_id IN ({in_list})"
        );

        let params: Vec<&dyn rusqlite::types::ToSql> = batch
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params.as_slice(), |row| row.get::<_, String>(0))?;
        for row in rows {
            out.insert(row?);
        }
    }
    Ok(out)
}

// ─── Memory registry (harness file-based memory — never embedded / never injected) ───

/// One memory_registry row to upsert. `file_path` is the stable primary key
/// (absolute path to the .md file). All other fields are simply overwritten
/// on conflict — unlike session_registry there is no earliest/latest merge
/// logic, because a given file_path only ever describes one file: the most
/// recent scan's read of it is authoritative.
#[derive(Debug, Clone)]
pub struct MemoryRegistryRow {
    pub file_path: String,
    pub project: String,
    pub slug: String,
    pub description: Option<String>,
    pub mem_type: Option<String>,
    pub origin_session_id: Option<String>,
    pub modified_ts: Option<String>,
    pub file_mtime: i64,
    pub content_hash: String,
    pub links_json: String,
    pub last_seen_scan: i64,
}

/// Upsert memory_registry rows within an existing transaction. On conflict
/// every field is overwritten from the incoming row — the latest scan is
/// authoritative for a given file_path.
///
/// Does NOT open a new transaction — caller already holds one (see
/// `Storage::with_transaction` for atomic upsert+stale-delete).
pub fn upsert_memory_registry_batch(
    conn: &Connection,
    rows: &[MemoryRegistryRow],
) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    let mut stmt = conn.prepare(
        "INSERT INTO memory_registry (
            file_path, project, slug, description, mem_type,
            origin_session_id, modified_ts, file_mtime, content_hash,
            links_json, last_seen_scan
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(file_path) DO UPDATE SET
           project = excluded.project,
           slug = excluded.slug,
           description = excluded.description,
           mem_type = excluded.mem_type,
           origin_session_id = excluded.origin_session_id,
           modified_ts = excluded.modified_ts,
           file_mtime = excluded.file_mtime,
           content_hash = excluded.content_hash,
           links_json = excluded.links_json,
           last_seen_scan = excluded.last_seen_scan",
    )?;
    for row in rows {
        stmt.execute(params![
            row.file_path,
            row.project,
            row.slug,
            row.description,
            row.mem_type,
            row.origin_session_id,
            row.modified_ts,
            row.file_mtime,
            row.content_hash,
            row.links_json,
            row.last_seen_scan,
        ])?;
    }
    Ok(rows.len())
}

/// Delete memory_registry rows for `project` whose `last_seen_scan` is older
/// than `current_scan_generation`. Scoped to ONE project per call by design —
/// a project whose directory was unreadable during a scan pass must simply
/// never have this function called for it that pass, so its rows are never
/// touched (unreadable != deleted). This is how "never delete for unscanned
/// projects" is enforced at the call-site level, not inside this function.
pub fn delete_memory_registry_stale(
    conn: &Connection,
    project: &str,
    current_scan_generation: i64,
) -> Result<usize> {
    let deleted = conn.execute(
        "DELETE FROM memory_registry WHERE project = ?1 AND last_seen_scan < ?2",
        params![project, current_scan_generation],
    )?;
    Ok(deleted)
}

/// All memory_registry rows whose `origin_session_id` matches `session_id`.
/// Lookup `csr_why` will use in a later stage.
pub fn get_memory_registry_by_origin_session(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<MemoryRegistryRow>> {
    let mut stmt = conn.prepare(
        "SELECT file_path, project, slug, description, mem_type,
                origin_session_id, modified_ts, file_mtime, content_hash,
                links_json, last_seen_scan
         FROM memory_registry WHERE origin_session_id = ?1",
    )?;
    let rows = stmt.query_map(params![session_id], |row| {
        Ok(MemoryRegistryRow {
            file_path: row.get(0)?,
            project: row.get(1)?,
            slug: row.get(2)?,
            description: row.get(3)?,
            mem_type: row.get(4)?,
            origin_session_id: row.get(5)?,
            modified_ts: row.get(6)?,
            file_mtime: row.get(7)?,
            content_hash: row.get(8)?,
            links_json: row.get(9)?,
            last_seen_scan: row.get(10)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::migrations;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        conn
    }

    fn test_chunk(id: &str, content: &str) -> ConversationChunk {
        ConversationChunk {
            id: id.into(),
            conversation_id: "conv-fts".into(),
            project_name: "project-fts".into(),
            timestamp: "2026-08-12T00:00:00Z".into(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: Speaker::ToolResult,
            seq: 0,
            is_sidechain: false,
        }
    }

    #[test]
    fn fts5_search_reflections_returns_bm25_order_with_zero_based_ordinals() {
        let conn = mem();
        insert_reflection(&conn, "dense", "needle needle needle", &[], &[0.1; 4]).unwrap();
        insert_reflection(
            &conn,
            "sparse",
            "needle with several unrelated filler words",
            &[],
            &[0.2; 4],
        )
        .unwrap();
        insert_reflection(&conn, "miss", "nothing relevant here", &[], &[0.3; 4]).unwrap();

        let hits = fts5_search_reflections(&conn, "needle", 10).unwrap();

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].0, "dense");
        assert_eq!(hits[0].1, 0);
        assert_eq!(hits[1].0, "sparse");
        assert_eq!(hits[1].1, 1);
        assert!(
            hits[0].2 <= hits[1].2,
            "bm25 must be ascending (better first)"
        );
        assert!(fts5_search_reflections(&conn, "!!", 10).unwrap().is_empty());
    }

    /// Opus review finding (high, W3-K spec (e)): a short or single-token
    /// prefix matches hundreds of unrelated rows and must not be promoted —
    /// live receipts: `Session` alone matched 129 reflections, `we` matched
    /// 5 unrelated stories opening with "We ..."/"we ...". Below the
    /// specificity floor (min tokens, min chars) the matcher must return
    /// nothing rather than let `promote_exact_prefix_reflections` shove
    /// every hit to the front of the page.
    #[test]
    fn short_or_single_token_prefix_is_not_promoted() {
        let conn = mem();
        for i in 0..5 {
            insert_reflection(
                &conn,
                &format!("session-{i}"),
                &format!("Session note number {i} about something"),
                &[],
                &[0.1; 4],
            )
            .unwrap();
        }
        assert!(
            fts5_reflection_prefix_matches(&conn, "Session", 10)
                .unwrap()
                .is_empty(),
            "a single-token 7-char prefix must not promote 5 unrelated rows"
        );
        for (i, opener) in ["We integrated manychat", "we are seeing wild swings"]
            .iter()
            .enumerate()
        {
            insert_reflection(
                &conn,
                &format!("we-{i}"),
                &format!("{opener} across the campaign"),
                &[],
                &[0.1; 4],
            )
            .unwrap();
        }
        assert!(
            fts5_reflection_prefix_matches(&conn, "we", 10)
                .unwrap()
                .is_empty(),
            "a 2-char prefix must not promote"
        );
    }

    /// Same finding: a prefix that clears the specificity floor but still
    /// matches more than the small promotion cap must not be promoted either
    /// — let bm25/RRF fusion decide instead of forcing every match to the
    /// front of the page.
    #[test]
    fn specific_prefix_over_the_promotion_cap_is_not_promoted() {
        let conn = mem();
        for i in 0..6 {
            insert_reflection(
                &conn,
                &format!("archive-{i}"),
                &format!("MEMORY-ARCHIVE common project note number {i}"),
                &[],
                &[0.1; 4],
            )
            .unwrap();
        }
        assert!(
            fts5_reflection_prefix_matches(&conn, "MEMORY-ARCHIVE common project", 10)
                .unwrap()
                .is_empty(),
            "6 matches is over the small promotion cap"
        );
    }

    /// The positive case this guard must not break: a specific, multi-token,
    /// long-enough prefix with a small number of matches is still promoted,
    /// newest first — the exact shape
    /// `exact_prefix_reflection_reaches_page_one_over_many_keyword_matching_chunks`
    /// (mcp::tools) depends on.
    #[test]
    fn specific_prefix_within_the_cap_is_promoted_newest_first() {
        let conn = mem();
        insert_reflection(
            &conn,
            "older",
            "MEMORY-ARCHIVE project-x-y-z memory number 0",
            &[],
            &[0.1; 4],
        )
        .unwrap();
        insert_reflection(
            &conn,
            "newer",
            "MEMORY-ARCHIVE project-x-y-z memory number 1",
            &[],
            &[0.1; 4],
        )
        .unwrap();
        conn.execute(
            "UPDATE reflections SET timestamp = '2020-01-01T00:00:00Z' WHERE id = 'older'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE reflections SET timestamp = '2026-01-01T00:00:00Z' WHERE id = 'newer'",
            [],
        )
        .unwrap();
        let matches =
            fts5_reflection_prefix_matches(&conn, "MEMORY-ARCHIVE project-x-y-z", 10).unwrap();
        assert_eq!(matches, vec!["newer".to_string(), "older".to_string()]);
    }

    /// A prefix containing LIKE metacharacters must not act as a wildcard —
    /// `%` alone must never match every reflection.
    #[test]
    fn like_metacharacters_in_the_prefix_are_escaped() {
        let conn = mem();
        insert_reflection(
            &conn,
            "literal-percent",
            "% is a literal opener here for this specific memory note",
            &[],
            &[0.1; 4],
        )
        .unwrap();
        insert_reflection(
            &conn,
            "unrelated",
            "this row has nothing to do with the prefix at all",
            &[],
            &[0.1; 4],
        )
        .unwrap();
        // Below specificity floor by itself, so wrap it to reach the char
        // minimum while still testing that '%' is escaped rather than
        // acting as a wildcard: a literal-prefix query starting with '%'
        // must not match the unrelated row.
        let matches = fts5_reflection_prefix_matches(&conn, "% is a literal opener", 10).unwrap();
        assert_eq!(matches, vec!["literal-percent".to_string()]);
    }

    /// Word-boundary guard: a prefix must not match mid-token. The query is
    /// a strict string-prefix of `mid-token`'s content ("campaign" is a
    /// prefix of "campaigns") but stops inside the word, so it must not
    /// count as a match; `exact` ends exactly at a word boundary (a space)
    /// and must.
    #[test]
    fn prefix_match_requires_a_word_boundary() {
        let conn = mem();
        insert_reflection(
            &conn,
            "exact",
            "MEMORY-ARCHIVE anukriti-meta-campaign notes here",
            &[],
            &[0.1; 4],
        )
        .unwrap();
        insert_reflection(
            &conn,
            "mid-token",
            "MEMORY-ARCHIVE anukriti-meta-campaigns notes here",
            &[],
            &[0.1; 4],
        )
        .unwrap();
        let matches =
            fts5_reflection_prefix_matches(&conn, "MEMORY-ARCHIVE anukriti-meta-campaign", 10)
                .unwrap();
        assert_eq!(matches, vec!["exact".to_string()]);
    }

    #[test]
    fn reflections_fts_follows_replace_and_delete() {
        // `INSERT OR REPLACE` never fires the delete trigger for the row it
        // replaces, so the keyword index must be keyed by id, not rowid.
        let conn = mem();
        insert_reflection(&conn, "r1", "alpha beta", &[], &[0.1; 4]).unwrap();
        assert_eq!(
            fts5_search_reflections(&conn, "alpha", 10).unwrap().len(),
            1
        );

        insert_reflection(&conn, "r1", "gamma delta", &[], &[0.1; 4]).unwrap();
        assert!(fts5_search_reflections(&conn, "alpha", 10)
            .unwrap()
            .is_empty());
        let after = fts5_search_reflections(&conn, "gamma", 10).unwrap();
        assert_eq!(
            after.len(),
            1,
            "replacement must not leave a duplicate keyword row"
        );

        conn.execute(
            "DELETE FROM reflection_embeddings WHERE reflection_id = 'r1'",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM reflections WHERE id = 'r1'", [])
            .unwrap();
        assert!(fts5_search_reflections(&conn, "gamma", 10)
            .unwrap()
            .is_empty());
        let triggers: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name LIKE 'reflections_fts_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(triggers, 3);
    }

    #[test]
    fn fts5_search_returns_bm25_order_with_zero_based_ordinals() {
        let conn = mem();
        insert_chunk(
            &conn,
            &test_chunk("dense", "needle needle needle"),
            &[0.1; 4],
        )
        .unwrap();
        insert_chunk(
            &conn,
            &test_chunk("sparse", "needle with several unrelated filler words"),
            &[0.2; 4],
        )
        .unwrap();

        let hits = fts5_search(&conn, "needle", 10, None).unwrap();

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].0.id, "dense");
        assert_eq!(hits[0].1, 0);
        assert!(hits[0].2.is_finite());
        assert_eq!(hits[1].0.id, "sparse");
        assert_eq!(hits[1].1, 1);
        assert!(hits[1].2.is_finite());
        assert!(
            hits[0].2 <= hits[1].2,
            "FTS5 bm25 order must be ascending: {hits:?}"
        );
    }

    #[test]
    fn chunk_reimport_preserves_rowids_and_replaces_one_fts_document() {
        // Break caught: using INSERT OR REPLACE for either TEXT-keyed table
        // deletes and reinserts its row, changing the implicit rowid. For
        // chunks that also leaves the old rowid indexed in FTS.
        let conn = mem();
        insert_chunk(&conn, &test_chunk("stable", "legacytoken"), &[0.1; 4]).unwrap();
        let chunk_rowid_before: i64 = conn
            .query_row("SELECT rowid FROM chunks WHERE id = 'stable'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let embedding_rowid_before: i64 = conn
            .query_row(
                "SELECT rowid FROM chunk_embeddings WHERE chunk_id = 'stable'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        insert_chunk(&conn, &test_chunk("stable", "replacementtoken"), &[0.2; 4]).unwrap();

        let chunk_rowid_after: i64 = conn
            .query_row("SELECT rowid FROM chunks WHERE id = 'stable'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let embedding_rowid_after: i64 = conn
            .query_row(
                "SELECT rowid FROM chunk_embeddings WHERE chunk_id = 'stable'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(chunk_rowid_after, chunk_rowid_before);
        assert_eq!(embedding_rowid_after, embedding_rowid_before);
        assert!(fts5_search(&conn, "legacytoken", 10, None)
            .unwrap()
            .is_empty());
        assert_eq!(
            fts5_search(&conn, "replacementtoken", 10, None).unwrap()[0]
                .0
                .id,
            "stable"
        );
        let indexed_documents: i64 = conn
            .query_row("SELECT COUNT(*) FROM chunks_fts_docsize", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            indexed_documents, 1,
            "reimport must not append an orphan FTS row"
        );

        delete_chunks_for_conversation(&conn, "conv-fts").unwrap();
        assert!(fts5_search(&conn, "replacementtoken", 10, None)
            .unwrap()
            .is_empty());
        let indexed_documents: i64 = conn
            .query_row("SELECT COUNT(*) FROM chunks_fts_docsize", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            indexed_documents, 0,
            "deleting the chunk must delete its FTS row"
        );
    }

    #[test]
    fn external_fts_tracks_direct_chunk_updates_and_deletes() {
        // Break caught: external-content FTS has no automatic synchronization.
        // A writer that bypasses insert_chunk must still be covered.
        let conn = mem();
        conn.execute(
            "INSERT INTO chunks
                 (id, conversation_id, project_name, timestamp, content, message_count)
             VALUES ('direct', 'conv-direct', 'project-fts',
                     '2026-08-12T00:00:00Z', 'beforetoken', 1)",
            [],
        )
        .unwrap();
        assert_eq!(
            fts5_search(&conn, "beforetoken", 10, None).unwrap()[0].0.id,
            "direct"
        );

        conn.execute(
            "UPDATE chunks SET content = 'aftertoken' WHERE id = 'direct'",
            [],
        )
        .unwrap();
        assert!(fts5_search(&conn, "beforetoken", 10, None)
            .unwrap()
            .is_empty());
        assert_eq!(
            fts5_search(&conn, "aftertoken", 10, None).unwrap()[0].0.id,
            "direct"
        );

        conn.execute("DELETE FROM chunks WHERE id = 'direct'", [])
            .unwrap();
        assert!(fts5_search(&conn, "aftertoken", 10, None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_narrative_usage_record_and_summary() {
        let conn = mem();
        let row = NarrativeUsageRow {
            call_site: "briefing".into(),
            model: "claude-haiku-4-5".into(),
            input_tokens: 1500,
            output_tokens: 300,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            duration_ms: 4200,
            success: true,
        };
        record_narrative_usage(&conn, &row).unwrap();
        record_narrative_usage(&conn, &row).unwrap();

        let s = narrative_usage_summary(&conn).unwrap();
        assert_eq!(s.calls_total, 2);
        assert_eq!(s.tokens_total, 3600);
        assert_eq!(s.calls_today, 2); // rows stamped 'now' are today
        assert_eq!(s.tokens_today, 3600);
        assert_eq!(s.cache_tokens_total, 0);
        assert_eq!(s.cache_tokens_today, 0);
        assert_eq!(s.last_model.as_deref(), Some("claude-haiku-4-5"));
    }

    #[test]
    fn test_narrative_usage_summary_empty() {
        let conn = mem();
        let s = narrative_usage_summary(&conn).unwrap();
        assert_eq!(s.calls_total, 0);
        assert_eq!(s.cache_tokens_total, 0);
        assert_eq!(s.cache_tokens_today, 0);
        assert_eq!(s.last_model, None);
    }

    #[test]
    fn test_get_ratification_scores_empty_input() {
        let conn = mem();
        let map = get_ratification_scores(&conn, &[]).unwrap();
        assert!(map.is_empty());
    }

    /// Regression: `get_chunks_by_ids` alone always defaults `author` to
    /// `ToolResult` (chunks table has no author column), which silently starved
    /// ratification's `build_digest` of operator turns in production even
    /// though import correctly wrote `Speaker::User` rows into
    /// `chunk_provenance`. `get_chunks_by_ids_with_provenance` must recover the
    /// real author via the LEFT JOIN.
    #[test]
    fn get_chunks_by_ids_with_provenance_resolves_true_author() {
        let conn = mem();
        let chunk = ConversationChunk {
            id: "c1".into(),
            conversation_id: "conv-1".into(),
            project_name: "p".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            content: "fix the import bug".into(),
            message_count: 1,
            summary: None,
            author: Speaker::ToolResult, // irrelevant: insert_chunk doesn't persist author
            seq: 0,
            is_sidechain: false,
        };
        insert_chunk(&conn, &chunk, &[0.0; 4]).unwrap();
        insert_chunk_provenance(
            &conn,
            "c1",
            &ChunkProvenance {
                author: Speaker::User,
                source_conv_id: "conv-1".into(),
                supersedes: None,
            },
        )
        .unwrap();

        // Plain path (no join) always degrades to ToolResult.
        let plain = get_chunks_by_ids(&conn, &["c1".into()]).unwrap();
        assert_eq!(plain[0].author, Speaker::ToolResult);

        // Provenance-aware path recovers the true author.
        let with_prov = get_chunks_by_ids_with_provenance(&conn, &["c1".into()]).unwrap();
        assert_eq!(with_prov.len(), 1);
        assert_eq!(with_prov[0].author, Speaker::User);
        assert_eq!(with_prov[0].content, "fix the import bug");
    }

    /// A chunk with no `chunk_provenance` row at all must still degrade to
    /// `ToolResult` (not error), matching `get_chunk_provenance`'s fallback.
    #[test]
    fn get_chunks_by_ids_with_provenance_defaults_when_no_row() {
        let conn = mem();
        let chunk = ConversationChunk {
            id: "c2".into(),
            conversation_id: "conv-2".into(),
            project_name: "p".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            content: "no provenance row".into(),
            message_count: 1,
            summary: None,
            author: Speaker::ToolResult,
            seq: 0,
            is_sidechain: false,
        };
        insert_chunk(&conn, &chunk, &[0.0; 4]).unwrap();

        let with_prov = get_chunks_by_ids_with_provenance(&conn, &["c2".into()]).unwrap();
        assert_eq!(with_prov.len(), 1);
        assert_eq!(with_prov[0].author, Speaker::ToolResult);
    }

    #[test]
    fn test_upsert_and_get_ratification_score() {
        let conn = mem();
        let row = RatificationScoreRow {
            conversation_id: "c1".into(),
            score: 0.6,
            acts_json: r#"{"acts":[]}"#.into(),
            ledger_refs: None,
            extractor_version: "ratification-v1".into(),
        };
        upsert_ratification_score(&conn, &row).unwrap();
        assert_eq!(get_ratification_score(&conn, "c1").unwrap(), Some(0.6));
        let (count, avg) = ratification_summary(&conn).unwrap();
        assert_eq!(count, 1);
        assert!((avg - 0.6).abs() < 1e-6);
        let map = get_ratification_scores(&conn, &["c1".into(), "missing".into()]).unwrap();
        assert_eq!(map.get("c1"), Some(&0.6));
        assert!(!map.contains_key("missing"));
    }

    #[test]
    fn a_story_remembers_how_much_of_the_session_it_covered() {
        let conn = mem();
        assert_eq!(story_messages_covered(&conn, "conv-s").unwrap(), None);
        // a story written before the count was kept covers an unknown part
        mark_enrichment_completed(&conn, "conv-s", "session_story", "story_conv-s").unwrap();
        assert_eq!(story_messages_covered(&conn, "conv-s").unwrap(), None);
        mark_story_written(&conn, "conv-s", "story_conv-s", 300).unwrap();
        assert_eq!(story_messages_covered(&conn, "conv-s").unwrap(), Some(300));
        mark_story_written(&conn, "conv-s", "story_conv-s", 640).unwrap();
        assert_eq!(story_messages_covered(&conn, "conv-s").unwrap(), Some(640));
        assert!(is_conversation_enriched(&conn, "conv-s", "session_story").unwrap());
    }

    #[test]
    fn test_reset_enrichment() {
        let conn = mem();
        mark_enrichment_completed(&conn, "conv-reset", "ratification", "x").unwrap();
        assert!(is_conversation_enriched(&conn, "conv-reset", "ratification").unwrap());

        reset_enrichment(&conn, "conv-reset", "ratification").unwrap();
        assert!(!is_conversation_enriched(&conn, "conv-reset", "ratification").unwrap());

        // Idempotent: deleting an absent row is Ok
        reset_enrichment(&conn, "conv-reset", "ratification").unwrap();
        assert!(!is_conversation_enriched(&conn, "conv-reset", "ratification").unwrap());
    }

    #[test]
    fn test_narrative_usage_summary_cache_tokens() {
        let conn = mem();
        let row = NarrativeUsageRow {
            call_site: "briefing".into(),
            model: "claude-haiku-4-5".into(),
            input_tokens: 100,
            output_tokens: 57,
            cache_read_tokens: 5000,
            cache_creation_tokens: 1200,
            duration_ms: 4200,
            success: true,
        };
        record_narrative_usage(&conn, &row).unwrap();

        let s = narrative_usage_summary(&conn).unwrap();
        // Fresh tokens only (input + output) — cache must not be folded in.
        assert_eq!(s.tokens_total, 157);
        assert_eq!(s.tokens_today, 157);
        // Cache read + creation tracked separately.
        assert_eq!(s.cache_tokens_total, 6200);
        assert_eq!(s.cache_tokens_today, 6200);
        assert_eq!(s.calls_total, 1);
        assert_eq!(s.calls_today, 1);
    }

    #[test]
    fn test_narrative_usage_failed_call_still_counts() {
        let conn = mem();
        let row = NarrativeUsageRow {
            call_site: "story".into(),
            model: "unknown".into(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            duration_ms: 150,
            success: false,
        };
        record_narrative_usage(&conn, &row).unwrap();

        let s = narrative_usage_summary(&conn).unwrap();
        // Failed/attempted calls still count as calls — "calls" means attempts, not successes.
        assert_eq!(s.calls_total, 1);
        assert_eq!(s.calls_today, 1);
        assert_eq!(s.tokens_total, 0);
    }

    #[test]
    fn set_chunk_saga_columns_roundtrip() {
        let conn = mem();
        let chunk = ConversationChunk {
            id: "chunk-saga-1".into(),
            conversation_id: "conv-1".into(),
            project_name: "proj".into(),
            timestamp: "2026-06-10T12:00:00Z".into(),
            content: "hello saga".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        insert_chunk(&conn, &chunk, &[0.1; 384]).unwrap();
        set_chunk_saga_columns(&conn, "chunk-saga-1", 7, true).unwrap();
        let (seq, is_sidechain): (i64, i64) = conn
            .query_row(
                "SELECT seq, is_sidechain FROM chunks WHERE id = ?1",
                params!["chunk-saga-1"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(seq, 7);
        assert_eq!(is_sidechain, 1);
    }

    /// Seed two projects that share a file path from different sessions.
    fn seed_cross_project_shared_file(conn: &Connection) {
        insert_code_evolution(
            conn,
            "sess_a",
            "proj_a",
            "shared.rs",
            "rust",
            "Edit",
            "[]",
            "[]",
            "[]",
            "[]",
            "[]",
            "[]",
            None,
        )
        .unwrap();
        insert_code_evolution(
            conn,
            "sess_b",
            "proj_b",
            "shared.rs",
            "rust",
            "Edit",
            "[]",
            "[]",
            "[]",
            "[]",
            "[]",
            "[]",
            None,
        )
        .unwrap();
    }

    #[test]
    fn sessions_for_file_filters_by_project() {
        let conn = mem();
        seed_cross_project_shared_file(&conn);

        // Scoped to proj_a: must not leak sess_b from proj_b (same file path).
        // Only other-project row exists, so filtered result is empty.
        let got = sessions_for_file(&conn, "shared.rs", "sess_a", Some("proj_a"), 10).unwrap();
        assert!(
            !got.contains(&"sess_b".to_string()),
            "project-scoped lookup must not return other-project sessions: {got:?}"
        );
    }

    #[test]
    fn sessions_for_file_none_project_is_unscoped() {
        let conn = mem();
        seed_cross_project_shared_file(&conn);

        // Unscoped (None): back-compat — can see sess_b across projects.
        let got = sessions_for_file(&conn, "shared.rs", "sess_a", None, 10).unwrap();
        assert!(
            got.contains(&"sess_b".to_string()),
            "unscoped lookup must return other-project sessions: {got:?}"
        );
    }

    #[test]
    fn sessions_for_file_accepts_every_namespace_in_resolved_family() {
        let conn = mem();
        seed_cross_project_shared_file(&conn);

        let got = sessions_for_file_in_projects(
            &conn,
            "shared.rs",
            "sess_a",
            &["proj_a".to_string(), "proj_b".to_string()],
            10,
        )
        .unwrap();
        assert_eq!(got, vec!["sess_b"]);
    }

    fn sample_memory_row(
        file_path: &str,
        project: &str,
        slug: &str,
        origin_session_id: Option<&str>,
        last_seen_scan: i64,
    ) -> MemoryRegistryRow {
        MemoryRegistryRow {
            file_path: file_path.into(),
            project: project.into(),
            slug: slug.into(),
            description: Some(format!("desc-{slug}")),
            mem_type: Some("user".into()),
            origin_session_id: origin_session_id.map(str::to_string),
            modified_ts: Some("2026-08-19T00:00:00Z".into()),
            file_mtime: 1_700_000_000,
            content_hash: format!("hash-{slug}"),
            links_json: "[]".into(),
            last_seen_scan,
        }
    }

    #[test]
    fn memory_registry_upsert_and_lookup_by_origin_session() {
        let conn = mem();
        upsert_memory_registry_batch(
            &conn,
            &[
                sample_memory_row("/mem/a.md", "proj", "a", Some("sess-1"), 1),
                sample_memory_row("/mem/b.md", "proj", "b", None, 1),
            ],
        )
        .unwrap();

        let hit = get_memory_registry_by_origin_session(&conn, "sess-1").unwrap();
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].file_path, "/mem/a.md");
        assert_eq!(hit[0].origin_session_id.as_deref(), Some("sess-1"));

        let miss = get_memory_registry_by_origin_session(&conn, "sess-missing").unwrap();
        assert!(miss.is_empty());
    }

    #[test]
    fn memory_registry_upsert_on_conflict_overwrites_all_fields() {
        let conn = mem();
        upsert_memory_registry_batch(
            &conn,
            &[sample_memory_row(
                "/mem/a.md",
                "proj",
                "first",
                Some("s1"),
                1,
            )],
        )
        .unwrap();

        let mut second = sample_memory_row("/mem/a.md", "proj", "second", Some("s2"), 9);
        second.description = Some("updated-desc".into());
        second.content_hash = "hash-updated".into();
        upsert_memory_registry_batch(&conn, &[second.clone()]).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_registry WHERE file_path = '/mem/a.md'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        let rows = get_memory_registry_by_origin_session(&conn, "s2").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].slug, "second");
        assert_eq!(rows[0].description.as_deref(), Some("updated-desc"));
        assert_eq!(rows[0].content_hash, "hash-updated");
        assert_eq!(rows[0].last_seen_scan, 9);
        assert!(get_memory_registry_by_origin_session(&conn, "s1")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn memory_registry_delete_stale_removes_only_lower_generation_same_project() {
        let conn = mem();
        upsert_memory_registry_batch(
            &conn,
            &[
                sample_memory_row("/mem/gone.md", "proj-a", "gone", None, 1),
                sample_memory_row("/mem/keep1.md", "proj-a", "keep1", None, 2),
                sample_memory_row("/mem/keep2.md", "proj-a", "keep2", None, 2),
            ],
        )
        .unwrap();

        let deleted = delete_memory_registry_stale(&conn, "proj-a", 2).unwrap();
        assert_eq!(deleted, 1);

        let gone: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_registry WHERE file_path = '/mem/gone.md'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(gone, 0);

        let kept: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_registry WHERE project = 'proj-a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, 2);
    }

    #[test]
    fn memory_registry_delete_stale_never_touches_other_projects() {
        let conn = mem();
        upsert_memory_registry_batch(
            &conn,
            &[
                sample_memory_row("/mem/a.md", "proj-a", "a", None, 1),
                sample_memory_row("/mem/b.md", "proj-b", "b", None, 1),
            ],
        )
        .unwrap();

        // Only proj-a was scanned this pass; proj-b was unreadable → never call
        // delete_memory_registry_stale for it.
        let deleted = delete_memory_registry_stale(&conn, "proj-a", 5).unwrap();
        assert_eq!(deleted, 1);

        let a: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_registry WHERE project = 'proj-a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let b: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_registry WHERE project = 'proj-b'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(a, 0);
        assert_eq!(b, 1);
    }

    /// An id keeps its paths only while it has few of them. The per-id read
    /// stops one row past the cap, so a stem with any number of files costs
    /// the same as one with nine.
    #[test]
    fn import_paths_are_returned_only_for_ids_with_few_of_them() {
        let conn = mem();
        let file = |stem: &str, n: usize| {
            for i in 0..n {
                let path = format!("/cc/projects/-f{i}/{stem}.jsonl");
                upsert_import_state_explicit(&conn, &path, stem, 1, "0").unwrap();
            }
        };
        file("one", 1);
        file("eight", 8);
        file("nine", 9);
        file("journal", 500);

        let ids: Vec<String> = [
            "one", "eight", "nine", "journal", "absent", "one", "journal", "absent",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let got = import_paths_for_conversations(&conn, &ids, 8).unwrap();
        let mut keys: Vec<&String> = got.keys().collect();
        keys.sort();
        assert_eq!(keys, ["eight", "one"]);
        assert_eq!(got["one"], ["/cc/projects/-f0/one.jsonl"]);
        assert_eq!(got["eight"].len(), 8);
        assert!(got["eight"].windows(2).all(|w| w[0] < w[1]));
    }

    // ─── get_recent_chunks ───

    fn recent_chunk(
        conv_id: &str,
        project: &str,
        ts: &str,
        content: &str,
        is_sidechain: bool,
    ) -> ConversationChunk {
        ConversationChunk {
            id: format!("{conv_id}-c0"),
            conversation_id: conv_id.into(),
            project_name: project.into(),
            timestamp: ts.into(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: Speaker::ToolResult,
            seq: 0,
            is_sidechain,
        }
    }

    fn insert_recent(
        conn: &Connection,
        conv_id: &str,
        project: &str,
        ts: &str,
        content: &str,
        is_sidechain: bool,
        source: &str,
    ) {
        let chunk = recent_chunk(conv_id, project, ts, content, is_sidechain);
        insert_chunk_with_source(conn, &chunk, &[0.1, 0.2], source).unwrap();
    }

    #[test]
    fn recent_chunks_include_family_namespace_and_exclude_unrelated_sibling() {
        // Hermetic: resolves family membership against a tempdir root instead
        // of the real `$HOME/projects`, so this passes regardless of what
        // exists on the machine running it (was previously gated on
        // `$HOME/projects/claude-self-reflect` genuinely existing, which
        // fails on CI runners — see codex review finding on queries.rs:4648).
        let projects_root = tempfile::tempdir().unwrap();
        std::fs::create_dir(projects_root.path().join("claude-self-reflect")).unwrap();
        // A genuine sibling repo whose directory exists stays out of family —
        // this is the case the family gate exists for, unlike an exact-match
        // query (which would also exclude it, trivially, since the name
        // differs outright).
        std::fs::create_dir(projects_root.path().join("claude-self-reflect-sibling")).unwrap();

        let conn = mem();
        insert_recent(
            &conn,
            "conv-family",
            "claude-self-reflect-csr-engine",
            "2026-09-20T10:00:00Z",
            "family member content",
            false,
            "conversation",
        );
        insert_recent(
            &conn,
            "conv-sibling",
            "claude-self-reflect-sibling",
            "2026-09-20T11:00:00Z",
            "unrelated sibling content",
            false,
            "conversation",
        );

        let projects =
            recent_work_family_projects_at_root(&conn, "claude-self-reflect", projects_root.path())
                .unwrap();
        assert!(
            projects
                .iter()
                .any(|p| p == "claude-self-reflect-csr-engine"),
            "family member must resolve into the project family: {projects:?}"
        );
        assert!(
            !projects.iter().any(|p| p == "claude-self-reflect-sibling"),
            "sibling repo whose directory exists must not leak into the family: {projects:?}"
        );

        let chunks = get_recent_chunks_in_projects(&conn, 10, &projects).unwrap();
        let ids: Vec<&str> = chunks.iter().map(|c| c.conversation_id.as_str()).collect();
        assert!(
            ids.contains(&"conv-family"),
            "family member must be included: {ids:?}"
        );
        assert!(
            !ids.contains(&"conv-sibling"),
            "unrelated sibling must not leak in: {ids:?}"
        );
    }

    #[test]
    fn recent_chunks_exclude_sidechains_and_order_by_last_activity() {
        let conn = mem();
        // Started earliest but touched most recently -> must rank first.
        insert_recent(
            &conn,
            "conv-old-start",
            "proj-recent",
            "2026-09-01T00:00:00Z",
            "first message",
            false,
            "conversation",
        );
        conn.execute(
            "INSERT INTO chunks (id, conversation_id, project_name, timestamp, content, message_count, is_sidechain, source)
             VALUES ('conv-old-start-c1', 'conv-old-start', 'proj-recent', '2026-09-20T00:00:00Z', 'later message', 1, 0, 'conversation')",
            [],
        )
        .unwrap();
        insert_recent(
            &conn,
            "conv-newer-start",
            "proj-recent",
            "2026-09-10T00:00:00Z",
            "started later, never touched again",
            false,
            "conversation",
        );
        insert_recent(
            &conn,
            "agent-sidechain",
            "proj-recent",
            "2026-09-21T00:00:00Z",
            "subagent chatter",
            true,
            "conversation",
        );

        let chunks = get_recent_chunks(&conn, 10, Some("proj-recent")).unwrap();
        let ids: Vec<&str> = chunks.iter().map(|c| c.conversation_id.as_str()).collect();

        assert!(
            !ids.contains(&"agent-sidechain"),
            "sidechain conversation must be excluded: {ids:?}"
        );
        assert_eq!(
            ids[0], "conv-old-start",
            "ordering must follow last activity (max timestamp), not first-chunk start: {ids:?}"
        );
        // Preview stays the *first* chunk's content even though the display
        // timestamp is last-activity.
        let top = &chunks[0];
        assert_eq!(top.content, "first message");
        assert_eq!(top.timestamp, "2026-09-20T00:00:00Z");
    }

    #[test]
    fn recent_chunks_collapse_same_second_codex_burst_and_count_limit_once() {
        let conn = mem();
        insert_recent(
            &conn,
            "codex-a",
            "proj-burst",
            "2026-09-20T10:00:00Z",
            "burst member a",
            false,
            "codex_rollout",
        );
        insert_recent(
            &conn,
            "codex-b",
            "proj-burst",
            "2026-09-20T10:00:00.500Z",
            "burst member b",
            false,
            "codex_rollout",
        );
        insert_recent(
            &conn,
            "codex-c",
            "proj-burst",
            "2026-09-20T10:00:00.900Z",
            "burst member c",
            false,
            "codex_rollout",
        );
        insert_recent(
            &conn,
            "solo-later",
            "proj-burst",
            "2026-09-20T11:00:00Z",
            "solo conversation",
            false,
            "conversation",
        );

        // limit=2 must still return both slots: the solo row and the
        // collapsed burst, not truncate the burst away.
        let chunks = get_recent_chunks(&conn, 2, Some("proj-burst")).unwrap();
        assert_eq!(chunks.len(), 2, "collapsed burst must count as one slot");
        assert_eq!(chunks[0].conversation_id, "solo-later");
        assert!(
            chunks[1].content.starts_with("[+2 parallel]"),
            "burst of 3 same-second codex rows collapses to +2: {}",
            chunks[1].content
        );
    }

    #[test]
    fn recent_chunks_collapse_codex_burst_spanning_several_seconds() {
        // Reproduces the live-DB 01a0bc57 burst: 4 rollouts at
        // 2026-09-20T01:04:05.6xx plus one at 01:04:00.802 — 5 seconds
        // apart, so the old same-UTC-second rule saw it as two rows.
        let conn = mem();
        insert_recent(
            &conn,
            "codex-head",
            "proj-burst2",
            "2026-09-20T01:04:00.802Z",
            "burst head",
            false,
            "codex_rollout",
        );
        for (i, ms) in ["05.601", "05.622", "05.640", "05.658"].iter().enumerate() {
            insert_recent(
                &conn,
                &format!("codex-sibling-{i}"),
                "proj-burst2",
                &format!("2026-09-20T01:04:{ms}Z"),
                "burst sibling",
                false,
                "codex_rollout",
            );
        }
        insert_recent(
            &conn,
            "solo-later",
            "proj-burst2",
            "2026-09-20T02:00:00Z",
            "solo conversation",
            false,
            "conversation",
        );

        let chunks = get_recent_chunks(&conn, 6, Some("proj-burst2")).unwrap();
        assert_eq!(
            chunks.len(),
            2,
            "the whole 5-row burst plus the solo conversation must collapse to 2 rows: {:?}",
            chunks
                .iter()
                .map(|c| &c.conversation_id)
                .collect::<Vec<_>>()
        );
        assert_eq!(chunks[0].conversation_id, "solo-later");
        assert!(
            chunks[1].content.starts_with("[+4 parallel]"),
            "5-row burst spanning 5s collapses to +4: {}",
            chunks[1].content
        );
    }

    #[test]
    fn recent_chunks_burst_tail_truncated_by_fetch_window_gets_exact_count() {
        // Solo rows are newer than the 25-member burst, so limit=4 (3 solos +
        // 1 collapsed burst row) is already satisfied by the *first* fetch
        // window (target=16: 3 solo + 13 burst rows) — the widening retry in
        // get_recent_chunks_in_projects never fires because `collapsed.len()
        // >= limit` is already true. Before the follow-up-count fix, the
        // burst's window-based scan stopped at the fetch-window edge (13
        // members captured of 25) and reported "[+12 parallel]" instead of
        // the true "[+24 parallel]" — this is the regression this test
        // guards (finding on queries.rs `collapse_codex_bursts`). Contrast
        // with `recent_chunks_widening_retry_pulls_solos_behind_head_burst`
        // below, which puts the burst ahead of the solos so the widening
        // retry itself is what gets exercised.
        let conn = mem();
        let burst_len = 25;
        for i in 0..burst_len {
            // Sub-second spacing: the whole burst sits well inside the
            // collapse window regardless of chaining order.
            insert_recent(
                &conn,
                &format!("codex-big-{i}"),
                "proj-bigburst",
                &format!("2026-09-20T03:00:00.{:03}Z", i * 30),
                "burst member",
                false,
                "codex_rollout",
            );
        }
        for i in 0..3 {
            insert_recent(
                &conn,
                &format!("solo-{i}"),
                "proj-bigburst",
                &format!("2026-09-20T04:0{i}:00Z"),
                "solo conversation",
                false,
                "conversation",
            );
        }

        let chunks = get_recent_chunks(&conn, 4, Some("proj-bigburst")).unwrap();
        let ids: Vec<&str> = chunks.iter().map(|c| c.conversation_id.as_str()).collect();
        assert_eq!(
            chunks.len(),
            4,
            "limit must still be honoured despite an oversized burst filling the first window: {ids:?}"
        );
        assert!(
            chunks[..3]
                .iter()
                .all(|c| c.conversation_id.starts_with("solo-")),
            "the 3 solo rows (last-activity order) must surface ahead of the collapsed burst: {ids:?}"
        );
        assert_eq!(
            chunks[3].content, "[+24 parallel] burst member",
            "the burst must report its exact true size (25 members = +24), not just the 13 \
             that fit inside the first fetch window"
        );
    }

    #[test]
    fn recent_chunks_widening_retry_pulls_solos_behind_head_burst() {
        // The burst is *newer* than the solo rows this time, so the first
        // fetch window (target=16 for limit=4) is entirely burst members and
        // collapses to a single row (1 < limit=4) — this is what actually
        // forces get_recent_chunks_in_projects's widening retry to run and
        // fetch a wider window that reaches the solo rows behind the burst.
        // (The sibling test above, with solos ahead, satisfies `limit`
        // before the retry ever fires — it does not exercise this path.)
        let conn = mem();
        let burst_len = 25;
        for i in 0..burst_len {
            insert_recent(
                &conn,
                &format!("codex-big-{i}"),
                "proj-headburst",
                &format!("2026-09-20T04:00:00.{:03}Z", i * 30),
                "burst member",
                false,
                "codex_rollout",
            );
        }
        for i in 0..3 {
            insert_recent(
                &conn,
                &format!("solo-{i}"),
                "proj-headburst",
                &format!("2026-09-20T03:0{i}:00Z"),
                "solo conversation",
                false,
                "conversation",
            );
        }

        let chunks = get_recent_chunks(&conn, 4, Some("proj-headburst")).unwrap();
        let ids: Vec<&str> = chunks.iter().map(|c| c.conversation_id.as_str()).collect();
        assert_eq!(
            chunks.len(),
            4,
            "limit must be honoured once the widening retry pulls in the solo rows: {ids:?}"
        );
        for i in 0..3 {
            let want = format!("solo-{i}");
            assert!(
                ids.contains(&want.as_str()),
                "solo-{i} must be reached by the widening retry: {ids:?}"
            );
        }
        let burst_row = chunks
            .iter()
            .find(|c| c.conversation_id.starts_with("codex-big-"))
            .expect("the collapsed burst row must still be present");
        assert_eq!(
            burst_row.content, "[+24 parallel] burst member",
            "the full 25-member burst must be counted exactly once the retry fetches it whole"
        );
    }

    fn insert_registered_session(
        conn: &Connection,
        session_id: &str,
        project: &str,
        first_ts: &str,
        last_ts: &str,
    ) {
        upsert_session_registry_batch(
            conn,
            &[SessionRegistryRow {
                session_id: session_id.to_string(),
                project: project.to_string(),
                first_prompt: Some("real prompt".to_string()),
                first_ts: first_ts.to_string(),
                last_ts: last_ts.to_string(),
                prompt_count_delta: 1,
            }],
        )
        .unwrap();
    }

    #[test]
    fn recent_chunks_exclude_headless_sdk_cli_entrypoint_conversation() {
        // W3-B1 review#2 (HIGH), 2nd pass: exclusion must be driven by a
        // *positive* headless signal (the transcript's own `entrypoint`),
        // never by absence from `session_registry` — the old mechanism hid
        // real sessions `history.jsonl` never happened to record. A
        // registered interactive session, an unregistered-but-real
        // `entrypoint=cli` session, and a `entrypoint=sdk-cli` headless probe
        // all coexist here; only the `sdk-cli` one is dropped.
        let conn = mem();
        insert_registered_session(
            &conn,
            "interactive-a",
            "proj-registry",
            "2026-09-20T09:00:00Z",
            "2026-09-20T09:05:00Z",
        );
        insert_recent(
            &conn,
            "interactive-a",
            "proj-registry",
            "2026-09-20T10:00:00Z",
            "real user prompt",
            false,
            "conversation",
        );
        set_conversation_entrypoint(&conn, "interactive-a", "cli").unwrap();

        insert_recent(
            &conn,
            "probe-b",
            "proj-registry",
            "2026-09-20T09:03:00Z",
            "Reply with exactly the word PROBE-DONE",
            false,
            "conversation",
        );
        set_conversation_entrypoint(&conn, "probe-b", "sdk-cli").unwrap();

        let chunks = get_recent_chunks(&conn, 6, Some("proj-registry")).unwrap();
        let ids: Vec<&str> = chunks.iter().map(|c| c.conversation_id.as_str()).collect();

        assert!(
            ids.contains(&"interactive-a"),
            "a real cli-entrypoint session must still surface: {ids:?}"
        );
        assert!(
            !ids.contains(&"probe-b"),
            "a sdk-cli-entrypoint headless probe conversation must be excluded: {ids:?}"
        );
    }

    #[test]
    fn recent_chunks_surface_unregistered_cli_entrypoint_despite_nonempty_registry() {
        // W3-B1 review#2 (HIGH): the exact regression the reviewer found live
        // — `entrypoint=cli`, but `history.jsonl`/`session_registry` never
        // recorded it (six real whats-next sessions did this). A non-empty
        // registry (containing an unrelated session) must not make this one
        // read as headless.
        let conn = mem();
        insert_registered_session(
            &conn,
            "some-other-registered-session",
            "proj-registry-lag",
            "2026-09-20T09:00:00Z",
            "2026-09-20T09:05:00Z",
        );
        insert_recent(
            &conn,
            "brand-new-live",
            "proj-registry-lag",
            "2026-09-20T12:00:00Z",
            "What directory are you operating on",
            false,
            "conversation",
        );
        set_conversation_entrypoint(&conn, "brand-new-live", "cli").unwrap();

        let chunks = get_recent_chunks(&conn, 6, Some("proj-registry-lag")).unwrap();
        let ids: Vec<&str> = chunks.iter().map(|c| c.conversation_id.as_str()).collect();
        assert!(
            ids.contains(&"brand-new-live"),
            "an unregistered real cli-entrypoint session must surface despite a non-empty registry: {ids:?}"
        );
    }

    #[test]
    fn recent_chunks_fail_open_when_entrypoint_unknown() {
        // No `conversation_entrypoint` row at all (pre-dates the column, or
        // the source file is gone) must never be read as headless — only an
        // explicit `sdk-*` value is a headless signal.
        let conn = mem();
        insert_recent(
            &conn,
            "conv-a",
            "proj-empty-registry",
            "2026-09-20T09:00:00Z",
            "real user prompt",
            false,
            "conversation",
        );

        let chunks = get_recent_chunks(&conn, 6, Some("proj-empty-registry")).unwrap();
        let ids: Vec<&str> = chunks.iter().map(|c| c.conversation_id.as_str()).collect();
        assert!(
            ids.contains(&"conv-a"),
            "a conversation with no recorded entrypoint must fail open, not be hidden: {ids:?}"
        );
    }

    #[test]
    fn recent_chunks_exclude_csr_emission_when_entrypoint_unknown() {
        // W3-B1 review#2 (HIGH): the fallback net for rows with no recorded
        // entrypoint at all — CSR's own emission echoed back must still be
        // excluded on that basis, exactly as before.
        let conn = mem();
        insert_recent(
            &conn,
            "narrator-echo",
            "proj-emission",
            "2026-09-20T09:00:00Z",
            "You are CSR Episode Analyst reviewing a transcript",
            false,
            "conversation",
        );
        insert_recent(
            &conn,
            "conv-real",
            "proj-emission",
            "2026-09-20T09:01:00Z",
            "real user prompt",
            false,
            "conversation",
        );

        let chunks = get_recent_chunks(&conn, 6, Some("proj-emission")).unwrap();
        let ids: Vec<&str> = chunks.iter().map(|c| c.conversation_id.as_str()).collect();
        assert!(
            !ids.contains(&"narrator-echo"),
            "a CSR-emission preview with no entrypoint recorded must still be excluded: {ids:?}"
        );
        assert!(
            ids.contains(&"conv-real"),
            "a real session in the same batch must still surface: {ids:?}"
        );
    }

    #[test]
    fn recent_work_family_excludes_headless_scratchpad_namespace() {
        let conn = mem();
        insert_recent(
            &conn,
            "conv-main",
            "myproj",
            "2026-09-20T09:00:00Z",
            "real content",
            false,
            "conversation",
        );
        insert_recent(
            &conn,
            "conv-scratch",
            "myproj-scratchpad-abc123",
            "2026-09-20T10:00:00Z",
            "headless workflow cwd content",
            false,
            "conversation",
        );

        // Only the base dir exists -- the scratchpad namespace is a runtime
        // worktree/cwd alias, not a checked-out sibling repo, so it must
        // pass the alias check (`is_alias_at_root`) on its own merits before
        // the scratchpad filter even gets a chance to exclude it.
        let projects_root = tempfile::tempdir().unwrap();
        std::fs::create_dir(projects_root.path().join("myproj")).unwrap();

        assert!(
            crate::search::cross_project::same_project_family_at_root(
                "myproj",
                "myproj-scratchpad-abc123",
                projects_root.path(),
            ),
            "sanity: the fixture must actually resolve as a family alias so this test can tell \
             the scratchpad filter apart from the alias check itself"
        );

        let projects =
            recent_work_family_projects_at_root(&conn, "myproj", projects_root.path()).unwrap();
        assert!(
            !projects.iter().any(|p| p.contains("-scratchpad-")),
            "a headless scratchpad namespace must never join the family: {projects:?}"
        );
    }

    #[test]
    fn recent_work_family_projects_with_home_none_returns_exact_scope_only() {
        // Deterministic regardless of this machine's real `$HOME`: a
        // candidate that would otherwise be a plausible family member (same
        // hyphenated prefix) must still be excluded when there is no
        // resolvable home directory to form a projects root against.
        let conn = mem();
        insert_recent(
            &conn,
            "conv-a",
            "myproj",
            "2026-09-20T09:00:00Z",
            "content",
            false,
            "conversation",
        );
        insert_recent(
            &conn,
            "conv-b",
            "myproj-csr-engine",
            "2026-09-20T09:00:00Z",
            "content",
            false,
            "conversation",
        );

        let projects = recent_work_family_projects_with_home(&conn, "myproj", None).unwrap();
        assert_eq!(
            projects,
            vec!["myproj".to_string()],
            "no resolvable home directory must mean exact scope only: {projects:?}"
        );
    }

    #[test]
    fn recent_chunks_preview_skips_codex_harness_boilerplate_first_chunk() {
        // W3-B1 review#2 finding: a still-polluted Codex rollout's chunk 0 is
        // harness boilerplate (`<recommended_plugins>...`); the preview must
        // skip to the first real chunk instead of showing the boilerplate.
        let conn = mem();
        insert_chunk_with_source(
            &conn,
            &ConversationChunk {
                id: "codex-polluted-c0".into(),
                conversation_id: "codex-polluted".into(),
                project_name: "proj-preview".into(),
                timestamp: "2026-09-20T09:00:00Z".into(),
                content: "<recommended_plugins>\nHere is a list of plugins\n</recommended_plugins>"
                    .into(),
                message_count: 1,
                summary: None,
                author: Speaker::ToolResult,
                seq: 0,
                is_sidechain: false,
            },
            &[0.1, 0.2],
            "codex_rollout",
        )
        .unwrap();
        insert_chunk_with_source(
            &conn,
            &ConversationChunk {
                id: "codex-polluted-c1".into(),
                conversation_id: "codex-polluted".into(),
                project_name: "proj-preview".into(),
                timestamp: "2026-09-20T09:00:01Z".into(),
                content: "real user request about the recall bug".into(),
                message_count: 1,
                summary: None,
                author: Speaker::ToolResult,
                seq: 1,
                is_sidechain: false,
            },
            &[0.1, 0.2],
            "codex_rollout",
        )
        .unwrap();

        let chunks = get_recent_chunks(&conn, 6, Some("proj-preview")).unwrap();
        let top = chunks
            .iter()
            .find(|c| c.conversation_id == "codex-polluted")
            .expect("the polluted conversation must still surface");
        assert_eq!(
            top.content, "real user request about the recall bug",
            "the preview must skip the harness-boilerplate first chunk: {}",
            top.content
        );
    }

    #[test]
    fn recent_chunks_preview_skips_multi_chunk_codex_harness_span() {
        // W3-B1 review#2 (MEDIUM), 2nd pass: the real `codex:01a0c5df` shape —
        // only chunk 0 starts with a harness marker, but the injected block
        // (CSR's chunker re-splits it at a flat char budget with no idea it
        // was boilerplate) continues, unmarked, across several more chunks
        // before its closing tag turns up, and only THEN does the real user
        // prompt begin. A per-chunk-only noisy check (the old behaviour)
        // picks the first unmarked continuation chunk — pure boilerplate —
        // as the preview, exactly as it did live.
        let conn = mem();
        let mut rows: Vec<(String, usize, String)> = vec![(
            "harness-span-c0".to_string(),
            0,
            "<recommended_plugins>\nHere is a list of plugins, continued below".to_string(),
        )];
        for seq in 1..11usize {
            rows.push((
                format!("harness-span-c{seq}"),
                seq,
                format!("- Consensus (app-{seq:x}…@openai-curated-remote) plugin entry"),
            ));
        }
        rows.push((
            "harness-span-c11".to_string(),
            11,
            "...the rest of the plugin list </recommended_plugins>".to_string(),
        ));
        rows.push((
            "harness-span-c12".to_string(),
            12,
            "dont look at the literature install graphify".to_string(),
        ));

        for (id, seq, content) in &rows {
            insert_chunk_with_source(
                &conn,
                &ConversationChunk {
                    id: id.clone(),
                    conversation_id: "harness-span".into(),
                    project_name: "proj-preview".into(),
                    timestamp: format!("2026-09-20T09:00:{seq:02}Z"),
                    content: content.clone(),
                    message_count: 1,
                    summary: None,
                    author: Speaker::ToolResult,
                    seq: *seq,
                    is_sidechain: false,
                },
                &[0.1, 0.2],
                "codex_rollout",
            )
            .unwrap();
        }

        let chunks = get_recent_chunks(&conn, 6, Some("proj-preview")).unwrap();
        let top = chunks
            .iter()
            .find(|c| c.conversation_id == "harness-span")
            .expect("the polluted conversation must still surface");
        assert_eq!(
            top.content, "dont look at the literature install graphify",
            "the preview must skip the whole multi-chunk harness span, not just chunk 0: {}",
            top.content
        );
    }

    #[test]
    fn recent_chunks_preview_survives_harness_block_closing_and_opening_in_same_chunk() {
        // W3-B1 review#2 (MEDIUM), 3rd pass: the exact live `codex:01a0c5df`
        // shape (verified on a clone of the production DB). Multiple harness
        // blocks are packed back-to-back inside ONE original message with no
        // chunk-boundary alignment: `</recommended_plugins>` (closing block
        // A) and `<INSTRUCTIONS>` (opening block B, the "# AGENTS.md
        // instructions" body) land in the very same stored chunk. A "first
        // closing tag ends the span" rule reopens picking right there — on
        // block B's own boilerplate body (which is CLAUDE.md content quoted
        // as agent instructions, live) — instead of staying closed until
        // block B's own `</INSTRUCTIONS>` shows up several chunks later.
        let conn = mem();
        let rows: Vec<(&str, usize, &str)> = vec![
            ("c0", 0, "<recommended_plugins>\nplugin list starts here"),
            ("c1", 1, "more plugins, no marker of its own"),
            (
                "c2",
                2,
                "</recommended_plugins>\n# AGENTS.md instructions\n<INSTRUCTIONS>\nCore prefs",
            ),
            ("c3", 3, "more instructions body, no marker of its own"),
            ("c4", 4, "closing soon </INSTRUCTIONS>"),
            ("c5", 5, "the real first prompt of the session"),
        ];
        for (id, seq, content) in &rows {
            insert_chunk_with_source(
                &conn,
                &ConversationChunk {
                    id: format!("same-chunk-{id}"),
                    conversation_id: "same-chunk-span".into(),
                    project_name: "proj-preview".into(),
                    timestamp: format!("2026-09-20T09:00:{seq:02}Z"),
                    content: content.to_string(),
                    message_count: 1,
                    summary: None,
                    author: Speaker::ToolResult,
                    seq: *seq,
                    is_sidechain: false,
                },
                &[0.1, 0.2],
                "codex_rollout",
            )
            .unwrap();
        }

        let chunks = get_recent_chunks(&conn, 6, Some("proj-preview")).unwrap();
        let top = chunks
            .iter()
            .find(|c| c.conversation_id == "same-chunk-span")
            .expect("the polluted conversation must still surface");
        assert_eq!(
            top.content, "the real first prompt of the session",
            "closing one block and opening the next in the same chunk must not reopen picking early: {}",
            top.content
        );
    }

    #[test]
    fn recent_chunks_preview_skips_local_command_caveat_first_chunk() {
        // A Claude session that opens with a slash command (`/login`) has a
        // `<local-command-caveat>` wrapper as its first chunk (live-DB
        // bd3294f7/d8fe3a96); the preview must skip that wrapper rather than
        // showing the harness caveat text itself.
        let conn = mem();
        insert_registered_session(
            &conn,
            "conv-slash",
            "proj-preview-claude",
            "2026-09-20T09:00:00Z",
            "2026-09-20T09:00:01Z",
        );
        insert_chunk_with_source(
            &conn,
            &ConversationChunk {
                id: "conv-slash-c0".into(),
                conversation_id: "conv-slash".into(),
                project_name: "proj-preview-claude".into(),
                timestamp: "2026-09-20T09:00:00Z".into(),
                content: "<local-command-caveat>Caveat: The messages below were generated \
                          by the user while running local commands. DO NOT respond\
                          </local-command-caveat>"
                    .into(),
                message_count: 1,
                summary: None,
                author: Speaker::ToolResult,
                seq: 0,
                is_sidechain: false,
            },
            &[0.1, 0.2],
            "conversation",
        )
        .unwrap();
        insert_chunk_with_source(
            &conn,
            &ConversationChunk {
                id: "conv-slash-c1".into(),
                conversation_id: "conv-slash".into(),
                project_name: "proj-preview-claude".into(),
                timestamp: "2026-09-20T09:00:01Z".into(),
                content: "/login".into(),
                message_count: 1,
                summary: None,
                author: Speaker::ToolResult,
                seq: 1,
                is_sidechain: false,
            },
            &[0.1, 0.2],
            "conversation",
        )
        .unwrap();

        let chunks = get_recent_chunks(&conn, 6, Some("proj-preview-claude")).unwrap();
        let top = chunks
            .iter()
            .find(|c| c.conversation_id == "conv-slash")
            .expect("the session must still surface");
        assert_eq!(
            top.content, "/login",
            "the preview must skip the local-command-caveat wrapper: {}",
            top.content
        );
    }

    #[test]
    fn recent_chunks_preview_survives_noncontiguous_rowids_between_own_chunks() {
        // W3-B1 review#2 (LOW): the live-DB `d8fe3a96`/`356b2649` case — the
        // watcher imports incrementally and interleaves conversations, so a
        // session's own chunk 1 can land dozens of rowids after its chunk 0
        // once other conversations' chunks are inserted in between. A
        // `rowid <` range window sized for *contiguous* rowids then holds
        // only chunk 0 (the caveat) and never even sees chunk 1, no matter
        // how large the window is raised. Ranking by `seq` instead must
        // still find chunk 1 regardless of how many unrelated rows separate
        // the two by rowid.
        let conn = mem();
        insert_chunk_with_source(
            &conn,
            &ConversationChunk {
                id: "conv-gap-c0".into(),
                conversation_id: "conv-gap".into(),
                project_name: "proj-preview-gap".into(),
                timestamp: "2026-09-20T09:00:00Z".into(),
                content: "<local-command-caveat>Caveat: The messages below were generated \
                          by the user while running local commands. DO NOT respond\
                          </local-command-caveat>"
                    .into(),
                message_count: 1,
                summary: None,
                author: Speaker::ToolResult,
                seq: 0,
                is_sidechain: false,
            },
            &[0.1, 0.2],
            "conversation",
        )
        .unwrap();
        // 40 unrelated chunks from a different, unrelated conversation land
        // in between by rowid — more than any realistic `rowid <` window,
        // reproducing the live gap (21 rows) at a size that would defeat
        // even the review's own suggested "raise the window" band-aid.
        for i in 0..40 {
            insert_chunk_with_source(
                &conn,
                &ConversationChunk {
                    id: format!("filler-{i}"),
                    conversation_id: "filler-conv".into(),
                    project_name: "proj-preview-gap".into(),
                    timestamp: format!("2026-09-20T08:{:02}:00Z", i % 60),
                    content: "unrelated filler content".into(),
                    message_count: 1,
                    summary: None,
                    author: Speaker::ToolResult,
                    seq: i as usize,
                    is_sidechain: false,
                },
                &[0.1, 0.2],
                "conversation",
            )
            .unwrap();
        }
        insert_chunk_with_source(
            &conn,
            &ConversationChunk {
                id: "conv-gap-c1".into(),
                conversation_id: "conv-gap".into(),
                project_name: "proj-preview-gap".into(),
                timestamp: "2026-09-20T09:00:01Z".into(),
                content: "the real first prompt of the session".into(),
                message_count: 1,
                summary: None,
                author: Speaker::ToolResult,
                seq: 1,
                is_sidechain: false,
            },
            &[0.1, 0.2],
            "conversation",
        )
        .unwrap();

        let chunks = get_recent_chunks(&conn, 6, Some("proj-preview-gap")).unwrap();
        let top = chunks
            .iter()
            .find(|c| c.conversation_id == "conv-gap")
            .expect("the session must still surface");
        assert_eq!(
            top.content, "the real first prompt of the session",
            "the preview must find chunk 1 by seq despite a large rowid gap: {}",
            top.content
        );
    }

    #[test]
    fn recent_chunks_exclude_codex_subagent_child_marked_sidechain() {
        // Once the codex_rollout adapter correctly marks a `thread_spawn`
        // child's chunks `is_sidechain = true` (see
        // `import::codex_rollout::backfill_codex_subagent_sidechain`), the
        // existing sidechain exclusion here must keep it from taking its own
        // recent-work slot next to its parent.
        let conn = mem();
        insert_recent(
            &conn,
            "codex:parent-thread",
            "proj-subagent",
            "2026-09-20T10:00:00Z",
            "parent rollout",
            false,
            "codex_rollout",
        );
        insert_recent(
            &conn,
            "codex:child-thread",
            "proj-subagent",
            "2026-09-20T10:00:00.050Z",
            "subagent child rollout",
            true,
            "codex_rollout",
        );

        let chunks = get_recent_chunks(&conn, 6, Some("proj-subagent")).unwrap();
        let ids: Vec<&str> = chunks.iter().map(|c| c.conversation_id.as_str()).collect();
        assert!(
            ids.contains(&"codex:parent-thread"),
            "the parent rollout must surface: {ids:?}"
        );
        assert!(
            !ids.contains(&"codex:child-thread"),
            "a subagent child marked is_sidechain must never take its own slot: {ids:?}"
        );
    }

    #[test]
    fn codex_burst_key_ts_uses_uuidv7_spawn_time_not_restamped_first_chunk_ts() {
        // Reproduces the live-DB 01a093d5 pair: two rollouts spawned 100ms
        // apart via `thread_spawn` (per their UUIDv7 ids), but the
        // chunk-timestamp restamp rewrote their first-chunk timestamps to
        // the *message* time, 7 minutes apart. Burst grouping must use the
        // id's embedded spawn time, not the restamped chunk timestamp.
        let head_id = "codex:01935e3b-0000-7000-8000-000000000001";
        let sibling_id = "codex:01935e3b-0064-7000-8000-000000000002"; // +0x64 = +100ms
        let head_ts = codex_burst_key_ts(head_id, "2026-09-20T04:16:49.700Z");
        let sibling_ts = codex_burst_key_ts(sibling_id, "2026-09-20T04:23:47.808Z");
        assert!(
            within_burst_window(&head_ts, &sibling_ts),
            "two ids 100ms apart must be treated as the same burst even though their \
             restamped first-chunk timestamps are 7 minutes apart: head={head_ts} sibling={sibling_ts}"
        );
    }

    #[test]
    fn recent_chunks_collapse_codex_burst_by_uuidv7_id_despite_restamped_gap() {
        let conn = mem();
        insert_recent(
            &conn,
            "codex:01935e3b-0000-7000-8000-000000000001",
            "proj-uuidburst",
            "2026-09-20T04:16:49.700Z",
            "burst head",
            false,
            "codex_rollout",
        );
        insert_recent(
            &conn,
            "codex:01935e3b-0064-7000-8000-000000000002",
            "proj-uuidburst",
            "2026-09-20T04:23:47.808Z",
            "burst sibling",
            false,
            "codex_rollout",
        );
        insert_recent(
            &conn,
            "solo-later",
            "proj-uuidburst",
            "2026-09-20T05:00:00Z",
            "solo conversation",
            false,
            "conversation",
        );

        let chunks = get_recent_chunks(&conn, 4, Some("proj-uuidburst")).unwrap();
        let ids: Vec<&str> = chunks.iter().map(|c| c.conversation_id.as_str()).collect();
        assert_eq!(
            chunks.len(),
            2,
            "the UUIDv7-adjacent pair must collapse into one row despite a 7-minute \
             restamped first-chunk gap: {ids:?}"
        );
        assert_eq!(chunks[0].conversation_id, "solo-later");
        assert!(
            chunks[1].content.starts_with("[+1 parallel]"),
            "the pair must report exactly one parallel sibling: {}",
            chunks[1].content
        );
    }

    #[test]
    fn latest_chunk_names_the_conversation_get_most_recent_session_names() {
        let conn = mem();
        let put = |id: &str, conv: &str, project: &str, ts: &str| {
            let mut chunk = test_chunk(id, "body");
            chunk.conversation_id = conv.into();
            chunk.project_name = project.into();
            chunk.timestamp = ts.into();
            insert_chunk(&conn, &chunk, &[0.0; 4]).unwrap();
        };
        put("a1", "conv-a", "p", "2026-10-01T10:00:00Z");
        put("a2", "conv-a", "p", "2026-10-03T10:00:00Z");
        put("b1", "conv-b", "p", "2026-10-02T10:00:00Z");
        put("b2", "conv-b", "p", "2026-10-04T09:00:00Z");
        put("c1", "conv-c", "q", "2026-10-05T10:00:00Z");
        for project in ["p", "q", "all"] {
            let session = get_most_recent_session(&conn, project).unwrap().unwrap();
            assert_eq!(
                latest_chunk_in_project(&conn, project).unwrap(),
                Some((session.conversation_id, session.timestamp)),
                "{project}"
            );
        }
        assert_eq!(latest_chunk_in_project(&conn, "none").unwrap(), None);
        // A tie on the newest timestamp goes to the smallest conversation id.
        put("z1", "conv-z", "p", "2026-10-04T09:00:00Z");
        put("a3", "conv-0", "p", "2026-10-04T09:00:00Z");
        assert_eq!(
            latest_chunk_in_project(&conn, "p").unwrap(),
            Some(("conv-0".into(), "2026-10-04T09:00:00Z".into()))
        );
    }

    #[test]
    fn reflections_by_tag_return_what_the_table_scan_returned() {
        let conn = mem();
        for i in 0..30 {
            let tags: Vec<String> = match i % 3 {
                0 => vec!["session_episode".into(), "conv_abc".into()],
                1 => vec!["session_story".into(), "conv_abc-def".into()],
                _ => vec!["conv_other".into()],
            };
            let id = format!("r{i:02}");
            insert_reflection(
                &conn,
                &id,
                &format!("body {i} {}", "x".repeat(5000)),
                &tags,
                &[0.0; 4],
            )
            .unwrap();
            conn.execute(
                "UPDATE reflections SET timestamp = ?1 WHERE id = ?2",
                params![
                    format!("2026-09-{:02}T00:00:{:02}Z", 1 + (i * 7) % 28, i),
                    id
                ],
            )
            .unwrap();
        }
        let scan = |tag: &str, limit: i64| -> Vec<(String, String)> {
            let pattern = format!("%{}%", tag.replace('%', "\\%").replace('_', "\\_"));
            let mut stmt = conn
                .prepare(
                    "SELECT id, timestamp FROM reflections
                     WHERE tags LIKE ?1 ESCAPE '\\' ORDER BY timestamp DESC LIMIT ?2",
                )
                .unwrap();
            stmt.query_map(params![pattern, limit], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        for (tag, limit) in [
            ("conv_abc", 10),
            ("conv_abc", 3),
            ("session_episode", 50),
            ("CONV_OTHER", 4),
            ("nothing", 5),
        ] {
            let got: Vec<(String, String)> = get_reflections_by_tag(&conn, tag, limit)
                .unwrap()
                .into_iter()
                .map(|(id, _, _, ts)| (id, ts))
                .collect();
            assert_eq!(got, scan(tag, limit as i64), "{tag} {limit}");
        }
    }

    #[test]
    fn batched_episode_rows_match_one_tag_scan_per_conversation() {
        let conn = mem();
        let mut n = 0;
        let mut put = |tags: &[&str], body: &str| {
            n += 1;
            let id = format!("r{n:03}");
            let tags: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
            insert_reflection(&conn, &id, body, &tags, &[0.0; 4]).unwrap();
            conn.execute(
                "UPDATE reflections SET timestamp = ?1 WHERE id = ?2",
                params![
                    format!("2026-09-{:02}T{:02}:00:00Z", 1 + (n * 5) % 28, n % 24),
                    id
                ],
            )
            .unwrap();
        };
        // An episode, its story and extract, and enough newer rows of one
        // conversation to push its episode out of a 3-row window.
        put(&["session_episode", "conv_aaa"], "{\"episode\":\"aaa\"}");
        put(&["session_story", "conv_aaa"], "story aaa");
        put(
            &["session_episode", "conv_aaa-bbb"],
            "{\"episode\":\"aaa-bbb\"}",
        );
        put(&["session_episode", "CONV_CCC"], "{\"episode\":\"ccc\"}");
        for i in 0..6 {
            put(&["context_cache", "conv_ddd"], &format!("cache {i}"));
        }
        put(&["session_episode", "conv_ddd"], "{\"episode\":\"ddd\"}");
        put(&["narrative_extracted_v3", "conv_eee"], "no episode here");
        let ids = ["aaa", "aaa-bbb", "ccc", "ddd", "eee", "missing", "aaa"];
        for window in [3usize, 10] {
            let batched = episode_rows_by_conversation(&conn, &ids, window).unwrap();
            for id in ids {
                let one: Vec<(String, String)> =
                    get_reflections_by_tag(&conn, &format!("conv_{id}"), window)
                        .unwrap()
                        .into_iter()
                        .filter(|(_, _, tags, _)| tags.iter().any(|t| t == "session_episode"))
                        .map(|(_, content, _, ts)| (content, ts))
                        .collect();
                assert_eq!(batched.get(id), Some(&one), "{id} window {window}");
            }
        }
    }
}
