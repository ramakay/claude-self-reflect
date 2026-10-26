//! Hybrid retrieval primitives shared by the MCP search path and the
//! `UserPromptSubmit` hook: the search mode switch, reciprocal-rank fusion of
//! the vector and keyword rankings, the keyword (FTS5) chunk channel, and the
//! synthetic score a keyword-only chunk carries.
//!
//! One copy. `mcp::tools` and `hooks::prompt_submit` both call these; neither
//! carries its own fusion.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::import::ConversationChunk;
use crate::search::identifiers::QueryIdent;
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
    fuse_rrf_weighted(&[(semantic_order, 1.0), (fts_order, 1.0)], k)
}

/// Fuse any number of rankings (best first), each with a channel weight, by
/// reciprocal rank: `score(id) = sum over lists of weight / (k + rank)`, rank
/// 0-based, the first occurrence of an id in a list counting. A list with a
/// weight of zero or less is skipped outright, so an inactive channel adds no
/// ids. Ties keep first-seen order, earlier list first.
pub fn fuse_rrf_weighted(lists: &[(&[String], f64)], k: usize) -> Vec<String> {
    let mut fused_order = Vec::new();
    let mut scores: HashMap<String, f64> = HashMap::new();

    for (ranking, weight) in lists {
        if *weight <= 0.0 {
            continue;
        }
        let mut seen = HashSet::new();
        for (rank, id) in ranking.iter().enumerate() {
            if !seen.insert(id.as_str()) {
                continue;
            }
            if !scores.contains_key(id) {
                fused_order.push(id.clone());
            }
            *scores.entry(id.clone()).or_default() += weight / (k + rank) as f64;
        }
    }

    fused_order.sort_by(|left, right| scores[right].total_cmp(&scores[left]));
    fused_order
}

/// Weight of the identifier channel when a rare, name-shaped term of the query
/// (weight 3, document frequency up to [`ID_HEAVY_DF_MAX`]) matched.
pub const ID_CHANNEL_HEAVY_WEIGHT: f64 = 1.5;
/// Weight of the identifier channel when some identifier matched but none was
/// rare enough to be a strong signal.
pub const ID_CHANNEL_BASE_WEIGHT: f64 = 1.0;
/// A weight-3 identifier in more chunks than this is common, not specific.
pub const ID_HEAVY_DF_MAX: usize = 5000;

/// Weight of the postings channel for a query. `dfs[i]` is the number of
/// postings of `idents[i]` (a missing entry counts as 0): 1.5 when any weight-3
/// identifier has `0 < df <= 5000`, 1.0 when any identifier matched at all,
/// 0.0 otherwise (no identifiers, none in the table, or no table).
pub fn id_channel_weight(idents: &[QueryIdent], dfs: &[usize]) -> f64 {
    let mut weight = 0.0;
    for (index, ident) in idents.iter().enumerate() {
        let df = dfs.get(index).copied().unwrap_or(0);
        if df == 0 {
            continue;
        }
        weight = ID_CHANNEL_BASE_WEIGHT;
        if ident.weight >= 3.0 && df <= ID_HEAVY_DF_MAX {
            return ID_CHANNEL_HEAVY_WEIGHT;
        }
    }
    weight
}

/// Session-level reciprocal-rank fusion. A session's rank in a channel is the
/// rank of its best chunk there, so a session scores
/// `sum over channels of weight / (k + best rank)`; rank is 0-based like
/// [`fuse_rrf_weighted`]. Returns one chunk id per session, best session
/// first: the session's chunk with the best chunk-level fused score.
///
/// `chunk_lists` go weakest channel first (semantic, keyword, postings). Two
/// sessions with equal scores keep first-seen order, earlier list first, as
/// [`fuse_rrf`] does. Two chunks of one session with equal scores go to the one
/// reached by the later list: postings over keyword over semantic. A list with
/// a weight of zero or less is skipped. A chunk in no list is not returned.
pub fn collapse_to_sessions(
    chunk_lists: &[(&[String], f64)],
    session_of: impl Fn(&str) -> String,
    k: usize,
) -> Vec<String> {
    collapse_to_sessions_by(chunk_lists, session_of, |_| 0, k)
}

