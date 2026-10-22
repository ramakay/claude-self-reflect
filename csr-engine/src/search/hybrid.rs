//! Hybrid retrieval primitives shared by the MCP search path and the
//! `UserPromptSubmit` hook: the search mode switch, reciprocal-rank fusion of
//! the vector and keyword rankings, the keyword (FTS5) chunk channel, and the
//! synthetic score a keyword-only chunk carries.
//!
//! One copy. `mcp::tools` and `hooks::prompt_submit` both call these; neither
//! carries its own fusion.

use std::collections::{HashMap, HashSet};

use crate::import::ConversationChunk;
use crate::storage::Storage;

/// `k` in reciprocal-rank fusion: `score(id) = sum over rankings of 1 / (k + rank)`.
pub const RRF_K: usize = 60;

/// Base score of a chunk reached only through the keyword channel, before age
/// decay. Sits just below the semantic threshold so it ranks after good
/// semantic matches and above nothing.
pub const FTS_ONLY_BASE_SCORE: f32 = 0.45;

/// What a keyword-only chunk scores when its timestamp does not parse: the
/// base score, undecayed, less the amount an undated chunk gives up.
pub const FTS_ONLY_UNDATED_SCORE: f32 = 0.40;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchMode {
    Hybrid,
    Vector,
    Fts,
}

impl SearchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hybrid => "hybrid",
            Self::Vector => "vector",
            Self::Fts => "fts",
        }
    }
}

/// `CSR_SEARCH_MODE` value to mode. Anything but `vector` or `fts` is hybrid.
pub fn search_mode_from(value: Option<&str>) -> SearchMode {
    match value {
        Some("vector") => SearchMode::Vector,
        Some("fts") => SearchMode::Fts,
        _ => SearchMode::Hybrid,
    }
}

/// The mode the process was asked for through `CSR_SEARCH_MODE`.
pub fn search_mode_from_env() -> SearchMode {
    search_mode_from(std::env::var("CSR_SEARCH_MODE").ok().as_deref())
}

/// Fuse two rankings (best first) by reciprocal rank. An id present in both
/// rankings sums its two contributions. Ties keep first-seen order, semantic
/// list first.
pub fn fuse_rrf(semantic_order: &[String], fts_order: &[String], k: usize) -> Vec<String> {
    let mut fused_order = Vec::with_capacity(semantic_order.len() + fts_order.len());
    let mut scores: HashMap<String, f64> = HashMap::new();

    for ranking in [semantic_order, fts_order] {
        let mut seen = HashSet::new();
        for (rank, id) in ranking.iter().enumerate() {
            if !seen.insert(id.as_str()) {
                continue;
            }
            if !scores.contains_key(id) {
                fused_order.push(id.clone());
            }
            *scores.entry(id.clone()).or_default() += 1.0 / (k + rank) as f64;
        }
    }

    fused_order.sort_by(|left, right| scores[right].total_cmp(&scores[left]));
    fused_order
}

/// A row the keyword lane lists is "weakly similar" when its cosine is below
/// this share of the best cosine among the rows that can answer. Measured on
/// the ten live known-answer questions with a 60-row vector window: the lowest
/// an answer sits is 0.54 of that best (Q3's `9eabddaf`, 0.527 of 0.971), then
/// 0.65, 0.67 and 0.73; the filler this catches is the `qdrant_v3_*` pair at
/// 0.45 and 0.475. 0.60 was tried first and sank Q3's answer out of the top 30,
/// so the margin to the lowest answer is 0.04 and the ratio does not separate
/// filler from answers with any room. It does not catch `fcc7eb6e` (0.52) or a
/// legacy uuid-id memory such as `2c9e0d3b` (0.70): that row is lifted by the
/// explicit-memory standing in the rerank, not by the keyword lane.
pub const KEYWORD_ROW_COSINE_FLOOR: f32 = 0.50;

