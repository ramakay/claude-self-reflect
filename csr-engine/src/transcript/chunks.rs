//! Answer a transcript query from the indexed chunks when the session's JSONL
//! and its mirror copy are both gone.
//!
//! The chunks are what the importer kept for search, not a transcript. This
//! module rebuilds them in order, says plainly that the result is lossy and
//! what the importer dropped, and tells a session CSR still knows about (the
//! chunks, or the session registry) apart from one it has never seen.
//!
//! Everything here is a read: the caller passes a [`Storage`] opened read-only
//! (CLI) or the server's own handle (MCP), and nothing is written or migrated.

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::import::{CHUNK_CHAR_BUDGET, MAX_TOOL_RESULT_CHARS, TOOL_COMMAND_DETAIL_CHARS};
use crate::storage::Storage;

/// At most this many chunks are read for one rebuild. A longer session is cut
/// here and the header says so.
pub(crate) const MAX_REBUILT_CHUNKS: usize = 5_000;

/// A session id shorter than this only matches exactly, never as a prefix.
const MIN_PREFIX_CHARS: usize = 8;

/// The chunk sources that come from the Claude Code JSONL importer and so share
/// the loss list below. `codex_rollout` and `plan` chunks are built by other
/// adapters and are not rebuilt here.
const REBUILDABLE_SOURCES: [&str; 2] = ["conversation", "sidechain"];

/// One stored chunk, in rebuilt order.
#[derive(Debug, Clone)]
pub(crate) struct RebuiltChunk {
    pub seq: Option<i64>,
    pub timestamp: String,
    /// `chunk_provenance.author`: `user`, `assistant`, `tool_result`, or
    /// `unknown` when the chunk has no provenance row.
    pub author: String,
    pub message_count: i64,
    pub content: String,
}

/// A session rebuilt from its chunks.
#[derive(Debug, Clone)]
pub(crate) struct RebuiltSession {
    pub conversation_id: String,
    pub project: String,
    /// Chunks the database holds for the session (before the read cap).
    pub total_chunks: usize,
    pub chunks: Vec<RebuiltChunk>,
    pub first_ts: String,
    pub last_ts: String,
}

impl RebuiltSession {
    pub(crate) fn capped(&self) -> bool {
        self.total_chunks > self.chunks.len()
    }
}

/// A session CSR knows about but holds no chunks to rebuild from.
#[derive(Debug, Clone)]
pub(crate) struct KnownSession {
    pub session_id: String,
    pub project: String,
    pub first_ts: Option<String>,
    /// `Some((source, chunk count))` when chunks exist but come from an adapter
    /// whose chunks are not rebuilt (`codex_rollout`, `plan`).
    pub other_source_chunks: Option<(String, i64)>,
    /// True when the knowledge comes from the session registry.
    pub in_registry: bool,
}

pub(crate) enum DbLookup {
    Rebuilt(RebuiltSession),
    Known(KnownSession),
    /// More than one session matches; `(id, project)` pairs, sorted.
    Ambiguous(Vec<(String, String)>),
    Unknown,
}

