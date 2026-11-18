//! Which chunks the cross-encoder scores, and the debug lines that show what it did with them.
//!
//! The pool is the first [`CE_POOL`](crate::search::cross_encoder::CE_POOL) distinct ids of the
//! fused chunk order, with the S1 top sessions' representatives forced in from the tail. The
//! debug lines (`RUST_LOG=csr_engine=debug`) carry ids and numbers only, never chunk text: the
//! pool, each pooled chunk's score and answer class, which chunk each session is shown and
//! ranked by and why, and the key of every row in the final order.

use std::collections::{HashMap, HashSet};

use crate::format::EnrichedResult;

/// The chunk ids the cross-encoder scores: the first `limit` distinct ids of
/// the fused chunk ranking, then every id in `protected` (the S1 top
/// sessions' representatives) forced in by replacing, from the tail, ids that
/// are not themselves protected. The pool never exceeds `limit`.
pub(super) fn ce_pool_ids(fused: &[String], protected: &[String], limit: usize) -> Vec<String> {
    let mut pool: Vec<String> = Vec::with_capacity(limit);
    let mut seen: HashSet<&str> = HashSet::new();
    for id in fused {
        if pool.len() >= limit {
            break;
        }
        if seen.insert(id.as_str()) {
            pool.push(id.clone());
        }
    }
    for id in protected {
        if pool.contains(id) {
            continue;
        }
        if pool.len() < limit {
            pool.push(id.clone());
        } else if let Some(tail) = pool.iter().rposition(|held| !protected.contains(held)) {
            pool.remove(tail);
            pool.push(id.clone());
        }
    }
    pool
}

/// One debug line per pooled chunk: `(chunk, session, answer class, score)`. Answer class 0 is
/// a row that can answer, 1 a question echo, 2 CSR's own payload or a terminal replay.
pub(super) fn log_ce_scores(scored: &[(String, String, u8, f32)]) {
    for (place, (chunk, session, class, score)) in scored.iter().enumerate() {
        tracing::debug!(
            place,
            chunk = chunk.as_str(),
            session = session.as_str(),
            class,
            score,
            "cross-encoder score"
        );
    }
}

/// The label a shown row carries: `ce` when the cross-encoder scored that chunk, else `keyword`
/// for a chunk only the keyword channel found, else `cosine`.
fn label(id: &str, score: Option<f32>, by_id: &HashMap<&str, &EnrichedResult>) -> &'static str {
    if score.is_some() {
        "ce"
    } else if by_id
        .get(id)
        .is_some_and(|row| row.chunk.content.starts_with("[keyword] "))
    {
        "keyword"
    } else {
        "cosine"
    }
}

/// One debug line per session row after the cross-encoder: the chunk it is shown by, the
/// chunk the cross-encoder ranked it by (`pick`), both scores, the session score that sets its
/// place, the shown chunk's label and why that chunk was chosen.
pub(super) fn log_ce_rows(
    ordered: &super::CeOrdered,
    picks: &[String],
    session_keys: &HashMap<String, String>,
    scored: &[(String, String, u8, f32)],
    by_id: &HashMap<&str, &EnrichedResult>,
) {
    let (order, by_session) = (&ordered.order, &ordered.by_session);
    let score_of = |id: &str| {
        scored
            .iter()
            .find(|row| row.0 == id)
            .map(|row| row.3)
            .filter(|score| score.is_finite())
    };
    let class_of = |id: &str| scored.iter().find(|row| row.0 == id).map(|row| row.2);
    for (rank, (shown, pick)) in order.iter().zip(picks).enumerate() {
        let session = session_keys
            .get(shown)
            .map_or(shown.as_str(), String::as_str);
        let session_score = by_session.get(session).copied();
        let why = match (session_score, shown == pick) {
            (None, _) => "no pooled chunk: kept its S1 place after the scored sessions",
            (Some(_), true) => "best score among its pooled rows of the lowest answer class",
            (Some(_), false) => "content chunk within 0.25 of the pick, shown in its place",
        };
        tracing::debug!(
            rank,
            session,
            shown = shown.as_str(),
            shown_score = ?score_of(shown),
            shown_class = ?class_of(shown),
            label = label(shown, score_of(shown), by_id),
            pick = pick.as_str(),
            pick_score = ?score_of(pick),
            session_score = ?session_score,
            why,
            "cross-encoder row"
        );
    }
}

/// One debug line per row of the order the page is cut from, with its sort key: the session's
/// cross-encoder score, or its S1 place when no chunk of it was pooled.
pub(super) fn log_final_order(
    order: &[String],
    session_keys: &HashMap<String, String>,
    scores: &HashMap<String, f32>,
) {
    if !tracing::enabled!(tracing::Level::DEBUG) {
        return;
    }
    let mut unscored = 0;
    for (rank, id) in order.iter().enumerate() {
        let session = session_keys.get(id).map_or(id.as_str(), String::as_str);
        let id = id.as_str();
        match scores.get(session) {
            Some(score) => {
                tracing::debug!(rank, id, session, key = "ce", score = *score, "final row")
            }
            None => {
                tracing::debug!(rank, id, session, key = "s1", s1 = unscored, "final row");
                unscored += 1;
            }
        }
    }
}