/// [`collapse_to_sessions`] with a standing for each chunk: within a session
/// only the chunks of the lowest `class` present count. They alone represent
/// the session and set its rank in each channel, so a session's echoes of the
/// question cannot lift the answer that stands for it. Fused score and list
/// decide among chunks of that class. The search path passes the class of a row
/// that carries no answer (a question echo, CSR's own output); a session with
/// only such rows is still ranked by them.
pub fn collapse_to_sessions_by(
    chunk_lists: &[(&[String], f64)],
    session_of: impl Fn(&str) -> String,
    class_of: impl Fn(&str) -> u8,
    k: usize,
) -> Vec<String> {
    let mut session_cache: HashMap<&str, String> = HashMap::new();
    let mut first_seen: Vec<&str> = Vec::new();
    let mut chunk_score: HashMap<&str, f64> = HashMap::new();
    // The latest list a chunk appears in: the tie-break between its siblings.
    let mut latest_list: HashMap<&str, usize> = HashMap::new();
    let mut placements: Vec<(usize, usize, &str)> = Vec::new();

    for (index, (ranking, weight)) in chunk_lists.iter().enumerate() {
        if *weight <= 0.0 {
            continue;
        }
        let mut seen = HashSet::new();
        for (rank, id) in ranking.iter().enumerate() {
            if !seen.insert(id.as_str()) {
                continue;
            }
            session_cache
                .entry(id.as_str())
                .or_insert_with(|| session_of(id));
            if !chunk_score.contains_key(id.as_str()) {
                first_seen.push(id.as_str());
            }
            *chunk_score.entry(id.as_str()).or_default() += weight / (k + rank) as f64;
            latest_list.insert(id.as_str(), index);
            placements.push((index, rank, id.as_str()));
        }
    }

    let class: HashMap<&str, u8> = first_seen.iter().map(|id| (*id, class_of(id))).collect();
    let mut session_class: HashMap<&str, u8> = HashMap::new();
    for id in &first_seen {
        let slot = session_class
            .entry(session_cache[*id].as_str())
            .or_insert(class[*id]);
        *slot = (*slot).min(class[*id]);
    }

    let mut session_order: Vec<&str> = Vec::new();
    let mut representative: HashMap<&str, &str> = HashMap::new();
    for id in &first_seen {
        let session = session_cache[*id].as_str();
        if class[*id] != session_class[session] {
            continue;
        }
        match representative.get(session).copied() {
            None => {
                session_order.push(session);
                representative.insert(session, *id);
            }
            Some(current) => {
                let challenger = (chunk_score[*id], latest_list[*id]);
                let holder = (chunk_score[current], latest_list[current]);
                if challenger.0 > holder.0 || (challenger.0 == holder.0 && challenger.1 > holder.1)
                {
                    representative.insert(session, *id);
                }
            }
        }
    }

    // Ordered, so a session's channel terms always add up in list order.
    let mut best_rank: BTreeMap<(usize, &str), usize> = BTreeMap::new();
    for (index, rank, id) in placements {
        let session = session_cache[id].as_str();
        if class[id] != session_class[session] {
            continue;
        }
        best_rank
            .entry((index, session))
            .and_modify(|best| *best = (*best).min(rank))
            .or_insert(rank);
    }
    let mut session_score: HashMap<&str, f64> = HashMap::new();
    for ((index, session), rank) in &best_rank {
        let weight = chunk_lists[*index].1;
        *session_score.entry(session).or_default() += weight / (k + rank) as f64;
    }

    session_order.sort_by(|left, right| session_score[right].total_cmp(&session_score[left]));
    session_order
        .into_iter()
        .map(|session| representative[session].to_string())
        .collect()
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

    fn ident(term: &str, weight: f32) -> QueryIdent {
        QueryIdent {
            term: term.to_string(),
            kind: crate::search::identifiers::IdentKind::Symbol,
            weight,
        }
    }

    #[test]
    fn weighted_fusion_of_three_lists_sums_weighted_contributions() {
        let semantic = ids(&["a", "b", "c"]);
        let fts = ids(&["b", "d"]);
        let postings = ids(&["d", "b"]);
        let fused = fuse_rrf_weighted(&[(&semantic, 1.0), (&fts, 1.0), (&postings, 1.5)], RRF_K);
        // b: 1/61 + 1/60 + 1.5/61; d: 1/61 + 1.5/60; a: 1/60.
        assert_eq!(fused, ids(&["b", "d", "a", "c"]), "{fused:?}");
    }

    #[test]
    fn weighted_fusion_with_unit_weights_equals_the_two_list_fusion() {
        let semantic = ids(&["a", "b", "c", "x"]);
        let fts = ids(&["c", "x", "y", "a"]);
        assert_eq!(
            fuse_rrf(&semantic, &fts, RRF_K),
            fuse_rrf_weighted(&[(&semantic, 1.0), (&fts, 1.0)], RRF_K)
        );
    }

    #[test]
    fn a_postings_only_id_reaches_rank_one_when_first_in_a_heavy_channel() {
        let semantic = ids(&["a", "b", "c"]);
        let fts = ids(&["x", "y"]);
        let postings = ids(&["p"]);
        let fused = fuse_rrf_weighted(&[(&semantic, 1.0), (&fts, 1.0), (&postings, 1.5)], RRF_K);
        assert_eq!(fused[0], "p", "{fused:?}");
    }

    #[test]
    fn a_zero_weight_channel_adds_nothing() {
        let semantic = ids(&["a", "b"]);
        let postings = ids(&["z"]);
        let fused = fuse_rrf_weighted(&[(&semantic, 1.0), (&postings, 0.0)], RRF_K);
        assert_eq!(fused, ids(&["a", "b"]));
    }

    #[test]
    fn channel_weight_is_zero_without_identifiers_or_matches() {
        assert_eq!(id_channel_weight(&[], &[]), 0.0);
        let idents = [ident("parse_widget", 3.0), ident("api", 2.0)];
        assert_eq!(id_channel_weight(&idents, &[0, 0]), 0.0);
        assert_eq!(id_channel_weight(&idents, &[]), 0.0);
    }

    #[test]
    fn channel_weight_is_heavy_only_for_a_rare_weight_three_identifier() {
        let idents = [ident("parse_widget", 3.0), ident("api", 2.0)];
        assert_eq!(id_channel_weight(&idents, &[5000, 0]), 1.5);
        assert_eq!(id_channel_weight(&idents, &[5001, 0]), 1.0);
        // A rare acronym alone is a match, not a heavy one.
        assert_eq!(id_channel_weight(&idents, &[0, 3]), 1.0);
        assert_eq!(id_channel_weight(&idents, &[0, 3]), ID_CHANNEL_BASE_WEIGHT);
        // The heavy one wins wherever it sits.
        assert_eq!(id_channel_weight(&idents, &[7, 9000]), 1.5);
    }

    fn session_by_prefix(id: &str) -> String {
        id.split(':').next().unwrap_or(id).to_string()
    }

    #[test]
    fn collapse_returns_one_row_per_session_with_its_best_chunk() {
        let semantic = ids(&["s1:a", "s2:a", "s1:b", "s1:c"]);
        let fts = ids(&["s2:b", "s1:b"]);
        let collapsed =
            collapse_to_sessions(&[(&semantic, 1.0), (&fts, 1.0)], session_by_prefix, RRF_K);
        assert_eq!(collapsed.len(), 2, "{collapsed:?}");
        // s1: best semantic rank 0, best fts rank 1. s2: semantic 1, fts 0.
        // Both score 1/60 + 1/61: a tie, first seen (semantic) first.
        assert_eq!(session_by_prefix(&collapsed[0]), "s1");
        assert_eq!(session_by_prefix(&collapsed[1]), "s2");
        // s1's best chunk is the one in both lists: s1:b (1/62 + 1/61).
        assert_eq!(collapsed[0], "s1:b");
        // s2's: s2:b (1/60) beats s2:a (1/61).
        assert_eq!(collapsed[1], "s2:b");
    }

    #[test]
    fn collapse_ranks_a_session_by_its_best_chunk_in_each_channel() {
        let semantic = ids(&["x:1", "y:1", "y:2", "y:3"]);
        let fts = ids(&["y:2", "x:2"]);
        // x: semantic 0, fts 1. y: semantic 1, fts 0. Equal weights tie.
        let tied = collapse_to_sessions(&[(&semantic, 1.0), (&fts, 1.0)], session_by_prefix, RRF_K);
        assert_eq!(tied, ids(&["x:1", "y:2"]));
        let fts_heavy =
            collapse_to_sessions(&[(&semantic, 1.0), (&fts, 2.0)], session_by_prefix, RRF_K);
        assert_eq!(fts_heavy, ids(&["y:2", "x:2"]));
        let semantic_heavy =
            collapse_to_sessions(&[(&semantic, 2.0), (&fts, 1.0)], session_by_prefix, RRF_K);
        assert_eq!(semantic_heavy, ids(&["x:1", "y:2"]));
    }

    #[test]
    fn collapse_with_one_chunk_per_session_equals_the_flat_fusion() {
        let semantic = ids(&["a", "b", "c", "x"]);
        let fts = ids(&["c", "x", "y", "a"]);
        let postings = ids(&["y"]);
        let lists: [(&[String], f64); 3] = [(&semantic, 1.0), (&fts, 1.0), (&postings, 1.5)];
        assert_eq!(
            collapse_to_sessions(&lists, |id| id.to_string(), RRF_K),
            fuse_rrf_weighted(&lists, RRF_K)
        );
    }

    #[test]
    fn collapse_ties_inside_a_session_go_to_postings_then_keyword_then_semantic() {
        let semantic = ids(&["q:sem"]);
        let fts = ids(&["q:fts"]);
        let postings = ids(&["q:post"]);
        let all = collapse_to_sessions(
            &[(&semantic, 1.0), (&fts, 1.0), (&postings, 1.0)],
            session_by_prefix,
            RRF_K,
        );
        assert_eq!(all, ids(&["q:post"]));
        let no_postings =
            collapse_to_sessions(&[(&semantic, 1.0), (&fts, 1.0)], session_by_prefix, RRF_K);
        assert_eq!(no_postings, ids(&["q:fts"]));
    }

    #[test]
    fn collapse_by_class_lets_an_answer_represent_its_session_over_a_better_ranked_echo() {
        let semantic = ids(&["q:echo", "q:answer", "r:only"]);
        let fts = ids(&["q:echo"]);
        let lists: [(&[String], f64); 2] = [(&semantic, 1.0), (&fts, 1.0)];
        let plain = collapse_to_sessions(&lists, session_by_prefix, RRF_K);
        assert_eq!(plain, ids(&["q:echo", "r:only"]));
        let by_class = collapse_to_sessions_by(
            &lists,
            session_by_prefix,
            |id| u8::from(id.ends_with(":echo")),
            RRF_K,
        );
        assert_eq!(by_class, ids(&["q:answer", "r:only"]));
        // The echo's rank does not lift its session: the answer stands at rank
        // 2, behind r:only at rank 1, though the echo is at rank 0.
        let lone = ids(&["q:echo", "r:only", "q:answer"]);
        let outranked = collapse_to_sessions_by(
            &[(&lone, 1.0)],
            session_by_prefix,
            |id| u8::from(id.ends_with(":echo")),
            RRF_K,
        );
        assert_eq!(outranked, ids(&["r:only", "q:answer"]));
    }

    #[test]
    fn collapse_never_merges_sessions_that_have_their_own_key() {
        // Each reflection is its own session: the key is the id itself.
        let semantic = ids(&["r1", "r2", "c:1"]);
        let fts = ids(&["r2", "c:2"]);
        let collapsed = collapse_to_sessions(
            &[(&semantic, 1.0), (&fts, 1.0)],
            |id| {
                if id.starts_with('r') {
                    id.to_string()
                } else {
                    session_by_prefix(id)
                }
            },
            RRF_K,
        );
        assert_eq!(collapsed.len(), 3, "{collapsed:?}");
        assert!(collapsed.contains(&"r1".to_string()));
        assert!(collapsed.contains(&"r2".to_string()));
    }

    #[test]
    fn collapse_of_empty_or_zero_weight_lists_is_empty() {
        assert!(collapse_to_sessions(&[], session_by_prefix, RRF_K).is_empty());
        let only = ids(&["a:1"]);
        assert!(collapse_to_sessions(&[(&only, 0.0)], session_by_prefix, RRF_K).is_empty());
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