/// Move the weakly similar keyword rows of a fused order below every strongly
/// similar vector hit, keeping the order inside each group.
///
/// A row is weak when the keyword lane lists it and its cosine is under
/// `floor` times the best cosine in `raw_cosine`; a row the vector window never
/// returned counts as weak only when `is_reflection` says it is a stored
/// reflection (a chunk found by keyword alone is a needle match: it carries no
/// cosine and is left where fusion put it). A row is strong when its cosine is
/// at least `floor` times the best. When the window has no cosine at all, or
/// no row is weak, the order is returned unchanged. Nothing is dropped.
pub fn demote_weakly_similar_keyword_rows(
    fused: Vec<String>,
    fts_order: &[String],
    raw_cosine: &HashMap<&str, f32>,
    is_reflection: impl Fn(&str) -> bool,
    floor: f32,
) -> Vec<String> {
    let best = raw_cosine.values().copied().fold(0.0_f32, f32::max);
    if best <= 0.0 {
        return fused;
    }
    let cut = floor * best;
    let keyword: HashSet<&str> = fts_order.iter().map(String::as_str).collect();
    let weak = |id: &str| {
        keyword.contains(id)
            && match raw_cosine.get(id) {
                Some(cosine) => *cosine < cut,
                None => is_reflection(id),
            }
    };
    let strong = |id: &str| raw_cosine.get(id).is_some_and(|cosine| *cosine >= cut);
    let Some(last_strong) = fused.iter().rposition(|id| strong(id)) else {
        return fused;
    };
    if !fused.iter().take(last_strong).any(|id| weak(id)) {
        return fused;
    }
    let mut kept = Vec::with_capacity(fused.len());
    let mut demoted = Vec::new();
    for (position, id) in fused.into_iter().enumerate() {
        if position < last_strong && weak(&id) {
            demoted.push(id);
        } else {
            kept.push(id);
        }
    }
    let tail = kept.split_off(
        kept.iter()
            .rposition(|id| strong(id))
            .map_or(kept.len(), |p| p + 1),
    );
    kept.extend(demoted);
    kept.extend(tail);
    kept
}

/// Keyword chunk hits, best first, plus whether any single FTS window came back
/// full (so the caller knows more matches may exist past `fetch`).
pub struct KeywordChunkHits {
    pub hits: Vec<(ConversationChunk, usize, f64)>,
    pub window_full: bool,
}

/// FTS5 chunk hits for `query`: one search per project in `family_projects`
/// (sorted, so the result is deterministic), or one unscoped search when
/// `family_projects` is `None`. Hits are ordered by bm25, then per-window
/// ordinal, then id. A project whose search errors contributes nothing.
/// Uses [`crate::storage::queries::fts5_match_expr`] through
/// [`Storage::fts5_search`], the one FTS query builder.
pub fn keyword_chunk_hits(
    storage: &Storage,
    query: &str,
    fetch: usize,
    family_projects: Option<Vec<&str>>,
) -> KeywordChunkHits {
    let searches = match family_projects {
        Some(mut projects) => {
            projects.sort_unstable();
            projects.dedup();
            // One ranking pass for the whole family; a failure falls back to
            // the search per project, where a failing project contributes
            // nothing and the others still answer.
            match storage.fts5_search_projects(query, fetch, &projects) {
                Ok(windows) => windows.into_iter().map(Ok).collect::<Vec<_>>(),
                Err(_) => projects
                    .into_iter()
                    .map(|project| storage.fts5_search(query, fetch, Some(project)))
                    .collect::<Vec<_>>(),
            }
        }
        None => vec![storage.fts5_search(query, fetch, None)],
    };
    let mut hits = Vec::new();
    let mut window_full = false;
    for window in searches.into_iter().flatten() {
        window_full |= window.len() == fetch;
        hits.extend(window);
    }
    hits.sort_by(|left, right| {
        left.2
            .total_cmp(&right.2)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.0.id.cmp(&right.0.id))
    });
    KeywordChunkHits { hits, window_full }
}

