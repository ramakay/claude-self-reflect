//! Prompts typed while Claude was busy.
//!
//! Claude Code writes such a prompt only as a `queued_command` attachment (plus
//! `queue-operation` rows), never as a user row, so the importer's chunks never
//! held it. The parse rule is [`crate::transcript::queued_command`], shared with
//! the transcript reader and the story's `typed_prompts`; this module reads the
//! human ones off the importer's own pass, indexes them as chunk rows of their
//! own (see `storage::queued_prompts` for why they are not woven into the
//! conversation's chunks), and backfills the conversations imported before.
//!
//! A prompt is indexed once: the ledger is keyed by the transcript line, so the
//! import path, the watcher, the backfill and any rerun all land on one row set.
//! A prompt whose text also arrives as a user row at or after its place is that
//! same prompt, already in the conversation's chunks, and is not indexed again;
//! the story's `asked_since` pairs them by the same function.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, LazyLock};

use anyhow::Result;
use tokio::sync::RwLock;

use super::{
    chunk_is_sidechain, extract_message_text, provenance_event_id, sanitize_message_for_search,
    ConversationChunk, CsrMessageSanitizer, ParentContext, CHUNK_CHAR_BUDGET, CSR_NAMESPACE,
};
use crate::embeddings::EmbeddingEngine;
use crate::provenance::{
    content_hash, trust_for_channel, ChunkEvidence, ChunkSpan, ProvenanceEvent, Speaker, TrustTier,
};
use crate::search::SearchEngine;
use crate::storage::queued_prompts::QueuedPromptWrite;
use crate::storage::Storage;

/// A human-typed queued prompt as the importer read it.
#[derive(Debug, Clone)]
pub(crate) struct QueuedRow {
    /// How many messages precede it in the transcript.
    pub place: usize,
    /// Byte offset of its line.
    pub line_offset: u64,
    /// The person's text, sanitized and scrubbed exactly like a user row's.
    pub text: String,
    /// The text as the pairing compares it: the person's own words, whitespace
    /// collapsed.
    pub key: String,
    pub timestamp: Option<String>,
    pub is_sidechain: bool,
    pub event: ProvenanceEvent,
}

/// Gathers queued prompts and the typed user rows after them while the importer
/// walks the transcript.
#[derive(Default)]
pub(crate) struct Collector {
    rows: Vec<QueuedRow>,
    seen: HashSet<(String, String)>,
    /// Typed text of each user row from the first queued prompt on, by text:
    /// the message indexes it sits at.
    user_rows: HashMap<String, Vec<usize>>,
}

impl Collector {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn see_attachment(
        &mut self,
        row: &serde_json::Value,
        conversation_id: &str,
        path: &Path,
        line_start: u64,
        place: usize,
        floor: Option<TrustTier>,
        parent: Option<&ParentContext>,
        sanitizer: &mut CsrMessageSanitizer,
    ) {
        let Some(command) = row
            .get("attachment")
            .and_then(crate::transcript::queued_command)
        else {
            return;
        };
        if !command.human_turn {
            return;
        }
        let timestamp = row
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        // The same enqueue can be written twice; one prompt, one count.
        let stamp = timestamp.clone().unwrap_or_default();
        if !self.seen.insert((stamp, command.prompt.clone())) {
            return;
        }
        let mut message =
            serde_json::json!({"type": "user", "message": {"content": command.prompt}});
        sanitize_message_for_search(&mut message, sanitizer);
        let text = extract_message_text(&message).trim().to_string();
        let Some(key) = crate::summarizer::typed_words(&text) else {
            return;
        };
        let message_key = row
            .get("uuid")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| content_hash(&text));
        let mut tier = trust_for_channel("user_message", floor.unwrap_or(TrustTier::Unknown));
        if let Some(parent) = parent {
            tier = tier.min(parent.floor);
        }
        let event = ProvenanceEvent {
            event_id: provenance_event_id(conversation_id, &message_key, place, "user_message", 0),
            conversation_id: conversation_id.to_string(),
            message_key,
            seq: place,
            channel: "user_message".to_string(),
            trust_tier: tier,
            parent_event_id: parent.and_then(|context| context.event_id.clone()),
            receipt_kind: "jsonl".to_string(),
            receipt_ref: Some(format!("{}#byte={line_start}", path.display())),
            observed_at: timestamp
                .clone()
                .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string()),
        };
        self.rows.push(QueuedRow {
            place,
            line_offset: line_start,
            text,
            key,
            timestamp,
            is_sidechain: row
                .get("isSidechain")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            event,
        });
    }

    /// A message the importer kept, already sanitized. Only user rows after the
    /// first queued prompt can be a twin of one.
    pub(super) fn see_message(&mut self, seq: usize, message: &serde_json::Value) {
        if self.rows.is_empty() {
            return;
        }
        for text in crate::summarizer::typed_prompts(std::slice::from_ref(message), 1, usize::MAX) {
            self.user_rows.entry(text).or_default().push(seq);
        }
    }

    /// The prompts that have no user row of their own.
    pub(super) fn finish(self) -> Vec<QueuedRow> {
        let places: Vec<(usize, Option<String>)> = self
            .rows
            .iter()
            .map(|row| (row.place, Some(row.key.clone())))
            .collect();
        let twins = pair_with_rows(&places, &self.user_rows);
        self.rows
            .into_iter()
            .zip(twins)
            .filter_map(|(row, twin)| twin.is_none().then_some(row))
            .collect()
    }
}

