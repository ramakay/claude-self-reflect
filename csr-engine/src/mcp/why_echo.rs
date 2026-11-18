//! `csr_why` evidence that is not an answer: a logged copy of the question, a
//! CSR emission, a benchmark or eval session's row, or the rig's own copy of
//! the audit question. These rows sink below the clean evidence (stable, never
//! dropped), the way `csr_reflect_on_past` treats them, so a probe that asked
//! the same question does not take the first place.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;

use crate::embeddings::EmbeddingEngine;
use crate::search::reinstatement::{self, EvidenceItem, ReinstateConfig, ReinstateResult};
use crate::search::SearchEngine;
use crate::storage::Storage;

/// Widest walk, in items, asked for to replace echoes cut out of the top `k`.
const WIDE_MAX: usize = 200;

/// `items` with the echo rows behind the rest, cut to `k`: the answers that
/// follow an echo take its place instead of the echo taking a slot.
pub(super) fn take_clean(
    storage: &Storage,
    query: &str,
    items: Vec<EvidenceItem>,
    k: usize,
) -> Vec<EvidenceItem> {
    let mut items = sink_echoes(storage, query, items);
    items.truncate(k);
    items
}

/// The reinstatement walk for `cfg.k` items, none of them an echo while clean
/// evidence exists. The walk cuts its list to `k` before any echo is known, so
/// while the list holds an echo, fewer than `k` clean items, and the walk was
/// full, it runs again at four times the width (up to [`WIDE_MAX`], or `k`
/// when that is larger), and the echoes are sunk before the cut to `k`. A list
/// with no echo is returned as walked.
pub(super) async fn reinstate_clean(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    project: Option<&str>,
    cfg: &ReinstateConfig,
) -> Result<ReinstateResult> {
    clean_walk(storage, query, cfg.k, move |width| {
        let at_width = ReinstateConfig {
            k: width,
            ..cfg.clone()
        };
        async move {
            reinstatement::reinstate(storage, embeddings, search, query, project, &at_width).await
        }
    })
    .await
}

/// The widening loop of [`reinstate_clean`]; `walk_at(width)` runs one walk
/// that returns up to `width` items.
async fn clean_walk<F, Fut>(
    storage: &Storage,
    query: &str,
    k: usize,
    mut walk_at: F,
) -> Result<ReinstateResult>
where
    F: FnMut(usize) -> Fut,
    Fut: std::future::Future<Output = Result<ReinstateResult>>,
{
    let ceiling = WIDE_MAX.max(k);
    let mut width = k;
    let mut walk = walk_at(width).await?;
    loop {
        let echoes = echo_set(storage, query, &walk.items);
        if echoes.is_empty() {
            walk.items.truncate(k);
            return Ok(walk);
        }
        let clean = walk.items.len() - echoes.len();
        if clean >= k || walk.items.len() < width || width >= ceiling {
            break;
        }
        width = width.saturating_mul(4).clamp(k, ceiling);
        walk = walk_at(width).await?;
    }
    walk.items = take_clean(storage, query, walk.items, k);
    Ok(walk)
}

/// The ids among `items` that are echoes. Fails open: a storage error is none.
/// An id that is no chunk is a stored reflection: it is judged by its text,
/// the rule recall applies to a reflection row (it is not a session's row, so
/// the session checks do not apply).
fn echo_set(storage: &Storage, query: &str, items: &[EvidenceItem]) -> HashSet<String> {
    let ids: Vec<String> = items.iter().map(|item| item.chunk_id.clone()).collect();
    let Ok(chunks) = storage.get_chunks_by_ids_with_provenance(&ids) else {
        return HashSet::new();
    };
    let mut sunk = echo_ids(storage, query, &chunks);
    let known: HashSet<&str> = chunks.iter().map(|chunk| chunk.id.as_str()).collect();
    for id in ids.iter().filter(|id| !known.contains(id.as_str())) {
        if let Ok(Some((content, _, _))) = storage.get_reflection_by_id(id) {
            if reflection_is_echo(&content, query) {
                sunk.insert(id.clone());
            }
        }
    }
    sunk
}

/// A reflection that only restates the question, or holds CSR's own payload.
fn reflection_is_echo(content: &str, query: &str) -> bool {
    crate::search::echo::is_question_echo(&format!("[reflection] {content}"), query)
        || crate::import::contamination::is_csr_tool_payload(content)
}