/// Hook keyword channel bounds. The MCP channel ORs every word of the query and
/// ranks every matching row, which on a long prompt means ranking most of the
/// table (537-821 ms p95 against a 755k-chunk store). The hook runs on every
/// prompt, so it asks for the few words that single out a chunk instead.
///
/// Words shorter than this are not candidates.
pub const HOOK_MIN_TERM_CHARS: usize = 5;
/// Candidate words probed for document frequency, identifier-shaped first.
pub const HOOK_MAX_PROBES: usize = 12;
/// A word in more chunks than this is common, not specific: it is dropped.
pub const HOOK_TERM_DOC_CAP: usize = 1500;
/// Words kept for the match expression, rarest first. Each matches at most
/// [`HOOK_TERM_DOC_CAP`] chunks, so the rows ranked per prompt stay bounded.
pub const HOOK_MAX_TERMS: usize = 4;

/// Whether `word` looks like a name rather than prose: it carries an
/// underscore, a hyphen or a digit, or a lowercase letter followed by a capital.
fn identifier_shaped(word: &str) -> bool {
    word.chars()
        .any(|c| c == '_' || c == '-' || c.is_ascii_digit())
        || word
            .chars()
            .zip(word.chars().skip(1))
            .any(|(a, b)| a.is_lowercase() && b.is_uppercase())
}

/// The words of `words` worth a keyword search: candidates are the words of at
/// least [`HOOK_MIN_TERM_CHARS`] characters, once each (case-insensitively),
/// identifier-shaped before prose and longer before shorter, at most
/// [`HOOK_MAX_PROBES`] of them. `doc_count` returns how many chunks contain a
/// word (counted up to [`HOOK_TERM_DOC_CAP`] + 1), or `None` when it could not
/// be counted. Words in no chunk, in more than the cap, or uncounted are
/// dropped; the [`HOOK_MAX_TERMS`] rarest remain, rarest first.
pub fn select_rare_terms(
    words: &[String],
    mut doc_count: impl FnMut(&str) -> Option<usize>,
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut candidates: Vec<&String> = words
        .iter()
        .filter(|w| w.chars().count() >= HOOK_MIN_TERM_CHARS)
        .filter(|w| seen.insert(w.to_lowercase()))
        .collect();
    candidates.sort_by_key(|w| (!identifier_shaped(w), std::cmp::Reverse(w.chars().count())));
    candidates.truncate(HOOK_MAX_PROBES);
    let mut counted: Vec<(usize, &String)> = candidates
        .into_iter()
        .filter_map(|w| doc_count(w).map(|n| (n, w)))
        .filter(|(n, _)| (1..=HOOK_TERM_DOC_CAP).contains(n))
        .collect();
    counted.sort_by_key(|(n, w)| (*n, (*w).clone()));
    counted
        .into_iter()
        .take(HOOK_MAX_TERMS)
        .map(|(_, w)| w.clone())
        .collect()
}

/// Keyword chunk hits for the prompt hook: the same FTS5 table, phrase quoting
/// and bm25 order as [`keyword_chunk_hits`], over the few rare words of the
/// prompt ([`select_rare_terms`]) instead of every word. No rare word, no
/// keyword hits. `project` scopes the search when given.
pub fn hook_keyword_chunk_hits(
    storage: &Storage,
    query: &str,
    fetch: usize,
    project: Option<&str>,
) -> KeywordChunkHits {
    let words = crate::storage::queries::fts5_query_words(query);
    let terms = select_rare_terms(&words, |w| {
        storage.fts5_chunk_doc_count(w, HOOK_TERM_DOC_CAP + 1).ok()
    });
    let none = KeywordChunkHits {
        hits: Vec::new(),
        window_full: false,
    };
    if terms.is_empty() {
        return none;
    }
    let expr = crate::storage::queries::fts5_or_expr(&terms);
    match storage.fts5_search_expr(&expr, fetch, project) {
        Ok(hits) => KeywordChunkHits {
            window_full: hits.len() == fetch,
            hits,
        },
        Err(_) => none,
    }
}