/// For each queued prompt (its place among the messages and its typed text), the
/// index of the user row that is the same prompt, if any: the first row with
/// that text at or after its place that no earlier prompt took. `None` text:
/// nothing the person typed, so nothing to pair. Shared by the story's
/// `asked_since` and by indexing, so a prompt counts as one thing in both.
pub(crate) fn pair_with_rows(
    prompts: &[(usize, Option<String>)],
    rows_by_text: &HashMap<String, Vec<usize>>,
) -> Vec<Option<usize>> {
    let mut taken: HashSet<usize> = HashSet::new();
    prompts
        .iter()
        .map(|(place, text)| {
            let rows = rows_by_text.get(text.as_ref()?)?;
            let at = *rows
                .iter()
                .find(|at| **at >= *place && !taken.contains(*at))?;
            taken.insert(at);
            Some(at)
        })
        .collect()
}

/// Where a conversation's queued prompts belong.
pub(crate) struct Target<'a> {
    pub conversation_id: &'a str,
    pub project_name: &'a str,
    /// The storage `source` of the conversation's own rows.
    pub source: &'a str,
    pub parent_conversation_id: Option<&'a str>,
}

fn chunk_id(conversation_id: &str, line_offset: u64, part: usize) -> String {
    let input = format!("{conversation_id}-queued-{line_offset}-{part}");
    uuid::Uuid::new_v5(&CSR_NAMESPACE, input.as_bytes()).to_string()
}

/// The chunks of one prompt: its text cut at the chunk budget, each with the
/// receipt that points at the transcript line.
fn chunks_for(target: &Target<'_>, row: &QueuedRow) -> Vec<(ConversationChunk, ChunkEvidence)> {
    let text = row.text.as_str();
    let mut pieces = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + CHUNK_CHAR_BUDGET).min(text.len());
        end = text.floor_char_boundary(end);
        if end <= start {
            end = text.len();
        }
        let piece = &text[start..end];
        let lead = piece.len() - piece.trim_start().len();
        let kept = piece.trim();
        if !kept.is_empty() {
            pieces.push((start + lead, kept));
        }
        start = end;
    }
    pieces
        .into_iter()
        .enumerate()
        .map(|(part, (from, kept))| {
            let id = chunk_id(target.conversation_id, row.line_offset, part);
            let start_char = text[..from].chars().count();
            let chunk = ConversationChunk {
                id: id.clone(),
                conversation_id: target.conversation_id.to_string(),
                project_name: target.project_name.to_string(),
                timestamp: row
                    .timestamp
                    .clone()
                    .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string()),
                content: kept.to_string(),
                message_count: 1,
                summary: None,
                author: Speaker::User,
                seq: 0,
                is_sidechain: chunk_is_sidechain(&[row.is_sidechain], target.conversation_id),
            };
            let evidence = ChunkEvidence {
                chunk_id: id.clone(),
                events: vec![row.event.clone()],
                spans: vec![ChunkSpan {
                    chunk_id: id,
                    event_id: row.event.event_id.clone(),
                    start_char,
                    end_char: start_char + kept.chars().count(),
                    content_hash: content_hash(kept),
                }],
                min_trust: row.event.trust_tier,
                tool_result_share: Some(0.0),
            };
            (chunk, evidence)
        })
        .collect()
}

/// Index the prompts of one conversation that are not indexed yet, and record
/// that its transcript was read, once all are. Returns how many prompts were
/// added. Call it under the operation lock.
pub(crate) async fn index(
    storage: &Storage,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    target: &Target<'_>,
    rows: &[QueuedRow],
) -> Result<usize> {
    let indexed = storage.queued_prompt_offsets(target.conversation_id)?;
    if rows.iter().any(|r| !indexed.contains(&r.line_offset)) {
        // This transcript holds prompts not indexed yet. Until every one is, the
        // conversation must not read as scanned: an embedding or insert error
        // below returns before the record is written, and a record left from an
        // earlier pass would keep the backfill from ever retrying it. A pass with
        // nothing to index leaves the record alone, so a live session's watcher
        // does not write on every turn.
        storage.clear_queued_scan(target.conversation_id)?;
    }
    let mut added = 0usize;
    for row in rows.iter().filter(|r| !indexed.contains(&r.line_offset)) {
        let chunks = chunks_for(target, row);
        if chunks.is_empty() {
            continue;
        }
        let texts: Vec<String> = chunks.iter().map(|(c, _)| c.content.clone()).collect();
        let emb = embeddings.clone();
        let vectors = tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            emb.embed(&refs)
        })
        .await??;
        let write = QueuedPromptWrite {
            conversation_id: target.conversation_id.to_string(),
            line_offset: row.line_offset,
            source: target.source.to_string(),
            source_conv_id: target
                .parent_conversation_id
                .unwrap_or(target.conversation_id)
                .to_string(),
            chunks: chunks
                .into_iter()
                .zip(vectors.iter())
                .map(|((chunk, evidence), vector)| (chunk, vector.clone(), evidence))
                .collect(),
        };
        if storage.insert_queued_prompt(&write)? {
            added += 1;
            let mut idx = search.write().await;
            for (chunk, vector, _) in write.chunks {
                idx.insert_chunk(chunk.id, vector);
            }
        }
    }
    storage.record_queued_scan(target.conversation_id, rows.len(), added)?;
    Ok(added)
}

static MENTIONS_QUEUED: LazyLock<regex::bytes::Regex> =
    LazyLock::new(|| regex::bytes::Regex::new("queued_command").expect("literal pattern"));

