//! Shared incremental transcript import.
//!
//! The daemon's `FileWatcher` and `Engine::import_file` both land here. They used
//! to carry near-identical copies of this routine and drifted: `engine.rs` grew an
//! incremental guard, `watcher.rs` never did, and the daemon spent eight days
//! re-embedding settled history at 250% CPU.
//!
//! Two invariants make incremental import safe:
//!
//! 1. **Chunk boundaries are stable under append.** The chunker is a greedy
//!    left-to-right fold with no lookahead — each flush decision reads only the
//!    current buffer and message length. So `c0..c_{k-1}` are a pure function of
//!    the message prefix. Only the final EOF-flushed chunk is mutable; every
//!    earlier chunk is frozen once written. That is why rebuilding from
//!    `chunks_imported - 1` is both necessary and sufficient.
//!
//! 2. **A chunk's vector must be written when its content is final.**
//!    [`SearchEngine::insert_chunk`] is a no-op for an id already in the index,
//!    so indexing the still-growing trailing chunk freezes a vector representing
//!    only its first fragment. [`SealPolicy::DeferTrailing`] holds it back.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;

use crate::embeddings::EmbeddingEngine;
use crate::import::{self, ConversationAttribution, ParseCursor, PARSE_CURSOR_VERSION};
use crate::search::SearchEngine;
use crate::storage::Storage;

/// Batch size for embedding. Matches the per-caller constants this replaced.
const BATCH_SIZE: usize = 10;

/// Borrowed handles the import needs. Grouped so callers pass one thing.
pub(crate) struct ImportContext<'a> {
    pub storage: &'a Arc<Storage>,
    pub embeddings: &'a Arc<EmbeddingEngine>,
    pub search: &'a Arc<RwLock<SearchEngine>>,
    pub index_state: IndexState,
}

/// Whether the in-memory vector index the caller handed us describes the corpus.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum IndexState {
    /// The HNSW cache was loaded, so `has_chunk` is meaningful and an insert
    /// lands in the index this process searches and may dump.
    Live,
    /// The process skipped loading the cache. `Engine::new_import_only` (the
    /// `precompact` and `session-end` hooks) starts from an empty index that is
    /// never dumped, so `has_chunk` answers false for every id and an insert is
    /// discarded at exit. The plan must not read anything into that.
    Detached,
}

/// Whether the trailing (still-growing) chunk may enter the vector index.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SealPolicy {
    /// Live watch: keep the trailing chunk out of HNSW until it stops growing.
    /// Its content still reaches SQLite and FTS immediately.
    DeferTrailing,
    /// Hook / bulk import: the transcript is final, index everything.
    SealAll,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ImportOutcome {
    /// Chunks whose vector was written to HNSW this pass.
    pub indexed_chunks: usize,
    /// Chunks whose content was (re)written to SQLite this pass.
    pub written_chunks: usize,
    /// `chunks.len()` — what went to `import_state.chunks_imported`.
    pub total_chunks: usize,
    /// No previously-imported chunks; this was the conversation's first import.
    pub first_import: bool,
    /// A rewrite was detected and the conversation was wiped and rebuilt.
    pub full_reimport: bool,
    /// The mtime gate matched — the file was not re-read at all.
    pub unchanged: bool,
}

/// What a single chunk needs this pass.
struct ChunkPlan {
    index: usize,
    /// Content differs from what is stored (or nothing is stored).
    write: bool,
    /// This chunk's vector should be in HNSW when we are done.
    index_it: bool,
    /// An id is already in HNSW and must be blanked before reinsertion.
    remove_first: bool,
}