/// Score of a chunk reached only through the keyword channel: the base score
/// decayed by the chunk's age, [`FTS_ONLY_UNDATED_SCORE`] when the timestamp
/// does not parse. This is the MCP path's undemoted score; the MCP path layers
/// its validity and release-ancestry signals on top of the same base.
pub fn keyword_only_score(timestamp: &str, now: &chrono::DateTime<chrono::Utc>) -> f32 {
    let Ok(timestamp) = timestamp.parse::<chrono::DateTime<chrono::Utc>>() else {
        return FTS_ONLY_UNDATED_SCORE;
    };
    crate::search::decay::apply_decay_with_release_ancestry(
        FTS_ONLY_BASE_SCORE,
        &timestamp,
        now,
        None,
        None,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn mode_defaults_to_hybrid() {
        assert_eq!(search_mode_from(None), SearchMode::Hybrid);
        assert_eq!(search_mode_from(Some("anything")), SearchMode::Hybrid);
        assert_eq!(search_mode_from(Some("vector")), SearchMode::Vector);
        assert_eq!(search_mode_from(Some("fts")), SearchMode::Fts);
    }

    #[test]
    fn fusion_rewards_agreement_between_rankings() {
        let fused = fuse_rrf(&ids(&["a", "b", "c"]), &ids(&["c", "x"]), RRF_K);
        assert_eq!(fused[0], "c", "{fused:?}");
        assert!(fused.contains(&"x".to_string()));
        assert_eq!(fused.len(), 4);
    }

    fn cosines<'a>(rows: &[(&'a str, f32)]) -> HashMap<&'a str, f32> {
        rows.iter().copied().collect()
    }

    #[test]
    fn a_weak_row_the_keyword_lane_lists_goes_below_the_strong_vector_hits() {
        // `weak` fuses first (listed by both lanes) but its cosine is 0.3 of the best.
        let fused = ids(&["weak", "strong-a", "strong-b", "only-vector"]);
        let fts = ids(&["weak"]);
        let cos = cosines(&[
            ("weak", 0.27),
            ("strong-a", 0.9),
            ("strong-b", 0.8),
            ("only-vector", 0.7),
        ]);
        let out = demote_weakly_similar_keyword_rows(fused, &fts, &cos, |_| false, 0.6);
        assert_eq!(out, ids(&["strong-a", "strong-b", "only-vector", "weak"]));
    }

    #[test]
    fn a_keyword_only_chunk_has_no_cosine_and_keeps_its_place() {
        let fused = ids(&["needle", "strong"]);
        let fts = ids(&["needle"]);
        let cos = cosines(&[("strong", 0.9)]);
        let out = demote_weakly_similar_keyword_rows(fused.clone(), &fts, &cos, |_| false, 0.6);
        assert_eq!(out, fused);
    }

    #[test]
    fn a_keyword_only_reflection_goes_below_the_strong_hits() {
        let fused = ids(&["note", "strong"]);
        let fts = ids(&["note"]);
        let cos = cosines(&[("strong", 0.9)]);
        let out = demote_weakly_similar_keyword_rows(fused, &fts, &cos, |id| id == "note", 0.6);
        assert_eq!(out, ids(&["strong", "note"]));
    }

    #[test]
    fn a_weak_row_the_keyword_lane_does_not_list_is_left_alone() {
        let fused = ids(&["weak-vector", "strong"]);
        let cos = cosines(&[("weak-vector", 0.2), ("strong", 0.9)]);
        let out = demote_weakly_similar_keyword_rows(fused.clone(), &[], &cos, |_| false, 0.6);
        assert_eq!(out, fused);
    }

    #[test]
    fn the_floor_keeps_every_row_and_the_order_inside_each_group() {
        let fused = ids(&["w1", "s1", "w2", "s2", "w3", "tail"]);
        let fts = ids(&["w1", "w2", "w3"]);
        let cos = cosines(&[
            ("w1", 0.1),
            ("w2", 0.2),
            ("w3", 0.3),
            ("s1", 0.9),
            ("s2", 0.8),
            ("tail", 0.1),
        ]);
        let out = demote_weakly_similar_keyword_rows(fused.clone(), &fts, &cos, |_| false, 0.6);
        // w3 sits after the last strong row already; w1 and w2 join it there.
        assert_eq!(out, ids(&["s1", "s2", "w1", "w2", "w3", "tail"]));
        let mut sorted_in = fused;
        let mut sorted_out = out;
        sorted_in.sort();
        sorted_out.sort();
        assert_eq!(sorted_in, sorted_out);
    }

    #[test]
    fn no_cosine_in_the_window_means_no_change() {
        let fused = ids(&["a", "b"]);
        let out = demote_weakly_similar_keyword_rows(
            fused.clone(),
            &ids(&["a"]),
            &HashMap::new(),
            |_| true,
            0.6,
        );
        assert_eq!(out, fused);
    }

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn rare_terms_drop_short_common_and_absent_words_and_keep_the_rarest_first() {
        let counts: HashMap<&str, usize> = [
            ("release", 1501),
            ("squash-merge", 127),
            ("trailers", 695),
            ("stripping", 40),
            ("history", 1501),
            ("ghostword", 0),
            ("another", 900),
            ("fourth", 1200),
        ]
        .into_iter()
        .collect();
        let mut probed = Vec::new();
        let kept = select_rare_terms(
            &words("why the release squash-merge history trailers stripping ghostword another fourth abc"),
            |w| {
                probed.push(w.to_string());
                counts.get(w).copied()
            },
        );
        assert_eq!(kept, ["stripping", "squash-merge", "trailers", "another"]);
        // Words under the minimum length are never probed.
        assert!(!probed
            .iter()
            .any(|w| w == "why" || w == "the" || w == "abc"));
    }

    #[test]
    fn rare_terms_probe_identifiers_first_and_never_more_than_the_cap() {
        let mut probed = Vec::new();
        let prose: String = (0..30u8)
            .map(|i| {
                format!(
                    "prose{}{}word ",
                    (b'a' + i / 26) as char,
                    (b'a' + i % 26) as char
                )
            })
            .collect();
        let text = format!("{prose} search_chunks_with_vec_core parseWidget");
        select_rare_terms(&words(&text), |w| {
            probed.push(w.to_string());
            Some(5)
        });
        assert_eq!(probed.len(), HOOK_MAX_PROBES);
        assert!(probed[..2].contains(&"search_chunks_with_vec_core".to_string()));
        assert!(probed[..2].contains(&"parseWidget".to_string()));
    }

    #[test]
    fn rare_terms_skip_a_word_that_could_not_be_counted_and_dedupe_case() {
        let kept = select_rare_terms(&words("Widget widget WIDGET broken"), |w| {
            (w.to_lowercase() == "widget").then_some(3)
        });
        assert_eq!(kept, ["Widget"]);
    }

    #[test]
    fn keyword_only_score_is_the_decayed_base_and_undated_floor() {
        let now = chrono::Utc::now();
        let fresh = keyword_only_score(&now.to_rfc3339(), &now);
        assert!(fresh > 0.40 && fresh <= FTS_ONLY_BASE_SCORE, "{fresh}");
        let old = keyword_only_score(&(now - chrono::Duration::days(60)).to_rfc3339(), &now);
        assert!(old < fresh, "{old} < {fresh}");
        assert_eq!(
            keyword_only_score("not a date", &now),
            FTS_ONLY_UNDATED_SCORE
        );
    }
}