fn valid_session_id(session: &str) -> bool {
    !session.is_empty()
        && session
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

/// Look `session` up in the database: the exact id first, else a unique prefix
/// of at least [`MIN_PREFIX_CHARS`]. Any database error reads as `Unknown`, so
/// a broken or old-schema database degrades to the plain not-found message.
pub(crate) fn lookup(storage: &Storage, session: &str, project: Option<&str>) -> DbLookup {
    if !valid_session_id(session) {
        return DbLookup::Unknown;
    }
    storage
        .with_connection(|conn| Ok(lookup_in(conn, session, project)))
        .unwrap_or(DbLookup::Unknown)
}

/// One `(conversation_id, project, source, chunks)` row of the id lookup.
type IdRow = (String, String, String, i64);

fn id_rows(
    conn: &Connection,
    session: &str,
    project: Option<&str>,
) -> rusqlite::Result<Vec<IdRow>> {
    // Ids are alphanumeric, `-` and `_` only (validated by the caller), so the
    // GLOB prefix below carries no wildcard of its own.
    let run = |where_id: &str, arg: &str| -> rusqlite::Result<Vec<IdRow>> {
        let sql = format!(
            "SELECT conversation_id, project_name, source, count(*) FROM chunks \
             WHERE {where_id} AND (?2 IS NULL OR instr(lower(project_name), lower(?2)) > 0) \
             GROUP BY conversation_id, project_name, source \
             ORDER BY conversation_id, project_name, source"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![arg, project], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        rows.collect()
    };
    let exact = run("conversation_id = ?1", session)?;
    if !exact.is_empty() || session.chars().count() < MIN_PREFIX_CHARS {
        return Ok(exact);
    }
    let prefix = format!("{session}*");
    run("conversation_id GLOB ?1", &prefix)
}

fn registry_rows(
    conn: &Connection,
    session: &str,
    project: Option<&str>,
) -> rusqlite::Result<Vec<(String, String, Option<String>)>> {
    let run =
        |where_id: &str, arg: &str| -> rusqlite::Result<Vec<(String, String, Option<String>)>> {
            let sql = format!(
                "SELECT session_id, project, first_ts FROM session_registry \
             WHERE {where_id} AND (?2 IS NULL OR instr(lower(project), lower(?2)) > 0) \
             ORDER BY session_id"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params![arg, project], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
            rows.collect()
        };
    let exact = run("session_id = ?1", session)?;
    if !exact.is_empty() || session.chars().count() < MIN_PREFIX_CHARS {
        return Ok(exact);
    }
    let prefix = format!("{session}*");
    run("session_id GLOB ?1", &prefix)
}

fn lookup_in(conn: &Connection, session: &str, project: Option<&str>) -> DbLookup {
    let rows = id_rows(conn, session, project).unwrap_or_default();
    let mut pairs: Vec<(String, String)> = rows
        .iter()
        .map(|(id, project, _, _)| (id.clone(), project.clone()))
        .collect();
    pairs.dedup();
    if pairs.len() > 1 {
        return DbLookup::Ambiguous(pairs);
    }
    if let Some((id, proj)) = pairs.first() {
        let rebuildable = rows
            .iter()
            .any(|(_, _, source, _)| REBUILDABLE_SOURCES.contains(&source.as_str()));
        if rebuildable {
            // A read that fails here must not turn into "no chunks".
            return match load_session(conn, id, proj) {
                Ok(Some(rebuilt)) => DbLookup::Rebuilt(rebuilt),
                _ => DbLookup::Unknown,
            };
        }
        let other = rows
            .iter()
            .find(|(_, _, source, _)| !REBUILDABLE_SOURCES.contains(&source.as_str()))
            .map(|(_, _, source, n)| (source.clone(), *n));
        let registry_ts = registry_rows(conn, id, None)
            .ok()
            .and_then(|r| r.into_iter().next())
            .and_then(|(_, _, ts)| ts);
        return DbLookup::Known(KnownSession {
            session_id: id.clone(),
            project: proj.clone(),
            first_ts: registry_ts,
            other_source_chunks: other,
            in_registry: false,
        });
    }

    let registry = registry_rows(conn, session, project).unwrap_or_default();
    if registry.len() > 1 {
        let listing = registry
            .into_iter()
            .map(|(id, project, _)| (id, project))
            .collect();
        return DbLookup::Ambiguous(listing);
    }
    match registry.into_iter().next() {
        Some((session_id, project, first_ts)) => DbLookup::Known(KnownSession {
            session_id,
            project,
            first_ts,
            other_source_chunks: None,
            in_registry: true,
        }),
        None => DbLookup::Unknown,
    }
}

fn load_session(
    conn: &Connection,
    conversation_id: &str,
    project: &str,
) -> rusqlite::Result<Option<RebuiltSession>> {
    let (total, first_ts, last_ts): (i64, Option<String>, Option<String>) = conn.query_row(
        "SELECT count(*), min(timestamp), max(timestamp) FROM chunks \
         WHERE conversation_id = ?1 AND project_name = ?2 \
           AND source IN ('conversation', 'sidechain')",
        params![conversation_id, project],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if total == 0 {
        return Ok(None);
    }

    let read = |with_author: bool| -> rusqlite::Result<Vec<RebuiltChunk>> {
        let author = if with_author {
            "COALESCE((SELECT p.author FROM chunk_provenance p WHERE p.chunk_id = c.id), 'unknown')"
        } else {
            "'unknown'"
        };
        // Chunk timestamps are the session's last-message time at import, so
        // they are equal across one import pass: `seq` (then rowid, for rows
        // that predate `seq`) is what orders them within a pass.
        let sql = format!(
            "SELECT c.timestamp, c.seq, {author}, c.message_count, c.content FROM chunks c \
             WHERE c.conversation_id = ?1 AND c.project_name = ?2 \
               AND c.source IN ('conversation', 'sidechain') \
             ORDER BY c.timestamp, c.seq, c.rowid LIMIT {MAX_REBUILT_CHUNKS}"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![conversation_id, project], |row| {
            Ok(RebuiltChunk {
                timestamp: row.get(0)?,
                seq: row.get(1)?,
                author: row.get(2)?,
                message_count: row.get(3)?,
                content: row.get(4)?,
            })
        })?;
        rows.collect()
    };
    let chunks = read(true).or_else(|_| read(false))?;

    Ok(Some(RebuiltSession {
        conversation_id: conversation_id.to_string(),
        project: project.to_string(),
        total_chunks: total as usize,
        chunks,
        first_ts: first_ts.unwrap_or_default(),
        last_ts: last_ts.unwrap_or_default(),
    }))
}

// ─── What the header says ───

/// What the importer did not keep, so a reader knows what a rebuilt transcript
/// cannot show. Every limit is read from the importer's own constants
/// (csr-engine/src/import/mod.rs): `CHUNK_CHAR_BUDGET` (the chunk size and the
/// joiner between messages), `MAX_TOOL_RESULT_CHARS` (tool_result cut),
/// `TOOL_COMMAND_DETAIL_CHARS` (tool_use command detail in `extract_tool_context`),
/// plus the block filter in `extract_message_text_raw` (text blocks only),
/// `extract_tool_context` (tool_use reduced to `[Name: detail]`),
/// `drops_as_marker_only` (tool-call-only assistant turns dropped),
/// `strip_private_tags` (`[private]`), `crate::redact` (`[secret redacted: …]`),
/// `sanitize_message_for_search` (CSR's own tool blocks suppressed, harness-written
/// rows dropped, hook wrappers scrubbed), `trim_chunk_edges`, and `push_chunk`
/// (the timestamp is the session's last-message time at import).
pub(crate) fn lossy_notes() -> Vec<String> {
    vec![
        "only text, tool_use and tool_result blocks were indexed: thinking blocks, images, attachments, system rows and summary rows are not in any chunk".to_string(),
        format!(
            "tool calls are one-line markers `[Name: detail]` (detail = last 2 path components of file_path, else the first {TOOL_COMMAND_DETAIL_CHARS} characters of command, else pattern or query): every other tool input (Edit old/new text, Write content, Task prompts) is gone, and an assistant turn holding only such markers was not indexed at all"
        ),
        format!(
            "each tool_result was cut to {MAX_TOOL_RESULT_CHARS} bytes"
        ),
        format!(
            "role labels are not stored in the text: messages inside one chunk are joined with a blank line, a chunk holds up to {CHUNK_CHAR_BUDGET} bytes and can hold several messages, and the author shown is the strongest speaker in it (user over assistant over tool_result)"
        ),
        format!(
            "a message over {CHUNK_CHAR_BUDGET} bytes is split at a byte boundary into consecutive chunks with no overlap, so text that crosses a split is not matched by grep"
        ),
        "per-message timestamps are not kept: a chunk carries the session's last-message time as of its import".to_string(),
        "redacted before indexing: text inside private tags is `[private]`, credentials are `[secret redacted: kind]`, CSR's own tool calls and results, hook-injected CSR reminder wrappers and harness-written user rows (task notifications, peer messages) are removed; leading and trailing whitespace of each chunk is trimmed".to_string(),
    ]
}

/// Plain-text header lines, shared by every view and by `get_full_conversation`.
pub(crate) fn header_lines(s: &RebuiltSession) -> Vec<String> {
    let mut lines = vec![
        format!(
            "NOT THE ORIGINAL TRANSCRIPT: the JSONL for session {} is gone and CSR kept no transcript copy of it.",
            s.conversation_id
        ),
        format!(
            "Rebuilt from {} indexed chunks in the CSR database (chunk timestamps {} .. {}), project {}. Turn numbers below are chunk numbers in rebuilt order.",
            s.total_chunks, s.first_ts, s.last_ts, s.project
        ),
    ];
    if s.capped() {
        lines.push(format!(
            "CAPPED: only the first {} of {} chunks were read.",
            s.chunks.len(),
            s.total_chunks
        ));
    }
    lines.push("LOSSY, this is what the importer did not keep:".to_string());
    lines.extend(lossy_notes().into_iter().map(|n| format!("- {n}")));
    lines
}

/// Fields added to every JSON answer built from chunks.
pub(crate) fn json_fields(s: &RebuiltSession) -> Vec<(String, Value)> {
    vec![
        ("source".to_string(), json!("db_chunks")),
        ("lossy".to_string(), json!(lossy_notes())),
        ("session".to_string(), json!(s.conversation_id)),
        ("project".to_string(), json!(s.project)),
        ("chunks_total".to_string(), json!(s.total_chunks)),
        ("chunks_read".to_string(), json!(s.chunks.len())),
        ("capped".to_string(), json!(s.capped())),
        ("first_ts".to_string(), json!(s.first_ts)),
        ("last_ts".to_string(), json!(s.last_ts)),
    ]
}

// ─── Known but not rebuildable ───

fn known_sentence(k: &KnownSession) -> String {
    let seen = k.first_ts.as_deref().unwrap_or("unknown time");
    match &k.other_source_chunks {
        Some((source, n)) => format!(
            "Session {} (project {}) has {n} indexed chunks from source '{source}' in CSR's database, but its JSONL is gone and CSR does not rebuild a transcript from that source.",
            k.session_id, k.project
        ),
        None => format!(
            "Session {} is in CSR's session registry (project {}, first seen {seen}), but its JSONL is gone and CSR kept no transcript copy or indexed chunks for it.",
            k.session_id, k.project
        ),
    }
}

/// The answer for a session CSR knows about but cannot rebuild. Same envelope
/// as the plain not-found message, minus its false "not imported yet" line.
pub(crate) fn known_message(k: &KnownSession, json: bool) -> String {
    let sentence = known_sentence(k);
    if json {
        json!({
            "error": sentence,
            "known_to": if k.in_registry { "session_registry" } else { "chunks" },
            "session": k.session_id,
            "project": k.project,
            "first_seen": k.first_ts,
        })
        .to_string()
    } else {
        format!(
            "<conversation_file>\n<error>{}</error>\n</conversation_file>",
            super::query::xml_escape(&sentence)
        )
    }
}

/// The `get_full_conversation` envelope for a rebuilt session: the same header,
/// then the chunks in order, cut at `budget_chars`.
pub(crate) fn full_conversation_text(s: &RebuiltSession, budget_chars: usize) -> String {
    use super::query::xml_escape;
    let mut out = String::from("<conversation_file>\n");
    out.push_str(&format!(
        "<conversation_id>{}</conversation_id>\n<project>{}</project>\n<source>db_chunks</source>\n",
        xml_escape(&s.conversation_id),
        xml_escape(&s.project)
    ));
    out.push_str("<notice>\n");
    for line in header_lines(s) {
        out.push_str(&xml_escape(&line));
        out.push('\n');
    }
    out.push_str("</notice>\n<chunks>\n");
    let mut used = out.chars().count();
    let mut shown = 0usize;
    for (i, c) in s.chunks.iter().enumerate() {
        let line = format!(
            "[turn {}] {}{}: {}\n",
            i + 1,
            xml_escape(&c.author),
            if c.message_count > 1 {
                format!(" (x{} messages)", c.message_count)
            } else {
                String::new()
            },
            xml_escape(&c.content)
        );
        let n = line.chars().count();
        if used + n > budget_chars {
            break;
        }
        out.push_str(&line);
        used += n;
        shown += 1;
    }
    if shown < s.chunks.len() {
        out.push_str(&format!(
            "... {} more chunks not shown; use csr_transcript(session=\"{}\", view=\"slice\", turns=\"{}..\") to continue\n",
            s.chunks.len() - shown,
            xml_escape(&s.conversation_id),
            shown + 1
        ));
    }
    out.push_str("</chunks>\n</conversation_file>");
    out
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) fn temp_storage(dir: &std::path::Path) -> Storage {
        Storage::open(&dir.join("csr-engine.db")).unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn add_chunk(
        storage: &Storage,
        id: &str,
        conv: &str,
        project: &str,
        ts: &str,
        seq: i64,
        author: &str,
        message_count: i64,
        content: &str,
    ) {
        storage
            .with_connection(|c| {
                c.execute(
                    "INSERT INTO chunks (id, conversation_id, project_name, timestamp, content, \
                     message_count, seq, source) VALUES (?1,?2,?3,?4,?5,?6,?7,'conversation')",
                    params![id, conv, project, ts, content, message_count, seq],
                )?;
                c.execute(
                    "INSERT INTO chunk_provenance (chunk_id, author, source_conv_id) VALUES (?1,?2,?3)",
                    params![id, author, conv],
                )?;
                Ok(())
            })
            .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{add_chunk, temp_storage};
    use super::*;

    #[test]
    fn invalid_ids_are_unknown_without_touching_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let storage = temp_storage(dir.path());
        for bad in ["", "a/b", "a%b", "x*"] {
            assert!(matches!(lookup(&storage, bad, None), DbLookup::Unknown));
        }
    }

    #[test]
    fn rows_come_back_in_timestamp_then_seq_order() {
        let dir = tempfile::tempdir().unwrap();
        let storage = temp_storage(dir.path());
        let conv = "11111111-aaaa-bbbb-cccc-222222222222";
        add_chunk(
            &storage,
            "c3",
            conv,
            "proj",
            "2026-08-02T00:00:00Z",
            2,
            "user",
            1,
            "third",
        );
        add_chunk(
            &storage,
            "c1",
            conv,
            "proj",
            "2026-08-01T00:00:00Z",
            0,
            "user",
            1,
            "first",
        );
        add_chunk(
            &storage,
            "c2",
            conv,
            "proj",
            "2026-08-01T00:00:00Z",
            1,
            "assistant",
            2,
            "second",
        );
        let DbLookup::Rebuilt(s) = lookup(&storage, conv, None) else {
            panic!("expected a rebuilt session");
        };
        let order: Vec<&str> = s.chunks.iter().map(|c| c.content.as_str()).collect();
        assert_eq!(order, ["first", "second", "third"]);
        assert_eq!(s.chunks[1].author, "assistant");
        assert_eq!(s.first_ts, "2026-08-01T00:00:00Z");
        assert_eq!(s.last_ts, "2026-08-02T00:00:00Z");
        assert!(!s.capped());
    }
}