/// Import a transcript, embedding only what actually changed.
///
/// Returns immediately when the file's mtime matches the last import.
pub(crate) async fn import_file_incremental(
    ctx: &ImportContext<'_>,
    file_path: &Path,
    attribution: &ConversationAttribution,
    seal: SealPolicy,
) -> Result<ImportOutcome> {
    // A pass that can seal owes the trailing chunk its vector whenever the last
    // pass deferred it. Checking this before the mtime gate is the whole point:
    // the watcher imports a live transcript with `DeferTrailing` and records the
    // mtime, so once the session stops writing, every later `SealAll` matched the
    // mtime and returned `unchanged` without ever looking at the seal policy. The
    // conversation's last chunk, usually its conclusion, then stayed out of the
    // running index for the life of the process.
    let owes_seal = seal == SealPolicy::SealAll
        && ctx.index_state == IndexState::Live
        && !ctx.storage.is_trailing_chunk_sealed(file_path)?;

    // Cheap gate: nothing on disk changed since the last pass.
    if ctx.storage.is_file_imported(file_path)? && !owes_seal {
        return Ok(ImportOutcome {
            unchanged: true,
            ..Default::default()
        });
    }

    let conversation_id = file_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    // Read before parsing, not at commit. A live session's writer can append
    // between the parser reaching EOF and the row being written, and an mtime
    // read at that point describes bytes nothing has read. The gate above would
    // then skip them until the next append moved the mtime again, and if the
    // session ended there they would never be imported at all. Recording the
    // earlier value makes a racing append read as changed, which costs one cheap
    // cursor resume.
    let mtime_before_parse = Storage::current_file_mtime(file_path);

    // Resume from the stored byte cursor when it still describes this file.
    let mut stored_cursor = ctx
        .storage
        .get_parse_cursor(file_path)?
        .and_then(|json| serde_json::from_str::<ParseCursor>(&json).ok())
        .filter(|c| c.v == PARSE_CURSOR_VERSION && cursor_still_valid(c, file_path));

    let mut parsed = import::parse_jsonl_file_from_cursor(
        file_path,
        &attribution.project_name,
        stored_cursor.as_ref(),
    )?;

    // A resume that produced nothing is not evidence the transcript is empty --
    // the prefix the cursor skipped over is still on disk. Recording zero chunks
    // and clearing the cursor reported an imported conversation as unimported,
    // and the next changed-file pass then rebuilt it from scratch and called it a
    // first import. Re-read from the head instead: only a full parse can tell a
    // genuinely empty transcript from a resume that landed badly.
    if parsed.chunks.is_empty() && stored_cursor.is_some() {
        tracing::debug!(
            file = %file_path.display(),
            "cursor resume produced no chunks — retrying with a full parse"
        );
        stored_cursor = None;
        parsed = import::parse_jsonl_file_from_cursor(file_path, &attribution.project_name, None)?;
    }

    let suppression = parsed.suppression;
    let next_cursor = parsed.next_cursor;
    let chunks = parsed.chunks;
    // With a cursor the parse starts mid-file, so chunks[0] is chunk number
    // `first_index`, not chunk zero.
    let first_index = stored_cursor.as_ref().map(|c| c.chunk_index).unwrap_or(0);

    // Agent transcripts and empty conversations parse to nothing. Record the skip so
    // the file is not re-parsed on every pass and import_percent counts it.
    if chunks.is_empty() {
        ctx.storage
            .mark_file_imported_with_suppression(file_path, 0, suppression)?;
        return Ok(ImportOutcome::default());
    }

    let n = first_index + chunks.len();
    let prev_count = ctx.storage.get_imported_chunk_count(file_path)?;

    // ── Decide where to resume ────────────────────────────────────────────────
    //
    // Fewer chunks than last time means either a genuine rewrite or a transient
    // short read (a concurrent writer's trailing line is incomplete and gets
    // skipped). Wiping is destructive, so it needs corroboration from the stored
    // content: a rewrite changes it; a short read does not.
    let mut full_reimport = false;
    let rebuild_from = if stored_cursor.is_some() {
        // Everything the cursor handed back begins at the seam by construction,
        // and the cursor was only trusted after its head fingerprint matched.
        first_index
    } else {
        // Full parse, so the whole prefix is in hand. Walk it against what is
        // stored and rebuild from the first chunk that no longer matches.
        //
        // Comparing chunk zero alone was not enough. A compaction or an interior
        // edit that leaves the head and the length alone reads as a plain append
        // under that test, so the stale middle stays in place and keeps matching
        // content the transcript no longer holds. The cursor's 4 KiB head
        // fingerprint has the same blind spot, which is why the check lives here,
        // on the path a rejected cursor also falls back to.
        //
        // The last stored chunk is excluded on purpose: it was flushed partial at
        // EOF, so growth there is an append, not a rewrite. The walk costs one
        // primary-key lookup per frozen chunk, but only on the full-parse path --
        // a first import compares nothing, and every later pass resumes from the
        // cursor and never reaches here.
        let frozen = chunks.len().min(prev_count.saturating_sub(1));
        let mut first_stale = None;
        for (i, chunk) in chunks.iter().enumerate().take(frozen) {
            let intact = ctx
                .storage
                .get_chunk_content(&chunk.id)?
                .is_some_and(|stored| stored == chunk.content);
            if !intact {
                first_stale = Some(i);
                break;
            }
        }

        match first_stale {
            Some(0) => {
                // The head itself moved, so nothing of the old conversation can
                // be trusted. Wipe rather than leave a stale prefix behind.
                tracing::warn!(
                    conv = %conversation_id,
                    previous = prev_count,
                    current = n,
                    "transcript rewritten — wiping and rebuilding the conversation"
                );
                let old_ids = ctx
                    .storage
                    .get_chunk_ids_for_conversation(&conversation_id)?;
                ctx.storage
                    .delete_chunks_for_conversation(&conversation_id)?;
                {
                    let mut idx = ctx.search.write().await;
                    for id in &old_ids {
                        idx.remove_chunk(id);
                    }
                }
                full_reimport = true;
                0
            }
            Some(from) => {
                // The head survived but the middle did not. Keep the matching
                // prefix and rebuild everything from the first stale chunk on.
                tracing::warn!(
                    conv = %conversation_id,
                    previous = prev_count,
                    current = n,
                    from,
                    "transcript rewritten below the head — rebuilding from the first stale chunk"
                );
                drop_orphan_chunks(ctx, &conversation_id, n..prev_count).await?;
                from
            }
            None if n < prev_count => {
                // Prefix intact but fewer chunks: keep it and drop the orphan
                // tail rather than wiping a conversation needlessly.
                tracing::warn!(
                    conv = %conversation_id,
                    previous = prev_count,
                    current = n,
                    "transcript shrank with an intact head — dropping orphan tail chunks"
                );
                drop_orphan_chunks(ctx, &conversation_id, n..prev_count).await?;
                n.saturating_sub(1)
            }
            None => {
                // The seam. `prev_count` counts chunks WRITTEN, and the last of
                // those was a partial buffer flushed at EOF — on this pass it may
                // have grown, so it must be rebuilt. Slicing from `prev_count`
                // instead drops its new messages into no chunk at all.
                prev_count.saturating_sub(1)
            }
        }
    };

    // ── Plan: what actually needs work ───────────────────────────────────────
    // A detached index is thrown away when the process exits, so a vector put
    // into it helps nobody, and `has_chunk` answers false for every id. Planning
    // off that would embed every chunk of the transcript on every `precompact`
    // and `session-end` hook — the exact cost this module exists to remove.
    // Content and its embedding still reach SQLite, and the next process that
    // loads the index picks them up through the additive backfill in
    // `Engine::new`.
    let detached = ctx.index_state == IndexState::Detached;
    let mut plans: Vec<ChunkPlan> = Vec::new();
    {
        let idx = ctx.search.read().await;
        for (local, chunk) in chunks.iter().enumerate() {
            let i = first_index + local;
            if i < rebuild_from {
                continue;
            }
            let is_trailing = i + 1 == n;
            let index_it = !detached && (!is_trailing || seal == SealPolicy::SealAll);
            let indexed = !detached && idx.has_chunk(&chunk.id);

            let content_same = ctx
                .storage
                .get_chunk_content(&chunk.id)?
                .is_some_and(|stored| stored == chunk.content);

            // Nothing to do when the content is already stored and the index
            // state is what we want. This is what keeps timestamps frozen: an
            // untouched chunk is never rewritten, so it keeps its original stamp.
            if content_same && (indexed || !index_it) {
                continue;
            }

            plans.push(ChunkPlan {
                index: local,
                write: !content_same,
                index_it: index_it && (!indexed || !content_same),
                remove_first: indexed,
            });
        }
    }

    let mut indexed_chunks = 0usize;
    let mut written_chunks = 0usize;

    // ── Execute ──────────────────────────────────────────────────────────────
    for batch in plans.chunks(BATCH_SIZE) {
        let texts: Vec<String> = batch
            .iter()
            .map(|p| chunks[p.index].content.clone())
            .collect();
        let emb = ctx.embeddings.clone();
        let embeddings = tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
            emb.embed(&refs)
        })
        .await??;

        // Taken per batch, not across the whole import: holding it for a 5,000-chunk
        // file starved MCP searches behind the watcher.
        let mut idx = ctx.search.write().await;
        for (plan, embedding) in batch.iter().zip(embeddings) {
            let chunk = &chunks[plan.index];

            if plan.write {
                ctx.storage
                    .insert_chunk_with_source(chunk, &embedding, attribution.source)?;
                if let Err(error) = ctx.storage.insert_chunk_provenance(
                    &chunk.id,
                    &crate::provenance::ChunkProvenance {
                        author: chunk.author,
                        source_conv_id: attribution
                            .parent_conversation_id
                            .clone()
                            .unwrap_or_else(|| chunk.conversation_id.clone()),
                        supersedes: None,
                    },
                ) {
                    tracing::warn!(error = %error, chunk = %chunk.id, "chunk provenance persist failed");
                }
                written_chunks += 1;
            }

            if plan.index_it {
                // insert_chunk is a no-op for a known id, so a changed chunk must
                // be blanked first or its stale vector survives forever.
                if plan.remove_first {
                    idx.remove_chunk(&chunk.id);
                }
                idx.insert_chunk(chunk.id.clone(), embedding);
                indexed_chunks += 1;
            } else if plan.remove_first {
                // Content moved on but this chunk is not eligible for the index
                // yet (still growing). Blank the stale vector rather than leave it
                // matching text the chunk no longer contains.
                idx.remove_chunk(&chunk.id);
            }
        }
    }

    let cursor_json = next_cursor
        .as_ref()
        .and_then(|c| serde_json::to_string(c).ok());
    // The trailing chunk is settled only if this pass could actually put its
    // vector somewhere that outlives the pass. `DeferTrailing` held it back on
    // purpose; a detached index throws away everything inserted into it. Either
    // way the debt is recorded so the next sealing pass bypasses the mtime gate.
    let trailing_sealed = seal == SealPolicy::SealAll && ctx.index_state == IndexState::Live;
    ctx.storage.mark_file_imported_with_cursor(
        file_path,
        n,
        suppression,
        cursor_json.as_deref(),
        trailing_sealed,
        Some(&mtime_before_parse),
    )?;

    Ok(ImportOutcome {
        indexed_chunks,
        written_chunks,
        total_chunks: n,
        first_import: prev_count == 0,
        full_reimport,
        unchanged: false,
    })
}