/// Cheap test before a full parse: does the transcript mention a queued
/// command at all? Most do not.
fn mentions_queued_command(path: &Path) -> std::io::Result<bool> {
    use std::io::Read;
    const BLOCK: usize = 8 << 20;
    const KEEP: usize = 16;
    let mut file = std::fs::File::open(path)?;
    let mut buffer = vec![0u8; BLOCK + KEEP];
    let mut carried = 0usize;
    loop {
        let read = file.read(&mut buffer[carried..])?;
        if read == 0 {
            return Ok(false);
        }
        let filled = carried + read;
        if MENTIONS_QUEUED.is_match(&buffer[..filled]) {
            return Ok(true);
        }
        carried = KEEP.min(filled);
        buffer.copy_within(filled - carried..filled, 0);
    }
}

/// What one backfill batch did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct BatchStats {
    pub scanned: usize,
    /// Prompts newly indexed.
    pub added: usize,
    /// Transcripts it could not read this time; they stay pending.
    pub failed: usize,
    /// The last file path handled, where the next batch continues.
    pub last_path: String,
    /// The batch found fewer than `limit` pending transcripts.
    pub done: bool,
}

/// One bounded batch of the backfill: read each imported transcript the pass has
/// not read yet, and index the queued prompts it holds. Additive only: it writes
/// new chunk rows and nothing else, and never touches `import_state`, so a
/// conversation imported before the scrubber is not stamped as scrub-aware by it.
/// `after` is the file path to continue after (empty to start).
pub(crate) async fn backfill_batch(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    projects_dir: &Path,
    after: &str,
    limit: usize,
) -> Result<BatchStats> {
    let pending = storage.pending_queued_scan_batch(after, limit)?;
    let mut stats = BatchStats {
        last_path: after.to_string(),
        done: pending.len() < limit,
        ..BatchStats::default()
    };
    for (file_path, conversation_id) in pending {
        stats.last_path = file_path.clone();
        match scan_file(
            storage,
            embeddings,
            search,
            projects_dir,
            Path::new(&file_path),
            &conversation_id,
        )
        .await
        {
            Ok(added) => {
                stats.scanned += 1;
                stats.added += added;
            }
            Err(error) => {
                stats.failed += 1;
                tracing::warn!(file = %file_path, %error, "queued-prompt backfill could not read a transcript");
            }
        }
    }
    Ok(stats)
}