/// `items` with the echo rows moved behind the rest, order kept in each group.
/// Fails open: a storage error leaves the order as it was.
pub(super) fn sink_echoes(
    storage: &Storage,
    query: &str,
    items: Vec<EvidenceItem>,
) -> Vec<EvidenceItem> {
    let sunk = echo_set(storage, query, &items);
    if sunk.is_empty() {
        return items;
    }
    let (clean, echoes): (Vec<_>, Vec<_>) = items
        .into_iter()
        .partition(|item| !sunk.contains(&item.chunk_id));
    clean.into_iter().chain(echoes).collect()
}

fn echo_ids(
    storage: &Storage,
    query: &str,
    chunks: &[crate::import::ConversationChunk],
) -> HashSet<String> {
    let mut sunk: HashSet<String> = chunks
        .iter()
        .filter(|chunk| {
            crate::search::echo::runtime_drop(
                &chunk.content,
                Some(chunk.author),
                &chunk.conversation_id,
                query,
            )
            .is_some()
        })
        .map(|chunk| chunk.id.clone())
        .collect();
    let refs: Vec<super::synthetic::ChunkRef> = chunks
        .iter()
        .map(|chunk| super::synthetic::ChunkRef {
            id: chunk.id.clone(),
            conversation_id: chunk.conversation_id.clone(),
            author: Some(chunk.author),
        })
        .collect();
    sunk.extend(super::synthetic::synthetic_ids(storage, &refs));
    let sinks: Vec<super::synthetic::SinkRef<'_>> = chunks
        .iter()
        .map(|chunk| super::synthetic::SinkRef {
            id: &chunk.id,
            conversation_id: &chunk.conversation_id,
            content: &chunk.content,
        })
        .collect();
    sunk.extend(super::synthetic::rig_sunk_ids(storage, &sinks, query));
    sunk
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::reinstatement::Via;
    use crate::transcript::chunks::test_support::{add_chunk, temp_storage};

    fn item(chunk: &str, conversation: &str, score: f32) -> EvidenceItem {
        EvidenceItem {
            chunk_id: chunk.into(),
            conversation_id: conversation.into(),
            score,
            best_route: Via::Seed,
            routes: [Via::Seed].into_iter().collect(),
            route_scores: [(Via::Seed, score)].into_iter().collect(),
            timestamp: "2026-11-01T00:00:00Z".into(),
            excerpt: String::new(),
            ratification: None,
            trust: crate::provenance::TrustTier::Unknown,
        }
    }

    #[test]
    fn a_logged_copy_of_the_question_sinks_below_the_answer() {
        let dir = tempfile::tempdir().unwrap();
        let storage = temp_storage(dir.path());
        let query = "How does the retry backoff work for outgoing hook calls?";
        add_chunk(
            &storage,
            "copy",
            "conv-probe",
            "p",
            "2026-11-01T00:00:00Z",
            0,
            "tool_result",
            1,
            &format!("{{\"query\": \"{query}\"}}"),
        );
        add_chunk(
            &storage,
            "answer",
            "conv-real",
            "p",
            "2026-11-01T00:00:00Z",
            0,
            "assistant",
            1,
            "The hook client doubles its delay after each failure, up to a cap of 30 seconds.",
        );
        let ordered = sink_echoes(
            &storage,
            query,
            vec![
                item("copy", "conv-probe", 0.9),
                item("answer", "conv-real", 0.8),
            ],
        );
        let ids: Vec<_> = ordered.iter().map(|i| i.chunk_id.as_str()).collect();
        assert_eq!(ids, ["answer", "copy"]);
    }

    #[test]
    fn a_cut_to_one_keeps_the_answer_and_not_the_echo_ahead_of_it() {
        let dir = tempfile::tempdir().unwrap();
        let storage = temp_storage(dir.path());
        let query = "How does the retry backoff work for outgoing hook calls?";
        add_chunk(
            &storage,
            "copy",
            "conv-probe",
            "p",
            "2026-11-01T00:00:00Z",
            0,
            "tool_result",
            1,
            &format!("{{\"query\": \"{query}\"}}"),
        );
        add_chunk(
            &storage,
            "answer",
            "conv-real",
            "p",
            "2026-11-01T00:00:00Z",
            0,
            "assistant",
            1,
            "The hook client doubles its delay after each failure, up to a cap of 30 seconds.",
        );
        let kept = take_clean(
            &storage,
            query,
            vec![
                item("copy", "conv-probe", 0.9),
                item("answer", "conv-real", 0.8),
            ],
            1,
        );
        let ids: Vec<_> = kept.iter().map(|i| i.chunk_id.as_str()).collect();
        assert_eq!(ids, ["answer"]);
    }

    #[test]
    fn clean_evidence_keeps_its_order() {
        let dir = tempfile::tempdir().unwrap();
        let storage = temp_storage(dir.path());
        for (id, conv) in [("a", "conv-a"), ("b", "conv-b")] {
            add_chunk(
                &storage,
                id,
                conv,
                "p",
                "2026-11-01T00:00:00Z",
                0,
                "assistant",
                1,
                "The hook client doubles its delay after each failure.",
            );
        }
        let ordered = sink_echoes(
            &storage,
            "How does the retry backoff work for outgoing hook calls?",
            vec![item("a", "conv-a", 0.9), item("b", "conv-b", 0.8)],
        );
        let ids: Vec<_> = ordered.iter().map(|i| i.chunk_id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
    }

    /// Thirty logged copies of the question, then one answer: the ranked walk
    /// ranks every copy ahead of the answer.
    fn thirty_echoes_then_an_answer(storage: &Storage, query: &str) -> Vec<EvidenceItem> {
        let mut ranked = Vec::new();
        for i in 0..30 {
            let (id, conv) = (format!("copy-{i}"), format!("conv-probe-{i}"));
            add_chunk(
                storage,
                &id,
                &conv,
                "p",
                "2026-11-01T00:00:00Z",
                0,
                "tool_result",
                1,
                &format!("{{\"query\": \"{query}\"}}"),
            );
            ranked.push(item(&id, &conv, 0.9));
        }
        add_chunk(
            storage,
            "answer",
            "conv-real",
            "p",
            "2026-11-01T00:00:00Z",
            0,
            "assistant",
            1,
            "The hook client doubles its delay after each failure, up to a cap of 30 seconds.",
        );
        ranked.push(item("answer", "conv-real", 0.5));
        ranked
    }

    #[tokio::test]
    async fn a_walk_that_starts_with_many_echoes_widens_until_the_answer_is_reached() {
        let dir = tempfile::tempdir().unwrap();
        let storage = temp_storage(dir.path());
        let query = "How does the retry backoff work for outgoing hook calls?";
        let ranked = thirty_echoes_then_an_answer(&storage, query);
        let widths = std::cell::RefCell::new(Vec::new());
        let walked = clean_walk(&storage, query, 1, |width| {
            widths.borrow_mut().push(width);
            let items: Vec<_> = ranked.iter().take(width).cloned().collect();
            async move {
                Ok(ReinstateResult {
                    items,
                    ..ReinstateResult::default()
                })
            }
        })
        .await
        .unwrap();
        let ids: Vec<_> = walked.items.iter().map(|i| i.chunk_id.as_str()).collect();
        assert_eq!(ids, ["answer"]);
        assert_eq!(*widths.borrow(), [1, 4, 16, 64]);
    }

    #[test]
    fn a_reflection_that_restates_the_question_sinks_below_one_that_answers() {
        let dir = tempfile::tempdir().unwrap();
        let storage = temp_storage(dir.path());
        let query = "How does the retry backoff work for outgoing hook calls?";
        let no_tags: Vec<String> = Vec::new();
        storage
            .insert_reflection("restated", query, &no_tags, &[0.0; 384])
            .unwrap();
        storage
            .insert_reflection(
                "answered",
                "The hook client doubles its delay after each failure, up to a cap of 30 seconds.",
                &no_tags,
                &[0.0; 384],
            )
            .unwrap();
        let ordered = sink_echoes(
            &storage,
            query,
            vec![
                item("restated", "conv-a", 0.9),
                item("answered", "conv-b", 0.8),
            ],
        );
        let ids: Vec<_> = ordered.iter().map(|i| i.chunk_id.as_str()).collect();
        assert_eq!(ids, ["answered", "restated"]);
    }

    #[tokio::test]
    async fn a_walk_with_no_echo_runs_once_and_is_returned_as_walked() {
        let dir = tempfile::tempdir().unwrap();
        let storage = temp_storage(dir.path());
        add_chunk(
            &storage,
            "a",
            "conv-a",
            "p",
            "2026-11-01T00:00:00Z",
            0,
            "assistant",
            1,
            "The hook client doubles its delay after each failure.",
        );
        let widths = std::cell::RefCell::new(Vec::new());
        let walked = clean_walk(
            &storage,
            "How does the retry backoff work for outgoing hook calls?",
            3,
            |width| {
                widths.borrow_mut().push(width);
                async move {
                    Ok(ReinstateResult {
                        items: vec![item("a", "conv-a", 0.9)],
                        ..ReinstateResult::default()
                    })
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(walked.items.len(), 1);
        assert_eq!(*widths.borrow(), [3]);
    }
}