/// Drop stored chunks the transcript no longer has, from both SQLite and the
/// vector index. An empty range is a no-op.
async fn drop_orphan_chunks(
    ctx: &ImportContext<'_>,
    conversation_id: &str,
    range: std::ops::Range<usize>,
) -> Result<()> {
    if range.is_empty() {
        return Ok(());
    }
    let orphans: Vec<String> = range
        .map(|i| import::generate_chunk_id(conversation_id, i))
        .collect();
    ctx.storage.delete_chunks_by_ids(&orphans)?;
    let mut idx = ctx.search.write().await;
    for id in &orphans {
        idx.remove_chunk(id);
    }
    Ok(())
}

/// Whether a stored cursor still describes the file on disk.
///
/// A shorter file means truncation. A changed head means the file was rewritten,
/// which a length check alone misses when the rewrite happens to be as long or
/// longer. Either way the offset is meaningless and the caller falls back to a
/// full parse.
fn cursor_still_valid(cursor: &ParseCursor, path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if meta.len() < cursor.file_len || meta.len() < cursor.byte_offset {
        return false;
    }
    // An edit below the 4 KiB the fingerprint covers is invisible to it, but any
    // edit that changes the length of the region it touches moves every byte
    // after it, so the resume offset no longer starts a line.
    if !import::resumes_on_a_line_boundary(path, cursor.byte_offset) {
        return false;
    }
    import::head_fingerprint(path) == cursor.head_fingerprint
}