async fn scan_file(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    projects_dir: &Path,
    path: &Path,
    conversation_id: &str,
) -> Result<usize> {
    let _operation = storage.operation_lock()?;
    // Plans, tasks and other adapters keep import_state rows too, and a transcript
    // Claude Code has since deleted has nothing to read: nothing to add, and no
    // reason to look again.
    let is_transcript =
        path.starts_with(projects_dir) && path.extension().is_some_and(|ext| ext == "jsonl");
    if !is_transcript {
        storage.record_queued_scan(conversation_id, 0, 0)?;
        return Ok(0);
    }
    match mentions_queued_command(path) {
        Ok(true) => {}
        Ok(false) => {
            storage.record_queued_scan(conversation_id, 0, 0)?;
            return Ok(0);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            storage.record_queued_scan(conversation_id, 0, 0)?;
            return Ok(0);
        }
        Err(error) => return Err(anyhow::Error::new(error).context("reading transcript")),
    }
    let attribution = super::derive_conversation_attribution(projects_dir, path);
    let parent_context = attribution
        .parent_conversation_id
        .as_deref()
        .map(|parent_id| {
            let message_key = super::sidechain_parent_message_key(path);
            storage.parent_provenance_context(parent_id, message_key.as_deref())
        })
        .transpose()?;
    let parse_path = path.to_path_buf();
    let project = attribution.project_name.clone();
    // Every message counts as already stored: nothing is chunked, only the
    // queued prompts come back.
    let rows = tokio::task::spawn_blocking(move || {
        super::parse_jsonl_file_resuming(
            &parse_path,
            &project,
            parent_context.as_ref(),
            None,
            Some(super::ResumePoint {
                after_seq: usize::MAX,
                first_chunk: 0,
            }),
        )
        .map(|parsed| parsed.queued)
    })
    .await??;
    let target = Target {
        conversation_id,
        project_name: &attribution.project_name,
        source: attribution.source,
        parent_conversation_id: attribution.parent_conversation_id.as_deref(),
    };
    index(storage, embeddings, search, &target, &rows).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;

    use crate::engine::Engine;

    struct Rig {
        _dir: tempfile::TempDir,
        data_dir: PathBuf,
        projects: PathBuf,
    }

    impl Rig {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let data_dir = dir.path().join("data");
            let projects = dir.path().join("projects");
            fs::create_dir_all(&data_dir).unwrap();
            fs::create_dir_all(projects.join("-tmp-proj")).unwrap();
            Self {
                data_dir,
                projects,
                _dir: dir,
            }
        }

        fn source(&self, name: &str) -> PathBuf {
            self.projects.join("-tmp-proj").join(name)
        }

        fn engine(&self) -> Engine {
            Engine::from_parts_with_index_dir(
                Arc::new(Storage::open(&self.data_dir.join("csr-engine.db")).unwrap()),
                Arc::new(EmbeddingEngine::new().unwrap()),
                Arc::new(RwLock::new(SearchEngine::new(64))),
                self.projects.clone(),
                self.data_dir.join("index-v3"),
            )
        }
    }

    fn user(n: usize, text: &str) -> String {
        serde_json::json!({
            "type": "user", "uuid": format!("u-{n}"),
            "timestamp": format!("2026-10-14T10:00:{n:02}Z"),
            "message": {"content": text}
        })
        .to_string()
            + "\n"
    }

    fn assistant(n: usize, text: &str) -> String {
        serde_json::json!({
            "type": "assistant", "uuid": format!("a-{n}"),
            "timestamp": format!("2026-10-14T10:00:{n:02}Z"),
            "message": {"content": [{"type": "text", "text": text}]}
        })
        .to_string()
            + "\n"
    }

    /// A `queued_command` attachment, as Claude Code writes one. `human`: the
    /// `humanTurn` flag, absent when `None`.
    fn queued(n: usize, text: &str, human: Option<bool>) -> String {
        let mut attachment = serde_json::json!({
            "type": "queued_command", "prompt": text, "commandMode": "prompt",
            "timestamp": format!("2026-10-14T10:01:{n:02}Z"),
        });
        if let Some(human) = human {
            attachment["humanTurn"] = human.into();
        }
        serde_json::json!({
            "type": "attachment", "uuid": format!("q-{n}"),
            "timestamp": format!("2026-10-14T10:01:{n:02}Z"),
            "attachment": attachment,
        })
        .to_string()
            + "\n"
    }

    fn enqueue(n: usize, text: &str) -> String {
        serde_json::json!({
            "type": "queue-operation", "operation": "enqueue",
            "timestamp": format!("2026-10-14T10:01:{n:02}Z"), "content": text,
        })
        .to_string()
            + "\n"
    }

    fn append(path: &Path, text: &str) {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }

    fn scalar(engine: &Engine, sql: &str) -> i64 {
        engine
            .storage()
            .with_connection(|conn| Ok(conn.query_row(sql, [], |row| row.get(0))?))
            .unwrap()
    }

    fn forget_scans(engine: &Engine) {
        engine
            .storage()
            .with_connection(|c| Ok(c.execute("DELETE FROM queued_prompt_scan", [])?))
            .unwrap();
    }

    /// Chunk rows whose text holds `needle`.
    fn chunks_like(engine: &Engine, needle: &str) -> i64 {
        engine
            .storage()
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM chunks WHERE content LIKE ?1",
                    [format!("%{needle}%")],
                    |row| row.get(0),
                )?)
            })
            .unwrap()
    }

    /// Every column of every chunk row of `conv` outside the queued ledger, and its
    /// embedding, as text.
    fn stored_rows(engine: &Engine, conv: &str) -> Vec<String> {
        engine
            .storage()
            .with_connection(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT c.id, c.project_name, c.timestamp, c.content, c.message_count,
                            c.summary, c.seq, c.is_sidechain, c.source, e.embedding,
                            c.min_trust, c.tool_result_share
                     FROM chunks c LEFT JOIN chunk_embeddings e ON e.chunk_id = c.id
                     WHERE c.conversation_id = ?1
                       AND c.id NOT IN (SELECT chunk_id FROM queued_prompt_chunks)
                     ORDER BY c.id",
                )?;
                let rows = stmt
                    .query_map([conv], |row| {
                        let mut cells = Vec::new();
                        for i in 0..12 {
                            cells.push(format!("{:?}", row.get::<_, rusqlite::types::Value>(i)?));
                        }
                        Ok(cells.join("|"))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .unwrap()
    }

    /// Bind every chunk of `conv` to a witness; returns the bound chunk ids.
    fn bind_to_a_witness(engine: &Engine, conv: &str) -> Vec<String> {
        engine
            .storage()
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO witness_ledger (project, file, symbol, stamp, tier, source_kind)
                     VALUES ('proj', '/repo/a.rs', 'foo', 'b3:1', 'committed', 'test')",
                    [],
                )?;
                let witness: i64 =
                    conn.query_row("SELECT id FROM witness_ledger", [], |r| r.get(0))?;
                conn.execute(
                    "INSERT INTO witness_chunk_bindings (witness_id, chunk_id)
                     SELECT ?1, id FROM chunks WHERE conversation_id = ?2",
                    rusqlite::params![witness, conv],
                )?;
                Ok(())
            })
            .unwrap();
        bindings(engine)
    }

    fn bindings(engine: &Engine) -> Vec<String> {
        engine
            .storage()
            .with_connection(|conn| {
                let mut stmt =
                    conn.prepare("SELECT chunk_id FROM witness_chunk_bindings ORDER BY chunk_id")?;
                let ids = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(ids)
            })
            .unwrap()
    }

    /// Whether full-text search finds `word` in a chunk row.
    fn found_by_search(engine: &Engine, word: &str) -> bool {
        engine
            .storage()
            .with_connection(|conn| {
                let hits: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM chunks c JOIN chunks_fts fts ON fts.rowid = c.rowid
                     WHERE chunks_fts MATCH ?1",
                    [word],
                    |row| row.get(0),
                )?;
                Ok(hits > 0)
            })
            .unwrap()
    }

    #[derive(Clone, Copy, Debug)]
    enum Via {
        Engine,
        Watcher,
    }

    async fn import_via(engine: &Engine, rig: &Rig, src: &Path, via: Via) {
        match via {
            Via::Engine => {
                engine.import_file(src, "proj").await.unwrap();
            }
            Via::Watcher => {
                crate::import::watcher::import_file_content_diff(
                    engine.storage(),
                    engine.embeddings(),
                    engine.search(),
                    &rig.projects,
                    src,
                )
                .await
                .unwrap();
            }
        }
    }

    async fn backfill(engine: &Engine, rig: &Rig, after: &str, limit: usize) -> BatchStats {
        backfill_batch(
            engine.storage(),
            engine.embeddings(),
            engine.search(),
            &rig.projects,
            after,
            limit,
        )
        .await
        .unwrap()
    }

    /// Two prompts typed while Claude was busy, and two things the harness queued.
    fn busy_session(first_user: &str) -> String {
        [
            user(0, first_user),
            assistant(1, "working on it, reading the files now"),
            enqueue(2, "zebracrossing the second request"),
            queued(2, "zebracrossing the second request", Some(true)),
            queued(
                3,
                "<task-notification>build finished</task-notification>",
                Some(false),
            ),
            queued(
                4,
                "<task-notification>another harness note</task-notification>",
                None,
            ),
            assistant(5, "done with the first part"),
            queued(6, "quokkaparade the third request", Some(true)),
            assistant(7, "and the second"),
        ]
        .concat()
    }

    async fn new_conversation_imports_each_prompt_once(via: Via) {
        let rig = Rig::new();
        let engine = rig.engine();
        let src = rig.source("sess-new.jsonl");
        let session = busy_session("please refactor the parser");
        fs::write(&src, &session).unwrap();

        import_via(&engine, &rig, &src, via).await;
        assert_eq!(
            scalar(
                &engine,
                "SELECT COUNT(*) FROM queued_prompt_chunks WHERE part = 0"
            ),
            2,
            "{via:?}"
        );
        assert_eq!(chunks_like(&engine, "zebracrossing"), 1, "{via:?}");
        assert_eq!(chunks_like(&engine, "quokkaparade"), 1, "{via:?}");
        assert_eq!(
            chunks_like(&engine, "task-notification"),
            0,
            "{via:?}: harness text stays out"
        );
        assert!(found_by_search(&engine, "quokkaparade"), "{via:?}");
        assert_eq!(
            scalar(
                &engine,
                "SELECT COUNT(*) FROM chunk_embeddings e JOIN queued_prompt_chunks q ON q.chunk_id = e.chunk_id"
            ),
            2,
            "{via:?}"
        );

        // The transcript grows and the conversation is imported again; then its
        // import record is lost and the whole file is imported once more.
        append(&src, &user(9, "thanks, now the tests"));
        import_via(&engine, &rig, &src, via).await;
        engine
            .storage()
            .with_connection(|c| Ok(c.execute("DELETE FROM import_state", [])?))
            .unwrap();
        import_via(&engine, &rig, &src, via).await;
        assert_eq!(chunks_like(&engine, "zebracrossing"), 1, "{via:?}");
        assert_eq!(chunks_like(&engine, "quokkaparade"), 1, "{via:?}");
        assert_eq!(
            engine.storage().queued_prompt_counts().unwrap().indexed,
            2,
            "{via:?}"
        );
    }

    #[tokio::test]
    async fn a_new_conversation_indexes_its_queued_prompts_once_through_the_engine() {
        new_conversation_imports_each_prompt_once(Via::Engine).await;
    }

    #[tokio::test]
    async fn a_new_conversation_indexes_its_queued_prompts_once_through_the_watcher() {
        new_conversation_imports_each_prompt_once(Via::Watcher).await;
    }

    /// A conversation stored before queued prompts were indexed (and before the
    /// scrubber): old rows stay as stored, the prompts land as new rows, a second
    /// run adds none, and the conversation still resumes as a legacy one.
    async fn backfill_adds_only_new_rows(via: Via) {
        let rig = Rig::new();
        let engine = rig.engine();
        let src = rig.source("sess-old.jsonl");
        let conv = "sess-old".to_string();
        {
            // Stored by a build without the scrubber: no stamp on the import state.
            let _off = crate::redact::DisabledForTest::set(true);
            let body: String = (0..4)
                .map(|n| user(n, &format!("legacy message {n} about the importer")))
                .collect();
            fs::write(&src, body).unwrap();
            assert!(engine.import_file(&src, "proj").await.unwrap() >= 1);
        }
        let before = stored_rows(&engine, &conv);
        let bound = bind_to_a_witness(&engine, &conv);
        assert_eq!(bound.len(), before.len());
        // The prompts were typed in that session, but the build that stored it
        // never indexed them: the transcript holds them, the ledger does not.
        append(&src, &busy_session("ignored by the legacy build"));
        forget_scans(&engine);
        assert_eq!(
            scalar(&engine, "SELECT COUNT(*) FROM queued_prompt_chunks"),
            0
        );
        assert_eq!(engine.storage().queued_prompt_counts().unwrap().pending, 1);

        let stats = backfill(&engine, &rig, "", 50).await;
        assert_eq!(
            (stats.scanned, stats.added, stats.failed, stats.done),
            (1, 2, 0, true)
        );
        assert_eq!(chunks_like(&engine, "zebracrossing"), 1);
        assert_eq!(chunks_like(&engine, "quokkaparade"), 1);
        assert_eq!(chunks_like(&engine, "task-notification"), 0);
        assert!(found_by_search(&engine, "zebracrossing"));
        assert_eq!(
            stored_rows(&engine, &conv),
            before,
            "old rows are byte-identical"
        );
        assert_eq!(bindings(&engine), bound, "witness bindings are untouched");
        assert_eq!(
            scalar(
                &engine,
                "SELECT COUNT(*) FROM import_state WHERE secrets_redacted IS NOT NULL"
            ),
            0,
            "the backfill does not stamp a legacy conversation scrub-aware"
        );
        assert_eq!(
            scalar(&engine, "SELECT COUNT(*) FROM legacy_import_frontier"),
            0
        );
        let counts = engine.storage().queued_prompt_counts().unwrap();
        assert_eq!((counts.indexed, counts.scanned, counts.pending), (2, 1, 0));

        // A second run has nothing pending; with the record wiped it still adds
        // nothing, because the ledger knows the lines.
        let again = backfill(&engine, &rig, "", 50).await;
        assert_eq!((again.scanned, again.added), (0, 0));
        forget_scans(&engine);
        let rerun = backfill(&engine, &rig, "", 50).await;
        assert_eq!((rerun.scanned, rerun.added), (1, 0));
        assert_eq!(chunks_like(&engine, "zebracrossing"), 1);
        assert_eq!(engine.storage().queued_prompt_counts().unwrap().indexed, 2);

        // The conversation still resumes as a legacy one: the prompt rows do not
        // shift where it is placed, and the old rows stay untouched.
        let token = format!("{}{}", "ghp_", "Q".repeat(36));
        append(&src, &user(20, &format!("freshly appended {token}")));
        import_via(&engine, &rig, &src, via).await;
        assert_eq!(
            scalar(&engine, "SELECT COUNT(*) FROM legacy_import_frontier"),
            1,
            "{via:?}: the conversation was placed as a legacy one"
        );
        let after = stored_rows(&engine, &conv);
        assert_eq!(after.len(), before.len() + 1, "{via:?}");
        for old in &before {
            assert!(after.contains(old), "{via:?}: {old}");
        }
        assert_eq!(bindings(&engine), bound);
        assert_eq!(chunks_like(&engine, "zebracrossing"), 1, "{via:?}");
        assert_eq!(chunks_like(&engine, &token), 0, "{via:?}");
        // No message after the stored rows is skipped.
        assert_eq!(
            chunks_like(&engine, "working on it, reading the files"),
            1,
            "{via:?}"
        );
        assert_eq!(chunks_like(&engine, "and the second"), 1, "{via:?}");
        assert_eq!(chunks_like(&engine, "freshly appended"), 1, "{via:?}");
    }

    #[tokio::test]
    async fn the_backfill_adds_queued_prompts_to_a_legacy_conversation_through_the_engine() {
        backfill_adds_only_new_rows(Via::Engine).await;
    }

    #[tokio::test]
    async fn the_backfill_adds_queued_prompts_to_a_legacy_conversation_through_the_watcher() {
        backfill_adds_only_new_rows(Via::Watcher).await;
    }

    /// The same prompt as an enqueue row, an attachment and a later user row is
    /// one chunk: the user row's, already in the conversation.
    #[tokio::test]
    async fn one_prompt_written_three_ways_is_one_chunk() {
        let rig = Rig::new();
        let engine = rig.engine();
        let src = rig.source("sess-twin.jsonl");
        let text = "narwhalmigration is the thing to fix";
        fs::write(
            &src,
            [
                user(0, "start"),
                assistant(1, "ok"),
                enqueue(2, text),
                queued(2, text, Some(true)),
                assistant(3, "still going"),
                user(4, text),
                assistant(5, "fixing it"),
            ]
            .concat(),
        )
        .unwrap();
        engine.import_file(&src, "proj").await.unwrap();
        assert_eq!(chunks_like(&engine, "narwhalmigration"), 1);
        assert_eq!(engine.storage().queued_prompt_counts().unwrap().indexed, 0);

        // Without the later user row it is the queued copy that carries the text.
        let alone = rig.source("sess-alone.jsonl");
        fs::write(
            &alone,
            [
                user(0, "start"),
                assistant(1, "ok"),
                enqueue(2, text),
                queued(2, text, Some(true)),
                assistant(3, "on it"),
            ]
            .concat(),
        )
        .unwrap();
        engine.import_file(&alone, "proj").await.unwrap();
        assert_eq!(chunks_like(&engine, "narwhalmigration"), 2);
        assert_eq!(engine.storage().queued_prompt_counts().unwrap().indexed, 1);
    }

    /// Queued text takes the same sanitizer and secret scrub as a user row.
    #[tokio::test]
    async fn a_queued_prompt_is_scrubbed_like_a_user_row() {
        let rig = Rig::new();
        let engine = rig.engine();
        let src = rig.source("sess-secret.jsonl");
        let token = format!("{}{}", "ghp_", "Z".repeat(36));
        let text =
            format!("use {token} for the platypusdeploy and <private>hush-hush</private> done");
        fs::write(
            &src,
            [
                user(0, "start"),
                assistant(1, "ok"),
                queued(2, &text, Some(true)),
                assistant(3, "yes"),
            ]
            .concat(),
        )
        .unwrap();
        engine.import_file(&src, "proj").await.unwrap();
        assert_eq!(chunks_like(&engine, "platypusdeploy"), 1);
        assert_eq!(chunks_like(&engine, &token), 0);
        assert_eq!(chunks_like(&engine, "hush-hush"), 0);
        assert_eq!(chunks_like(&engine, "[secret redacted: github_token]"), 1);
    }

    /// The prompt's chunk points at its transcript line, and a long one is cut at
    /// the chunk budget: one ledger row per chunk, one prompt counted.
    #[tokio::test]
    async fn a_queued_prompt_chunk_points_at_its_line_and_long_ones_split() {
        let rig = Rig::new();
        let engine = rig.engine();
        let src = rig.source("sess-long.jsonl");
        let long = format!("{} {}", "wombatsprawl".repeat(40), "tail ".repeat(300));
        let head = [user(0, "start"), assistant(1, "ok")].concat();
        fs::write(
            &src,
            format!(
                "{head}{}{}",
                queued(2, &long, Some(true)),
                assistant(3, "yes")
            ),
        )
        .unwrap();
        engine.import_file(&src, "proj").await.unwrap();
        assert_eq!(engine.storage().queued_prompt_counts().unwrap().indexed, 1);
        let parts = scalar(&engine, "SELECT COUNT(*) FROM queued_prompt_chunks");
        assert!(parts >= 2, "{parts}");
        let (kind, receipt, offset): (String, String, i64) = engine
            .storage()
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT e.receipt_kind, e.receipt_ref, q.line_offset FROM queued_prompt_chunks q
                     JOIN chunk_spans s ON s.chunk_id = q.chunk_id
                     JOIN provenance_events e ON e.event_id = s.event_id WHERE q.part = 0",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?)
            })
            .unwrap();
        assert_eq!(kind, "jsonl");
        assert_eq!(offset as usize, head.len());
        assert!(
            receipt.ends_with(&format!("#byte={}", head.len())),
            "{receipt}"
        );
        let rejoined: String = engine
            .storage()
            .with_connection(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT c.content FROM chunks c JOIN queued_prompt_chunks q ON q.chunk_id = c.id
                     ORDER BY q.part",
                )?;
                let rows = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(rows.concat())
            })
            .unwrap();
        assert_eq!(
            rejoined.split_whitespace().collect::<String>(),
            long.split_whitespace().collect::<String>()
        );
        // Real receipts: the provenance backfill has nothing to reconstruct.
        assert_eq!(
            scalar(
                &engine,
                "SELECT COUNT(*) FROM chunks c JOIN queued_prompt_chunks q ON q.chunk_id = c.id
                 WHERE NOT EXISTS (SELECT 1 FROM chunk_spans s WHERE s.chunk_id = c.id)"
            ),
            0
        );
    }

    /// Make every write to the ledger fail, as a full disk or a crash would.
    fn break_ledger(engine: &Engine) {
        engine
            .storage()
            .with_connection(|c| {
                Ok(c.execute_batch(
                    "CREATE TRIGGER fail_ledger BEFORE INSERT ON queued_prompt_chunks
                     BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
                )?)
            })
            .unwrap();
    }

    fn mend_ledger(engine: &Engine) {
        engine
            .storage()
            .with_connection(|c| Ok(c.execute_batch("DROP TRIGGER fail_ledger;")?))
            .unwrap();
    }

    /// A transcript already scanned gains queued prompts, and indexing them fails
    /// on the first try. The old scan record must not survive that: the next
    /// backfill pass indexes the prompts.
    async fn failed_indexing_is_retried_by_the_backfill(via: Via) {
        let rig = Rig::new();
        let engine = rig.engine();
        let src = rig.source("sess-retry.jsonl");
        fs::write(&src, user(0, "the first thing asked of the importer")).unwrap();
        import_via(&engine, &rig, &src, via).await;
        let counts = engine.storage().queued_prompt_counts().unwrap();
        assert_eq!((counts.indexed, counts.scanned, counts.pending), (0, 1, 0));

        append(&src, &busy_session("ignored, already stored"));
        break_ledger(&engine);
        import_via(&engine, &rig, &src, via).await;
        assert_eq!(chunks_like(&engine, "zebracrossing"), 0, "{via:?}");
        let counts = engine.storage().queued_prompt_counts().unwrap();
        assert_eq!(
            (counts.indexed, counts.scanned, counts.pending),
            (0, 0, 1),
            "{via:?}: the failed conversation reads as unscanned"
        );

        mend_ledger(&engine);
        let stats = backfill(&engine, &rig, "", 50).await;
        assert_eq!(
            (stats.scanned, stats.added, stats.failed, stats.done),
            (1, 2, 0, true),
            "{via:?}"
        );
        assert_eq!(chunks_like(&engine, "zebracrossing"), 1, "{via:?}");
        assert_eq!(chunks_like(&engine, "quokkaparade"), 1, "{via:?}");
        assert!(found_by_search(&engine, "zebracrossing"), "{via:?}");
        let counts = engine.storage().queued_prompt_counts().unwrap();
        assert_eq!((counts.indexed, counts.scanned, counts.pending), (2, 1, 0));
    }

    #[tokio::test]
    async fn a_failed_queued_indexing_is_retried_by_the_backfill_after_an_engine_import() {
        failed_indexing_is_retried_by_the_backfill(Via::Engine).await;
    }

    #[tokio::test]
    async fn a_failed_queued_indexing_is_retried_by_the_backfill_after_a_watcher_import() {
        failed_indexing_is_retried_by_the_backfill(Via::Watcher).await;
    }

    /// Deleting a queued prompt's chunk takes its ledger row and the
    /// conversation's scan record with it, so the prompt can be indexed again.
    #[tokio::test]
    async fn deleting_a_queued_prompt_chunk_lets_it_be_indexed_again() {
        let rig = Rig::new();
        let engine = rig.engine();
        let src = rig.source("sess-del.jsonl");
        fs::write(&src, busy_session("the opening ask")).unwrap();
        engine.import_file(&src, "proj").await.unwrap();
        let counts = engine.storage().queued_prompt_counts().unwrap();
        assert_eq!((counts.indexed, counts.scanned), (2, 1));
        let id: String = engine
            .storage()
            .with_connection(|c| {
                Ok(c.query_row(
                    "SELECT c.id FROM chunks c JOIN queued_prompt_chunks q ON q.chunk_id = c.id
                     WHERE c.content LIKE '%zebracrossing%'",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();

        engine.storage().delete_chunk(&id).unwrap();
        assert_eq!(chunks_like(&engine, "zebracrossing"), 0);
        assert_eq!(
            scalar(
                &engine,
                &format!("SELECT COUNT(*) FROM queued_prompt_chunks WHERE chunk_id = '{id}'")
            ),
            0,
            "the ledger row went with the chunk"
        );
        let counts = engine.storage().queued_prompt_counts().unwrap();
        assert_eq!((counts.indexed, counts.scanned, counts.pending), (1, 0, 1));
        assert_eq!(
            chunks_like(&engine, "quokkaparade"),
            1,
            "the other prompt is untouched"
        );

        let stats = backfill(&engine, &rig, "", 50).await;
        assert_eq!((stats.scanned, stats.added, stats.failed), (1, 1, 0));
        assert_eq!(chunks_like(&engine, "zebracrossing"), 1);
        assert_eq!(chunks_like(&engine, "quokkaparade"), 1);
        let counts = engine.storage().queued_prompt_counts().unwrap();
        assert_eq!((counts.indexed, counts.scanned, counts.pending), (2, 1, 0));
    }

    /// Deleting one part of a prompt cut at the chunk budget takes the ledger rows of every
    /// part, or the surviving rows would keep the line looking indexed and the missing part
    /// would never come back.
    #[tokio::test]
    async fn deleting_one_part_of_a_long_queued_prompt_lets_every_part_be_indexed_again() {
        let rig = Rig::new();
        let engine = rig.engine();
        let src = rig.source("sess-parts.jsonl");
        let long = format!("{} {}", "wombatsprawl".repeat(40), "tail ".repeat(300));
        let head = [user(0, "start"), assistant(1, "ok")].concat();
        fs::write(
            &src,
            format!(
                "{head}{}{}",
                queued(2, &long, Some(true)),
                assistant(3, "yes")
            ),
        )
        .unwrap();
        engine.import_file(&src, "proj").await.unwrap();
        let parts = scalar(&engine, "SELECT COUNT(*) FROM queued_prompt_chunks");
        assert!(parts >= 2, "{parts}");
        let part_ids: Vec<String> = engine
            .storage()
            .with_connection(|c| {
                let mut stmt = c.prepare("SELECT chunk_id FROM queued_prompt_chunks")?;
                let ids = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(ids)
            })
            .unwrap();
        let part_one: String = engine
            .storage()
            .with_connection(|c| {
                Ok(c.query_row(
                    "SELECT chunk_id FROM queued_prompt_chunks WHERE part = 1",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();

        engine.storage().delete_chunk(&part_one).unwrap();
        assert_eq!(
            scalar(&engine, "SELECT COUNT(*) FROM queued_prompt_chunks"),
            0,
            "every part's ledger row went with the deleted part"
        );
        let ids = part_ids
            .iter()
            .map(|id| format!("'{id}'"))
            .collect::<Vec<_>>();
        assert_eq!(
            scalar(
                &engine,
                &format!(
                    "SELECT COUNT(*) FROM chunks WHERE id IN ({})",
                    ids.join(",")
                )
            ),
            parts - 1,
            "the other parts' chunk rows stay"
        );

        let stats = backfill(&engine, &rig, "", 50).await;
        assert_eq!((stats.scanned, stats.added, stats.failed), (1, 1, 0));
        assert_eq!(
            scalar(&engine, "SELECT COUNT(*) FROM queued_prompt_chunks"),
            parts
        );
        assert_eq!(
            scalar(
                &engine,
                "SELECT COUNT(*) FROM queued_prompt_chunks q JOIN chunks c ON c.id = q.chunk_id"
            ),
            parts,
            "every ledger row has its chunk"
        );
        assert_eq!(
            scalar(
                &engine,
                &format!("SELECT COUNT(*) FROM chunks WHERE id = '{part_one}'")
            ),
            1,
            "the deleted part is back"
        );
        assert_eq!(
            scalar(
                &engine,
                "SELECT COUNT(*) FROM (SELECT chunk_id FROM queued_prompt_chunks
                  GROUP BY chunk_id HAVING COUNT(*) > 1)"
            ),
            0,
            "one ledger row per chunk"
        );
        let counts = engine.storage().queued_prompt_counts().unwrap();
        assert_eq!((counts.indexed, counts.scanned, counts.pending), (1, 1, 0));
    }

    /// Rows other adapters keep in import_state, and transcripts that are gone or
    /// mention no queued command, are marked read without a parse.
    #[tokio::test]
    async fn the_backfill_marks_what_it_cannot_use_as_read() {
        let rig = Rig::new();
        let engine = rig.engine();
        let plain = rig.source("sess-plain.jsonl");
        fs::write(
            &plain,
            [user(0, "hello there"), assistant(1, "hi")].concat(),
        )
        .unwrap();
        engine.import_file(&plain, "proj").await.unwrap();
        let gone = rig.source("sess-gone.jsonl");
        fs::write(&gone, user(0, "bye")).unwrap();
        engine.import_file(&gone, "proj").await.unwrap();
        fs::remove_file(&gone).unwrap();
        engine
            .storage()
            .mark_file_imported(Path::new("plan:some-plan"), 3)
            .unwrap();
        forget_scans(&engine);
        assert_eq!(engine.storage().queued_prompt_counts().unwrap().pending, 3);
        let first = backfill(&engine, &rig, "", 2).await;
        assert!(!first.done);
        let rest = backfill(&engine, &rig, &first.last_path, 2).await;
        assert!(rest.done);
        let counts = engine.storage().queued_prompt_counts().unwrap();
        assert_eq!((counts.indexed, counts.pending), (0, 0));
    }

    #[test]
    fn pairing_takes_each_user_row_once_and_in_order() {
        let rows: HashMap<String, Vec<usize>> = [
            ("go".to_string(), vec![3, 7]),
            ("stop".to_string(), vec![1]),
        ]
        .into();
        let prompts = vec![
            (2, Some("go".to_string())),
            (2, Some("go".to_string())),
            (2, Some("go".to_string())),
            (2, Some("stop".to_string())), // its row sits before it
            (0, None),
        ];
        assert_eq!(
            pair_with_rows(&prompts, &rows),
            vec![Some(3), Some(7), None, None, None]
        );
    }
}
