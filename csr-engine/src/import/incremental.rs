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
    // Cheap gate: nothing on disk changed since the last pass.
    if ctx.storage.is_file_imported(file_path)? {
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

    // Resume from the stored byte cursor when it still describes this file.
    let stored_cursor = ctx
        .storage
        .get_parse_cursor(file_path)?
        .and_then(|json| serde_json::from_str::<ParseCursor>(&json).ok())
        .filter(|c| c.v == PARSE_CURSOR_VERSION && cursor_still_valid(c, file_path));

    let parsed = import::parse_jsonl_file_from_cursor(
        file_path,
        &attribution.project_name,
        stored_cursor.as_ref(),
    )?;
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
    // skipped). Wiping is destructive, so it needs corroboration: compare the
    // head chunk's content. A rewrite changes it; a short read does not.
    let mut full_reimport = false;
    let rebuild_from = if stored_cursor.is_some() {
        // Everything the cursor handed back begins at the seam by construction,
        // and the cursor was only trusted after its head fingerprint matched.
        first_index
    } else {
        // Full parse, so the head is in hand. Verify the prefix really is intact
        // before trusting it: a rewrite changes chunk zero even when the chunk
        // count does not move, and resuming at the seam would strand the old
        // content in place. This also covers a cursor rejected as stale.
        let head_changed = prev_count > 0
            && ctx
                .storage
                .get_chunk_content(&chunks[0].id)?
                .is_none_or(|stored| stored != chunks[0].content);

        if head_changed {
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
        } else if n < prev_count {
            // Head intact but fewer chunks: keep the valid prefix, drop the
            // orphan tail rather than wiping a conversation needlessly.
            tracing::warn!(
                conv = %conversation_id,
                previous = prev_count,
                current = n,
                "transcript shrank with an intact head — dropping orphan tail chunks"
            );
            let orphans: Vec<String> = (n..prev_count)
                .map(|i| import::generate_chunk_id(&conversation_id, i))
                .collect();
            ctx.storage.delete_chunks_by_ids(&orphans)?;
            {
                let mut idx = ctx.search.write().await;
                for id in &orphans {
                    idx.remove_chunk(id);
                }
            }
            n.saturating_sub(1)
        } else {
            // The seam. `prev_count` counts chunks WRITTEN, and the last of those
            // was a partial buffer flushed at EOF — on this pass it may have
            // grown, so it must be rebuilt. Slicing from `prev_count` instead
            // drops its new messages into no chunk at all.
            prev_count.saturating_sub(1)
        }
    };

    // ── Plan: what actually needs work ───────────────────────────────────────
    let mut plans: Vec<ChunkPlan> = Vec::new();
    {
        let idx = ctx.search.read().await;
        for (local, chunk) in chunks.iter().enumerate() {
            let i = first_index + local;
            if i < rebuild_from {
                continue;
            }
            let is_trailing = i + 1 == n;
            let index_it = !is_trailing || seal == SealPolicy::SealAll;
            let indexed = idx.has_chunk(&chunk.id);

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
    ctx.storage.mark_file_imported_with_cursor(
        file_path,
        n,
        suppression,
        cursor_json.as_deref(),
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
        _dir: TempDir,
        path: PathBuf,
        storage: Arc<Storage>,
        embeddings: Arc<EmbeddingEngine>,
        search: Arc<RwLock<SearchEngine>>,
    }

    impl Harness {
        fn new(name: &str) -> Self {
            let dir = TempDir::new().unwrap();
            let path = dir.path().join(format!("{name}.jsonl"));
            Self {
                _dir: dir,
                path,
                storage: Arc::new(Storage::open_memory().unwrap()),
                embeddings: embeddings(),
                search: Arc::new(RwLock::new(SearchEngine::new(256))),
            }
        }

        fn ctx(&self) -> ImportContext<'_> {
            ImportContext {
                storage: &self.storage,
                embeddings: &self.embeddings,
                search: &self.search,
            }
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
    #[test]
    fn sealed_seam_replaces_stale_vector() {
        rt().block_on(async {
            let h = Harness::new("stale-vector");
            h.write(&msgs(5));
            // SealAll indexes the partial c2 straight away -- the situation the
            // hook path creates and DeferTrailing avoids.
            h.import(SealPolicy::SealAll).await;
            let seam = h.chunk_id(2);
            assert!(h.search.read().await.has_chunk(&seam));

            h.write(&msgs(7));
            let second = h.import(SealPolicy::SealAll).await;

            assert!(
                second.indexed_chunks > 0,
                "the grown seam must be re-indexed, not skipped"
            );
            // The index slot must now be backed by the grown content.
            assert!(h.stored(2).unwrap().contains("MSG005"));
            assert!(h.search.read().await.has_chunk(&seam));
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
    }
}