/// Layer 1 heuristic enrichment, shared by both callers.
///
/// `engine.rs` used to gate this on `prev_count == 0`, which permanently stranded
/// any conversation whose first attempt failed. `watcher.rs` had no gate at all,
/// so a persistently-failing conversation re-read the whole transcript every
/// debounce. Gate on "this pass did work" and let the existing
/// `is_conversation_enriched` check provide idempotence.
pub(crate) async fn maybe_enrich(
    ctx: &ImportContext<'_>,
    outcome: &ImportOutcome,
    file_path: &Path,
    attribution: &ConversationAttribution,
) {
    if outcome.unchanged || (!outcome.first_import && outcome.written_chunks == 0) {
        return;
    }
    let conv_id = file_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    if ctx
        .storage
        .is_conversation_enriched(&conv_id, "heuristic")
        .unwrap_or(false)
    {
        return;
    }
    if let Err(e) = crate::extraction::heuristic::enrich_conversation(
        file_path,
        &conv_id,
        &attribution.project_name,
        ctx.storage,
        ctx.embeddings,
        ctx.search,
    )
    .await
    {
        tracing::warn!(conv = %conv_id, error = %e, "heuristic enrichment failed (non-fatal)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use tempfile::TempDir;

    /// The embedding model is expensive to load; share one across the module.
    fn embeddings() -> Arc<EmbeddingEngine> {
        static ENGINE: OnceLock<Arc<EmbeddingEngine>> = OnceLock::new();
        ENGINE
            .get_or_init(|| Arc::new(EmbeddingEngine::new().expect("embedding model")))
            .clone()
    }

    struct Harness {
        dir: TempDir,
        path: PathBuf,
        storage: Arc<Storage>,
        embeddings: Arc<EmbeddingEngine>,
        search: Arc<RwLock<SearchEngine>>,
        index_state: std::cell::Cell<IndexState>,
    }

    impl Harness {
        fn new(name: &str) -> Self {
            let dir = TempDir::new().unwrap();
            let path = dir.path().join(format!("{name}.jsonl"));
            Self {
                dir,
                path,
                storage: Arc::new(Storage::open_memory().unwrap()),
                embeddings: embeddings(),
                search: Arc::new(RwLock::new(SearchEngine::new(256))),
                index_state: std::cell::Cell::new(IndexState::Live),
            }
        }

        fn ctx(&self) -> ImportContext<'_> {
            ImportContext {
                storage: &self.storage,
                embeddings: &self.embeddings,
                search: &self.search,
                index_state: self.index_state.get(),
            }
        }

        /// Stand in for a write-only hook process: the same database, an index
        /// that was never loaded and will never be dumped.
        async fn detach_index(&self) {
            self.index_state.set(IndexState::Detached);
            *self.search.write().await = SearchEngine::new(256);
        }

        fn conv_id(&self) -> String {
            self.path.file_stem().unwrap().to_string_lossy().to_string()
        }

        fn chunk_id(&self, i: usize) -> String {
            import::generate_chunk_id(&self.conv_id(), i)
        }

        /// Write `msgs` as a fresh transcript. Alternates user/assistant.
        fn write(&self, msgs: &[String]) {
            let lines: Vec<String> = msgs
                .iter()
                .enumerate()
                .map(|(i, text)| {
                    serde_json::json!({
                        "type": if i.is_multiple_of(2) { "user" } else { "assistant" },
                        "timestamp": format!("2026-02-22T10:00:{:02}Z", i),
                        "message": {"content": [{"type": "text", "text": text}]}
                    })
                    .to_string()
                })
                .collect();
            std::fs::write(&self.path, lines.join("\n")).unwrap();
            // mtime must differ from the previous write or the import short-circuits.
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        /// Write a transcript verbatim, for content no `String` can hold.
        fn write_bytes(&self, bytes: &[u8]) {
            std::fs::write(&self.path, bytes).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        async fn import(&self, seal: SealPolicy) -> ImportOutcome {
            let attribution = ConversationAttribution {
                project_name: "test".to_string(),
                source: "conversation",
                parent_conversation_id: None,
            };
            import_file_incremental(&self.ctx(), &self.path, &attribution, seal)
                .await
                .unwrap()
        }

        fn stored(&self, i: usize) -> Option<String> {
            self.storage.get_chunk_content(&self.chunk_id(i)).unwrap()
        }

        fn timestamp(&self, i: usize) -> String {
            self.storage
                .get_chunks_by_ids(&[self.chunk_id(i)])
                .unwrap()
                .first()
                .expect("chunk must exist")
                .timestamp
                .clone()
        }

        fn embed(&self, text: &str) -> Vec<f32> {
            self.embeddings.embed_single(text).unwrap()
        }

        /// This chunk's cosine score against `query`, or `None` when the index
        /// does not answer for it at all.
        fn score_of(index: &SearchEngine, query: &[f32], chunk_id: &str) -> Option<f32> {
            index
                .search_chunks(query, 16, -1.0)
                .into_iter()
                .find(|r| r.id == chunk_id)
                .map(|r| r.score)
        }

        /// Every stored chunk of this conversation, by index.
        fn all_stored(&self) -> Vec<String> {
            let mut out = Vec::new();
            let mut i = 0;
            while let Some(c) = self.stored(i) {
                out.push(c);
                i += 1;
            }
            out
        }
    }

    /// `n` messages of ~400 chars, so exactly two fit in a 900-char chunk.
    fn msgs(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| format!("MSG{i:03}-{}", "x".repeat(390)))
            .collect()
    }

    /// ~400 chars on one subject, so the message's embedding is dominated by
    /// that subject and a query for a different one separates them.
    fn topical(subject: &str) -> String {
        let mut out = String::new();
        while out.len() < 390 {
            out.push_str(subject);
            out.push_str(". ");
        }
        out
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().unwrap()
    }

    /// The off-by-one. Slicing from `prev_count` skips the trailing partial chunk,
    /// so messages that grew into it land in no stored chunk at all.
    #[test]
    fn seam_chunk_is_rebuilt_when_transcript_grows() {
        rt().block_on(async {
            let h = Harness::new("seam-grow");
            // 5 messages -> c0(m0,m1) c1(m2,m3) c2(m4, partial)
            h.write(&msgs(5));
            let first = h.import(SealPolicy::DeferTrailing).await;
            assert_eq!(first.total_chunks, 3, "fixture must produce 3 chunks");
            assert!(h.stored(2).unwrap().contains("MSG004"));
            assert!(!h.stored(2).unwrap().contains("MSG005"));

            // Grow: c2 becomes (m4,m5) and c3(m6) appears.
            h.write(&msgs(7));
            h.import(SealPolicy::DeferTrailing).await;

            let c2 = h.stored(2).expect("chunk 2 must still exist");
            assert!(
                c2.contains("MSG005"),
                "MSG005 grew into the seam chunk but was never rewritten -- \
                 this is the content the old &chunks[prev_count..] slice dropped"
            );
            // And it must not have been lost anywhere else either.
            let all = h.all_stored().join("\n");
            for i in 0..7 {
                assert!(all.contains(&format!("MSG{i:03}")), "MSG{i:03} missing");
            }
        });
    }

    /// Same loss, via the `chunks.len() <= prev_count` early return: the file grew
    /// but stayed under the budget, so the chunk count did not move.
    #[test]
    fn seam_chunk_updated_when_chunk_count_unchanged() {
        rt().block_on(async {
            let h = Harness::new("seam-same-count");
            h.write(&msgs(5));
            let first = h.import(SealPolicy::DeferTrailing).await;
            assert_eq!(first.total_chunks, 3);

            // Append a short message: c2 grows but no new chunk is created.
            let mut grown = msgs(5);
            grown.push("SHORTTAIL".to_string());
            h.write(&grown);
            let second = h.import(SealPolicy::DeferTrailing).await;

            assert_eq!(second.total_chunks, 3, "chunk count must be unchanged");
            assert!(
                h.stored(2).unwrap().contains("SHORTTAIL"),
                "a grown seam must be rewritten even when the chunk count is flat"
            );
        });
    }

    /// The property the whole design rests on: importing a transcript in N growing
    /// steps must land the same content as importing it once at full size.
    #[test]
    fn full_parse_and_incremental_parse_agree() {
        rt().block_on(async {
            let one_shot = Harness::new("agree");
            one_shot.write(&msgs(11));
            one_shot.import(SealPolicy::SealAll).await;

            let stepwise = Harness::new("agree");
            for n in [3usize, 5, 7, 9, 11] {
                stepwise.write(&msgs(n));
                stepwise.import(SealPolicy::SealAll).await;
            }

            assert_eq!(
                one_shot.all_stored(),
                stepwise.all_stored(),
                "incremental import must converge on the full-parse result"
            );
        });
    }

    /// Same property with a message larger than CHUNK_CHAR_BUDGET, which takes the
    /// hard-split branch and ends on a boundary with an empty buffer.
    #[test]
    fn hard_split_message_preserves_prefix_stability() {
        rt().block_on(async {
            let big = format!("BIG-{}", "y".repeat(2500));
            let mut base = msgs(3);
            base.push(big);

            let one_shot = Harness::new("hardsplit");
            let mut full = base.clone();
            full.extend(msgs(2));
            one_shot.write(&full);
            one_shot.import(SealPolicy::SealAll).await;

            let stepwise = Harness::new("hardsplit");
            stepwise.write(&base);
            stepwise.import(SealPolicy::SealAll).await;
            stepwise.write(&full);
            stepwise.import(SealPolicy::SealAll).await;

            assert_eq!(one_shot.all_stored(), stepwise.all_stored());
        });
    }

    /// A rewritten (shorter, different) transcript wipes the conversation instead
    /// of leaving orphan tail chunks matching content that no longer exists.
    #[test]
    fn truncated_transcript_triggers_full_reimport() {
        rt().block_on(async {
            let h = Harness::new("truncate");
            h.write(&msgs(7));
            let first = h.import(SealPolicy::SealAll).await;
            assert!(first.total_chunks >= 4);

            // Entirely different, shorter content.
            let replacement: Vec<String> = (0..3)
                .map(|i| format!("NEW{i:03}-{}", "z".repeat(390)))
                .collect();
            h.write(&replacement);
            let second = h.import(SealPolicy::SealAll).await;

            assert!(
                second.full_reimport,
                "a changed head must force a full wipe"
            );
            let all = h.all_stored();
            assert_eq!(
                all.len(),
                second.total_chunks,
                "no orphan tail chunks may survive the rewrite"
            );
            assert!(
                !all.join("\n").contains("MSG006"),
                "old content must be gone"
            );
        });
    }

    /// Under DeferTrailing the still-growing chunk reaches SQLite but not HNSW,
    /// because insert_chunk is a no-op for a known id and would freeze a vector
    /// representing only the chunk's first fragment.
    #[test]
    fn trailing_chunk_deferred_until_sealed() {
        rt().block_on(async {
            let h = Harness::new("defer");
            h.write(&msgs(5));
            h.import(SealPolicy::DeferTrailing).await;

            let trailing = h.chunk_id(2);
            assert!(
                h.stored(2).is_some(),
                "content must still reach SQLite immediately"
            );
            assert!(
                !h.search.read().await.has_chunk(&trailing),
                "the growing chunk must stay out of the vector index"
            );

            // Growing past it seals c2; c3 becomes the new trailing chunk.
            h.write(&msgs(7));
            h.import(SealPolicy::DeferTrailing).await;
            assert!(
                h.search.read().await.has_chunk(&trailing),
                "a sealed chunk must be indexed"
            );
            assert!(!h.search.read().await.has_chunk(&h.chunk_id(3)));
        });
    }

    /// Regression for the discarded-vector bug: re-inserting a changed chunk under
    /// its existing id is silently skipped, so the seam must be removed first.
    ///
    /// Asserted against the vector, not against `has_chunk`: the id is present
    /// either way, so an id check passes while the slot still holds the vector of
    /// the seam's first fragment. The discriminator is a query for a subject that
    /// only appears in the appended half, scored before and after the append. If
    /// the stale vector survives, the two scores are identical.
    #[test]
    fn sealed_seam_replaces_stale_vector() {
        rt().block_on(async {
            let h = Harness::new("stale-vector");
            // c0(m0,m1) c1(m2,m3) c2(m4).
            let before = vec![
                topical("quarterly revenue forecast spreadsheet"),
                topical("quarterly revenue forecast spreadsheet"),
                topical("database migration rollback plan"),
                topical("database migration rollback plan"),
                topical("espresso machine descaling procedure"),
            ];
            h.write(&before);
            // SealAll indexes the partial c2 straight away -- the situation the
            // hook path creates and DeferTrailing avoids.
            h.import(SealPolicy::SealAll).await;
            let seam = h.chunk_id(2);
            assert!(h.search.read().await.has_chunk(&seam));

            let query = h.embed("kayak paddle feathering angle");
            let stale = Harness::score_of(&*h.search.read().await, &query, &seam)
                .expect("the seam must be in the index before it grows");

            // c2 grows to (m4,m5), so its content now covers a subject its stored
            // vector has never seen. m6 becomes c3.
            let mut after = before.clone();
            after.push(topical("kayak paddle feathering angle"));
            after.push(topical("sourdough starter hydration ratio"));
            h.write(&after);
            let second = h.import(SealPolicy::SealAll).await;

            assert!(
                second.indexed_chunks > 0,
                "the grown seam must be re-indexed, not skipped"
            );
            assert!(
                h.stored(2).unwrap().contains("kayak"),
                "the appended message must be in the seam chunk's content"
            );

            let fresh = Harness::score_of(&*h.search.read().await, &query, &seam)
                .expect("the seam must still be reachable after the rewrite");
            assert!(
                fresh > stale + 0.1,
                "the seam's vector still represents only its first fragment \
                 (stale {stale}, after the append {fresh})"
            );

            // The same must hold for the next process, which comes up on the
            // dumped cache rather than on this in-memory index.
            let index_dir = h.dir.path().join("index");
            let db_chunks = h.storage.count_chunk_embeddings().unwrap();
            h.search
                .write()
                .await
                .dump_to_disk(&index_dir, db_chunks, 0)
                .expect("dump");
            let reloaded =
                SearchEngine::load_from_disk(&index_dir, db_chunks, 0).expect("cache must reload");
            let after_reload = Harness::score_of(&reloaded, &query, &seam)
                .expect("the seam must survive a dump and reload");
            assert!(
                (after_reload - fresh).abs() < 1e-3,
                "the reloaded cache must carry the rewritten vector, not the \
                 stale one (in memory {fresh}, reloaded {after_reload})"
            );
        });
    }

    /// Comparing chunk zero alone read an interior rewrite as a plain append, so
    /// the stale middle stayed in place matching content the transcript no longer
    /// held. This is the full-parse path, which a rejected or absent cursor falls
    /// back to.
    #[test]
    fn interior_rewrite_rebuilds_from_the_first_stale_chunk() {
        rt().block_on(async {
            let h = Harness::new("interior");
            // c0(m0,m1) c1(m2,m3) c2(m4,m5) c3(m6,m7) c4(m8)
            h.write(&msgs(9));
            let first = h.import(SealPolicy::SealAll).await;
            assert_eq!(first.total_chunks, 5, "fixture must produce 5 chunks");

            // Rewrite chunk 2's two messages at identical lengths, so neither the
            // head nor the file size moves.
            let mut edited = msgs(9);
            edited[4] = format!("EDT004-{}", "x".repeat(390));
            edited[5] = format!("EDT005-{}", "x".repeat(390));
            h.write(&edited);
            h.storage.clear_parse_cursor_for_test(&h.path).unwrap();

            let second = h.import(SealPolicy::SealAll).await;

            assert!(
                !second.full_reimport,
                "the head survived, so the conversation must not be wiped"
            );
            assert!(
                h.stored(2).unwrap().contains("EDT004"),
                "the rewritten chunk must be rebuilt"
            );
            let all = h.all_stored().join("\n");
            assert!(
                !all.contains("MSG004") && !all.contains("MSG005"),
                "no stale content may survive an interior rewrite"
            );
            assert!(
                all.contains("MSG000") && all.contains("MSG008"),
                "the matching prefix and the tail must both still be there"
            );
        });
    }

    /// The cursor's head fingerprint covers only 4 KiB, so an edit below that
    /// window left it looking valid while every byte after the edit had moved.
    /// Resuming at the stale offset stranded the rewritten region.
    #[test]
    fn interior_rewrite_below_the_cursor_forces_a_full_parse() {
        rt().block_on(async {
            let h = Harness::new("interior-cursor");
            // Thirteen messages of ~500 bytes each put chunk 5 well past 4 KiB.
            h.write(&msgs(13));
            let first = h.import(SealPolicy::SealAll).await;
            assert_eq!(first.total_chunks, 7, "fixture must produce 7 chunks");

            // Rewrite message 10 (chunk 5) at a different length, so the cursor's
            // byte offset no longer starts a line.
            let mut edited = msgs(13);
            edited[10] = format!("EDT010-{}", "x".repeat(430));
            h.write(&edited);

            let second = h.import(SealPolicy::SealAll).await;

            assert!(
                !second.full_reimport,
                "the head survived, so the conversation must not be wiped"
            );
            assert!(
                h.stored(5).unwrap().contains("EDT010"),
                "the rewritten chunk must be rebuilt, not resumed past"
            );
            let all = h.all_stored().join("\n");
            assert!(!all.contains("MSG010"), "no stale content may survive");
            for i in (0..13).filter(|i| *i != 10) {
                assert!(all.contains(&format!("MSG{i:03}")), "MSG{i:03} missing");
            }
        });
    }

    /// A cursor resume that yields no chunks is not evidence the transcript is
    /// empty. Recording zero chunks and clearing the cursor reported an imported
    /// conversation as unimported, so the next pass rebuilt it from scratch,
    /// called itself a first import, and never noticed the orphan tail.
    #[test]
    fn empty_cursor_resume_retries_a_full_parse() {
        rt().block_on(async {
            let h = Harness::new("empty-resume");
            // Thirteen messages put the seam past the 4 KiB the cursor's head
            // fingerprint covers, so the head below stays byte-identical.
            h.write(&msgs(13));
            let first = h.import(SealPolicy::SealAll).await;
            assert_eq!(first.total_chunks, 7, "fixture must produce 7 chunks");
            let original_len = std::fs::metadata(&h.path).unwrap().len() as usize;

            // Keep every byte before the seam, then replace the trailing chunk's
            // region with lines the parser skips outright.
            let raw = std::fs::read_to_string(&h.path).unwrap();
            let mut kept: Vec<String> = raw.lines().take(12).map(str::to_string).collect();
            while kept.join("\n").len() < original_len {
                kept.push(
                    serde_json::json!({"type": "summary", "summary": "NO MESSAGE CONTENT"})
                        .to_string(),
                );
            }
            h.write_bytes(kept.join("\n").as_bytes());

            let second = h.import(SealPolicy::SealAll).await;

            assert_eq!(
                second.total_chunks, 6,
                "the retry must see the six chunks the file still holds"
            );
            assert!(
                !second.first_import,
                "a conversation with stored chunks is never a first import"
            );
            assert_ne!(
                h.storage.get_imported_chunk_count(&h.path).unwrap(),
                0,
                "the stored chunk count must not be reset to zero"
            );
            assert!(
                h.storage.get_parse_cursor(&h.path).unwrap().is_some(),
                "the cursor must be replaced, not cleared"
            );
            assert!(
                h.stored(6).is_none(),
                "the orphan tail must be dropped, which the empty-result path \
                 returned too early to do"
            );
            let all = h.all_stored().join("\n");
            for i in 0..12 {
                assert!(all.contains(&format!("MSG{i:03}")), "MSG{i:03} missing");
            }
        });
    }

    /// The watcher imports a live transcript with `DeferTrailing` and records the
    /// mtime. When the session stops writing, a `SealAll` pass has to promote the
    /// held-back chunk -- but the mtime gate returned `unchanged` before the seal
    /// policy was ever read, so the conversation's last chunk, usually its
    /// conclusion, never reached the running index.
    #[test]
    fn seal_all_promotes_a_deferred_trailing_chunk() {
        rt().block_on(async {
            let h = Harness::new("promote");
            h.write(&msgs(5));
            let watched = h.import(SealPolicy::DeferTrailing).await;
            assert_eq!(watched.total_chunks, 3);

            let trailing = h.chunk_id(2);
            assert!(
                !h.search.read().await.has_chunk(&trailing),
                "the live watcher must hold the growing chunk back"
            );

            // The session ended. Same bytes, same mtime, sealing pass.
            let sealed = h.import(SealPolicy::SealAll).await;
            assert!(
                !sealed.unchanged,
                "the mtime gate must not short-circuit a pass that owes a vector"
            );
            assert!(
                h.search.read().await.has_chunk(&trailing),
                "the deferred chunk must be indexed once the transcript settles"
            );
            assert_eq!(
                sealed.written_chunks, 0,
                "promotion indexes the chunk, it does not rewrite or restamp it"
            );

            // And the debt clears: a second sealing pass short-circuits again.
            let again = h.import(SealPolicy::SealAll).await;
            assert!(
                again.unchanged,
                "a settled transcript must go back to the cheap mtime gate"
            );
        });
    }

    /// A detached index cannot hold the promotion, so the debt has to survive the
    /// hook rather than being marked paid by a pass that indexed nothing.
    #[test]
    fn detached_seal_pass_leaves_the_debt_for_a_live_one() {
        rt().block_on(async {
            let h = Harness::new("promote-detached");
            h.write(&msgs(5));
            h.import(SealPolicy::DeferTrailing).await;
            let trailing = h.chunk_id(2);

            h.detach_index().await;
            h.import(SealPolicy::SealAll).await;
            assert!(
                !h.storage.is_trailing_chunk_sealed(&h.path).unwrap(),
                "a pass with nowhere to put the vector must not claim it is sealed"
            );

            // A later process with a loaded index settles it.
            h.index_state.set(IndexState::Live);
            let sealed = h.import(SealPolicy::SealAll).await;
            assert!(!sealed.unchanged);
            assert!(h.search.read().await.has_chunk(&trailing));
        });
    }

    /// `precompact` and `session-end` run on an engine that never loads the HNSW
    /// cache (#304), so `has_chunk` answers false for every id and whatever is
    /// inserted dies with the process. Planning off that would re-embed the whole
    /// transcript on every one of those hooks.
    #[test]
    fn detached_index_does_not_re_embed_a_settled_transcript() {
        rt().block_on(async {
            let h = Harness::new("detached");
            h.write(&msgs(5));
            let first = h.import(SealPolicy::SealAll).await;
            assert_eq!(first.total_chunks, 3);

            h.detach_index().await;
            // Identical content, fresh mtime: the hook re-reads but owes no work.
            h.write(&msgs(5));
            let hook_pass = h.import(SealPolicy::SealAll).await;

            assert_eq!(
                hook_pass.written_chunks, 0,
                "settled content must not be rewritten"
            );
            assert_eq!(
                hook_pass.indexed_chunks, 0,
                "a throwaway index must not be fed, and feeding it costs an \
                 embedding per chunk of the whole transcript"
            );
        });
    }

    /// The other half: a detached index must not turn the hook into a no-op.
    /// New content still has to reach SQLite, which is where the next process
    /// that loads the index backfills from.
    #[test]
    fn detached_index_still_writes_new_content() {
        rt().block_on(async {
            let h = Harness::new("detached-writes");
            h.write(&msgs(5));
            h.import(SealPolicy::SealAll).await;

            h.detach_index().await;
            h.write(&msgs(7));
            let hook_pass = h.import(SealPolicy::SealAll).await;

            assert!(
                hook_pass.written_chunks > 0,
                "appended content must still be stored"
            );
            assert_eq!(hook_pass.indexed_chunks, 0);
            let all = h.all_stored().join("\n");
            for i in 0..7 {
                assert!(all.contains(&format!("MSG{i:03}")), "MSG{i:03} missing");
            }
            let vectors = h.storage.load_all_chunk_vectors().unwrap();
            assert!(
                vectors.iter().any(|(id, _)| id == &h.chunk_id(3)),
                "the embedding must be in SQLite for the next loader to pick up"
            );
        });
    }

    /// Frozen per-chunk timestamps: an untouched chunk is never rewritten, so a
    /// no-op pass cannot restamp it. search::decay reads this.
    #[test]
    fn unchanged_content_does_not_restamp_timestamp() {
        rt().block_on(async {
            let h = Harness::new("frozen-ts");
            h.write(&msgs(5));
            h.import(SealPolicy::SealAll).await;

            let ts_before = h.timestamp(0);

            // Rewrite identical content: mtime moves, content does not.
            h.write(&msgs(5));
            let second = h.import(SealPolicy::SealAll).await;
            assert_eq!(second.written_chunks, 0, "nothing changed, nothing written");

            let ts_after = h.timestamp(0);
            assert_eq!(ts_before, ts_after, "settled chunks must keep their stamp");
        });
    }

    /// The mtime gate still short-circuits an unchanged file without re-reading it.
    #[test]
    fn unchanged_file_short_circuits() {
        rt().block_on(async {
            let h = Harness::new("mtime-gate");
            h.write(&msgs(5));
            h.import(SealPolicy::SealAll).await;
            let second = h.import(SealPolicy::SealAll).await;
            assert!(second.unchanged);
            assert_eq!(second.total_chunks, 0);
        });
    }

    /// A pass that did no work must not trigger enrichment -- that gate is what
    /// stopped a persistently-failing conversation re-reading a 75 MB transcript
    /// every debounce.
    #[test]
    fn enrichment_skipped_when_pass_did_no_work() {
        rt().block_on(async {
            let h = Harness::new("enrich-gate");
            h.write(&msgs(5));
            let attribution = ConversationAttribution {
                project_name: "test".to_string(),
                source: "conversation",
                parent_conversation_id: None,
            };
            let idle = ImportOutcome {
                first_import: false,
                written_chunks: 0,
                ..Default::default()
            };
            maybe_enrich(&h.ctx(), &idle, &h.path, &attribution).await;
            assert!(
                !h.storage
                    .is_conversation_enriched(&h.conv_id(), "heuristic")
                    .unwrap_or(false),
                "an idle pass must not enrich"
            );
        });
    }

    /// A rewritten file of the same length must not be resumed from a stale offset.
    #[test]
    fn truncate_and_regrow_invalidates_cursor() {
        rt().block_on(async {
            let h = Harness::new("regrow");
            h.write(&msgs(7));
            h.import(SealPolicy::SealAll).await;
            let before = h.all_stored();
            assert!(before.join("\n").contains("MSG006"));

            // Same message count, entirely different content.
            let replaced: Vec<String> = (0..7)
                .map(|i| format!("NEW{i:03}-{}", "q".repeat(390)))
                .collect();
            h.write(&replaced);
            h.import(SealPolicy::SealAll).await;

            let after = h.all_stored().join("\n");
            assert!(
                after.contains("NEW000") && after.contains("NEW006"),
                "the rewrite must be fully reimported"
            );
            assert!(
                !after.contains("MSG"),
                "a stale cursor must not leave old content behind"
            );
        });
    }

    /// One invalid byte used to hide everything after it permanently: the pass
    /// ended at that byte, the cursor it wrote pointed before the bad line, and
    /// every later pass resumed there and stopped in the same place. Growth that
    /// lands after the bad byte must still be imported, pass after pass.
    #[test]
    fn invalid_utf8_line_does_not_freeze_the_cursor() {
        rt().block_on(async {
            let h = Harness::new("bad-byte");

            // Three clean messages, one carrying a lone 0xFF, then two more.
            let mut bytes: Vec<u8> = Vec::new();
            for (i, text) in msgs(3).iter().enumerate() {
                bytes.extend_from_slice(
                    serde_json::json!({
                        "type": if i.is_multiple_of(2) { "user" } else { "assistant" },
                        "timestamp": format!("2026-02-22T10:00:{:02}Z", i),
                        "message": {"content": [{"type": "text", "text": text}]}
                    })
                    .to_string()
                    .as_bytes(),
                );
                bytes.push(b'\n');
            }
            let bad_line_start = bytes.len() as u64;
            bytes.extend_from_slice(
                br#"{"type":"user","timestamp":"2026-02-22T10:00:03Z","message":{"content":[{"type":"text","text":"BADBYTE-"#,
            );
            bytes.push(0xFF);
            bytes.extend_from_slice("y".repeat(380).as_bytes());
            bytes.extend_from_slice(br#""}]}}"#);
            bytes.push(b'\n');

            let tail = |from: usize, to: usize| -> Vec<u8> {
                let mut out = Vec::new();
                for i in from..to {
                    out.extend_from_slice(
                        serde_json::json!({
                            "type": if i.is_multiple_of(2) { "user" } else { "assistant" },
                            "timestamp": format!("2026-02-22T10:00:{:02}Z", i),
                            "message": {"content": [
                                {"type": "text", "text": format!("MSG{i:03}-{}", "x".repeat(390))}
                            ]}
                        })
                        .to_string()
                        .as_bytes(),
                    );
                    out.push(b'\n');
                }
                out
            };

            let mut first = bytes.clone();
            first.extend_from_slice(&tail(4, 6));
            h.write_bytes(&first);
            h.import(SealPolicy::SealAll).await;

            let after_first = h.all_stored().join("\n");
            for i in 4..6 {
                assert!(
                    after_first.contains(&format!("MSG{i:03}")),
                    "MSG{i:03} follows the invalid byte and must be imported"
                );
            }
            let stored_cursor: ParseCursor = serde_json::from_str(
                &h.storage
                    .get_parse_cursor(&h.path)
                    .unwrap()
                    .expect("a cursor must be stored"),
            )
            .unwrap();
            assert!(
                stored_cursor.byte_offset > bad_line_start,
                "the stored cursor must sit past the invalid line, not on it"
            );

            // Append past the bad byte. A frozen cursor would never reach this.
            let mut grown = bytes.clone();
            grown.extend_from_slice(&tail(4, 10));
            h.write_bytes(&grown);
            h.import(SealPolicy::SealAll).await;

            let after_second = h.all_stored().join("\n");
            for i in 4..10 {
                assert!(
                    after_second.contains(&format!("MSG{i:03}")),
                    "MSG{i:03} was appended after the invalid byte and must be imported"
                );
            }
        });
    }

    /// A NULL cursor is the downgrade path and the pre-migration path: it must
    /// simply fall back to a full parse with the seam rebuild.
    #[test]
    fn null_cursor_falls_back_to_full_parse() {
        rt().block_on(async {
            let h = Harness::new("null-cursor");
            h.write(&msgs(5));
            h.import(SealPolicy::SealAll).await;

            // Simulate an older binary having written the row without a cursor.
            h.storage
                .clear_parse_cursor_for_test(&h.path)
                .expect("clear cursor");
            assert!(h.storage.get_parse_cursor(&h.path).unwrap().is_none());

            h.write(&msgs(7));
            h.import(SealPolicy::SealAll).await;

            let all = h.all_stored().join("\n");
            for i in 0..7 {
                assert!(all.contains(&format!("MSG{i:03}")), "MSG{i:03} missing");
            }
        });
    }

    /// The mtime recorded by an import has to describe the bytes the parser
    /// actually read. Read at commit instead, it can describe an append that
    /// landed after EOF, and the mtime gate then skips those bytes until the
    /// next append moves the mtime again -- or forever, if the session ended.
    #[test]
    fn recorded_mtime_describes_the_bytes_that_were_read() {
        let h = Harness::new("mtime-race");
        h.write(&msgs(5));
        let before_parse = Storage::current_file_mtime(&h.path);

        // Stand in for the writer appending between EOF and the commit.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&h.path, "x").unwrap();
        assert_ne!(before_parse, Storage::current_file_mtime(&h.path));

        h.storage
            .mark_file_imported_with_cursor(
                &h.path,
                3,
                Default::default(),
                None,
                true,
                Some(&before_parse),
            )
            .unwrap();

        assert!(
            !h.storage.is_file_imported(&h.path).unwrap(),
            "a file that moved during the pass must read as changed, so the \
             bytes the parser never saw get another chance"
        );
    }

    /// The migration must be additive on a database that predates the column.
    #[test]
    fn cursor_column_migration_is_additive() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("legacy.db");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            // The pre-cursor shape of the table, with a row already in it.
            conn.execute_batch(
                "CREATE TABLE import_state (
                     file_path TEXT PRIMARY KEY,
                     conversation_id TEXT,
                     chunks_imported INTEGER,
                     imported_at TEXT DEFAULT (datetime('now')),
                     file_mtime TEXT,
                     csr_tool_blocks_suppressed INTEGER NOT NULL DEFAULT 0,
                     csr_hook_wrappers_scrubbed INTEGER NOT NULL DEFAULT 0
                 );
                 INSERT INTO import_state (file_path, chunks_imported) VALUES ('/legacy.jsonl', 12);",
            )
            .unwrap();
        }

        let storage = Storage::open(&db).expect("migrations must run on a legacy database");
        let cursor = storage
            .get_parse_cursor(Path::new("/legacy.jsonl"))
            .expect("the column must exist after migration");
        assert!(cursor.is_none(), "legacy rows start with no cursor");
        assert_eq!(
            storage
                .get_imported_chunk_count(Path::new("/legacy.jsonl"))
                .unwrap(),
            12,
            "the existing row must survive the ALTER"
        );
        assert!(
            storage
                .is_trailing_chunk_sealed(Path::new("/legacy.jsonl"))
                .expect("the seal column must exist after migration"),
            "a row written before deferral existed did index its trailing chunk, \
             so NULL must read as sealed rather than forcing a reseal of the corpus"
        );
    }
}
