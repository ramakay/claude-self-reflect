//! Multi-signal relevance scorer for search results.
//!
//! Raw semantic similarity alone isn't enough — recency, file overlap,
//! and error pattern matching improve relevance for injection.
//!
//! Scoring formula: weighted combination via LAPI phase profiles (see weights.rs).
//! Recency decay: 2^(-age/14) — 14-day half-life to suppress stale results.

use std::collections::HashSet;

/// A search result enriched with multi-signal scoring.
#[derive(Debug, Clone)]
pub struct ScoredResult {
    pub content: String,
    pub raw_score: f32,
    pub final_score: f32,
    pub source: String,
    pub signals: Vec<Signal>,
    /// Stable storage ID for outcome tracking (carried from RawResult).
    pub memory_id: Option<String>,
}

/// What contributed to a result's score.
#[derive(Debug, Clone)]
pub enum Signal {
    SemanticMatch(f32),
    RecencyBoost(f32),
    FileOverlap(f32),
    ErrorPatternMatch(f32),
    /// Session continuity boost — result is from the immediately prior session.
    ContinuityBoost(f32),
    /// Release-tier ancestry tightened the chunk's recency half-life.
    AncestryDecay(u32),
}

/// Raw result from HNSW search, before scoring.
#[derive(Debug, Clone)]
pub struct RawResult {
    pub content: String,
    pub score: f32,
    pub source: String,
    /// ISO timestamp of the result (for recency scoring).
    pub timestamp: Option<String>,
    /// Files mentioned in this result.
    pub files: Vec<String>,
    /// Error signatures in this result.
    pub error_patterns: Vec<String>,
    /// Tags from storage (for phase boost scoring).
    pub tags: Vec<String>,
    /// Conversation ID this result came from (for session continuity boost).
    pub conversation_id: Option<String>,
    /// Stable storage ID (chunk_id or reflection_id) for outcome tracking.
    pub memory_id: Option<String>,
}

/// Score and rank results for injection.
///
/// Applies multi-signal scoring with LAPI (Lifecycle-Aware Predictive Injection):
/// When `phase` is `Some`, uses phase-specific weight profiles.
/// When `None`, uses PromptSubmit weights (backward compatible with existing callers).
pub fn rank_results(
    results: Vec<RawResult>,
    current_files: &[String],
    current_errors: &[String],
    phase: Option<super::weights::HookPhase>,
) -> Vec<ScoredResult> {
    rank_results_with_continuity(results, current_files, current_errors, phase, None)
}

/// Like `rank_results`, but with an optional session continuity boost.
/// Results whose `conversation_id` matches `continued_session_id` get a 1.5x score multiplier.
pub fn rank_results_with_continuity(
    results: Vec<RawResult>,
    current_files: &[String],
    current_errors: &[String],
    phase: Option<super::weights::HookPhase>,
    continued_session_id: Option<&str>,
) -> Vec<ScoredResult> {
    rank_results_with_continuity_and_ancestry(
        results,
        current_files,
        current_errors,
        phase,
        continued_session_id,
        &std::collections::HashMap::new(),
    )
}

/// Prompt-submit ranking with precomputed release ancestry. The map is keyed
/// by chunk id and is populated only for chunk results; reflections
/// retain their existing recency behavior.
pub fn rank_results_with_continuity_and_ancestry(
    results: Vec<RawResult>,
    current_files: &[String],
    current_errors: &[String],
    phase: Option<super::weights::HookPhase>,
    continued_session_id: Option<&str>,
    ancestry_releases: &std::collections::HashMap<String, u32>,
) -> Vec<ScoredResult> {
    let weights = phase
        .map(super::weights::WeightProfile::for_phase)
        .unwrap_or(super::weights::WeightProfile::for_phase(
            super::weights::HookPhase::PromptSubmit,
        ));
    let phase = phase.unwrap_or(super::weights::HookPhase::PromptSubmit);

    let mut scored: Vec<ScoredResult> = results
        .into_iter()
        .map(|r| {
            let is_continued = continued_session_id.is_some()
                && r.conversation_id.as_deref() == continued_session_id;
            let releases_behind = if crate::search::rerank::is_scaffold_text(&r.content) {
                None
            } else {
                r.memory_id
                    .as_ref()
                    .and_then(|id| ancestry_releases.get(id))
                    .copied()
            };
            let mut result = score_result(
                r,
                current_files,
                current_errors,
                &weights,
                phase,
                releases_behind,
            );
            if is_continued {
                result
                    .signals
                    .push(Signal::ContinuityBoost(CONTINUITY_MULTIPLIER));
                result.final_score *= CONTINUITY_MULTIPLIER;
            }
            result
        })
        .collect();

    scored.sort_by(|a, b| {
        b.final_score
            .partial_cmp(&a.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    scored
}

/// Multiplier for results from the immediately prior session (within continuity threshold).
const CONTINUITY_MULTIPLIER: f32 = 1.5;

fn score_result(
    result: RawResult,
    current_files: &[String],
    current_errors: &[String],
    weights: &super::weights::WeightProfile,
    phase: super::weights::HookPhase,
    ancestry_releases_behind: Option<u32>,
) -> ScoredResult {
    let mut signals = Vec::new();

    // 1. Semantic match (raw HNSW score, already 0.0-1.0)
    let semantic = result.score;
    signals.push(Signal::SemanticMatch(semantic));

    // 2. Recency boost (0.0-1.0 based on age)
    let (recency, ancestry_applied) =
        compute_recency_boost_with_ancestry(result.timestamp.as_deref(), ancestry_releases_behind);
    signals.push(Signal::RecencyBoost(recency));
    if ancestry_applied {
        let releases = ancestry_releases_behind.expect("applied ancestry has a release distance");
        signals.push(Signal::AncestryDecay(releases));
    }

    // 3. File overlap (fraction of current files mentioned in result)
    let file_overlap = compute_file_overlap(&result.files, current_files);
    signals.push(Signal::FileOverlap(file_overlap));

    // 4. Error pattern match (any current errors appear in result)
    let error_match = compute_error_match(&result.error_patterns, current_errors);
    signals.push(Signal::ErrorPatternMatch(error_match));

    // 5. Phase boost (LAPI: how well does this result type match the hook phase)
    let phase_boost = super::weights::compute_phase_boost(&result.source, &result.tags, phase);

    // Weighted combination using LAPI weight profile
    let final_score = semantic * weights.semantic
        + recency * weights.recency
        + file_overlap * weights.file_overlap
        + error_match * weights.error_match
        + phase_boost * weights.phase_boost;

    ScoredResult {
        content: result.content,
        raw_score: result.score,
        final_score,
        source: result.source,
        signals,
        memory_id: result.memory_id,
    }
}

/// Post-scoring outcome multiplier (gated, bounded).
/// Only applies when a memory has >= 3 non-neutral retrieval events.
/// Positive outcomes boost score, negative outcomes penalize, bounded to +/-10%.
pub fn apply_outcome_multiplier(base_score: f32, successes: i64, failures: i64) -> f32 {
    let total = successes + failures;
    if total < 3 {
        return base_score; // Not enough signal — no change
    }
    let ratio = successes as f32 / total as f32; // 0.0 to 1.0
    let delta = (ratio - 0.5) * 0.2; // -0.1 to +0.1
    (base_score * (1.0 + delta)).clamp(0.05, 1.0)
}

/// Compute recency boost: 1.0 for today, decaying with age.
/// Uses formula: 2^(-age_days / 14) — halves every 14 days (steeper decay).
/// Previous: 30-day half-life let 3-month-old results dominate on semantic alone.
/// Now: 14d=0.5, 30d=0.23, 60d=0.05 — stale content gets crushed.
fn compute_recency_boost_with_ancestry(
    timestamp: Option<&str>,
    releases_behind: Option<u32>,
) -> (f32, bool) {
    compute_recency_outcome_at(timestamp, &chrono::Utc::now(), releases_behind)
}

#[cfg(test)]
fn compute_recency_boost_at(
    timestamp: Option<&str>,
    now: &chrono::DateTime<chrono::Utc>,
    releases_behind: Option<u32>,
) -> f32 {
    compute_recency_outcome_at(timestamp, now, releases_behind).0
}

fn compute_recency_outcome_at(
    timestamp: Option<&str>,
    now: &chrono::DateTime<chrono::Utc>,
    releases_behind: Option<u32>,
) -> (f32, bool) {
    let ts = match timestamp {
        Some(ts) => ts,
        None => return (0.5, false), // No timestamp → neutral boost
    };

    let parsed = match crate::temporal::parse_timestamp(ts) {
        Some(dt) => dt,
        None => return (0.5, false),
    };

    let age_days = (*now - parsed).num_days().max(0) as f64;
    let age_multiplier = crate::search::decay::ancestry_age_multiplier(releases_behind);
    let ancestry_applied = age_days > 0.0 && age_multiplier > 1.0;

    // 2^(-age/14): 1.0 today, 0.5 at 14 days, 0.23 at 30 days, 0.05 at 60 days
    (
        (2.0_f64.powf(-(age_days * age_multiplier) / 14.0)) as f32,
        ancestry_applied,
    )
}

/// Compute file overlap: fraction of current session files that appear in result.
fn compute_file_overlap(result_files: &[String], current_files: &[String]) -> f32 {
    if current_files.is_empty() || result_files.is_empty() {
        return 0.0;
    }

    let overlap_count = current_files
        .iter()
        .filter(|cf| result_files.iter().any(|rf| files_match(rf, cf)))
        .count();

    (overlap_count as f32) / (current_files.len() as f32)
}

/// Check if two file paths refer to the same file.
/// Compares filename + parent directory (2 components) to avoid false positives
/// on common names like mod.rs, index.ts, __init__.py.
fn files_match(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    // Compare last 2 path components (parent/filename) for better accuracy
    let a_parts: Vec<&str> = a.rsplit('/').take(2).collect();
    let b_parts: Vec<&str> = b.rsplit('/').take(2).collect();
    // If both have 2+ components, compare parent/filename
    if a_parts.len() == 2 && b_parts.len() == 2 {
        return a_parts == b_parts;
    }
    // If one is just a filename, compare filenames only
    let a_name = a.rsplit('/').next().unwrap_or(a);
    let b_name = b.rsplit('/').next().unwrap_or(b);
    a_name == b_name
}

/// Compute error pattern match with graduated scoring.
/// Returns: containment 0.7-1.0, word overlap 0.0-0.7, no match 0.0.
fn compute_error_match(result_errors: &[String], current_errors: &[String]) -> f32 {
    if current_errors.is_empty() || result_errors.is_empty() {
        return 0.0;
    }

    let mut best_score: f32 = 0.0;
    for ce in current_errors {
        let ce_lower = ce.to_lowercase();
        let ce_words: HashSet<&str> = split_error_words(&ce_lower);
        for re in result_errors {
            let re_lower = re.to_lowercase();

            if re_lower.contains(&ce_lower) || ce_lower.contains(&re_lower) {
                let shorter = ce_lower.len().min(re_lower.len());
                let longer = ce_lower.len().max(re_lower.len());
                if longer > 0 {
                    best_score = best_score.max(0.7 + 0.3 * (shorter as f32 / longer as f32));
                }
            } else {
                let re_words: HashSet<&str> = split_error_words(&re_lower);
                let overlap = ce_words.intersection(&re_words).count();
                let total = ce_words.len().max(re_words.len());
                if total > 0 {
                    best_score = best_score.max(overlap as f32 / total as f32);
                }
            }
        }
    }
    best_score
}

/// Split error strings into words on whitespace, underscores, hyphens, colons.
fn split_error_words(s: &str) -> HashSet<&str> {
    s.split(|c: char| c.is_whitespace() || c == '_' || c == '-' || c == ':')
        .filter(|w| !w.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_semantic_only_scoring() {
        let results = vec![
            RawResult {
                content: "high match".into(),
                score: 0.9,
                source: "chunk".into(),
                timestamp: None,
                files: vec![],
                error_patterns: vec![],
                tags: vec![],
                conversation_id: None,
                memory_id: None,
            },
            RawResult {
                content: "low match".into(),
                score: 0.5,
                source: "chunk".into(),
                timestamp: None,
                files: vec![],
                error_patterns: vec![],
                tags: vec![],
                conversation_id: None,
                memory_id: None,
            },
        ];

        let scored = rank_results(results, &[], &[], None);
        assert_eq!(scored.len(), 2);
        assert!(scored[0].final_score > scored[1].final_score);
        assert_eq!(scored[0].content, "high match");
    }

    #[test]
    fn released_chunk_uses_stronger_recency_while_neutral_paths_are_bit_identical() {
        let now = chrono::Utc.with_ymd_and_hms(2026, 8, 6, 12, 0, 0).unwrap();
        let timestamp = (now - chrono::Duration::days(14)).to_rfc3339();
        let pre_change = compute_recency_boost_at(Some(&timestamp), &now, None);
        let released = compute_recency_boost_at(Some(&timestamp), &now, Some(5));
        let missing = compute_recency_boost_at(Some(&timestamp), &now, None);
        let current_release = compute_recency_boost_at(Some(&timestamp), &now, Some(0));

        assert!(released < pre_change);
        assert_eq!(missing.to_bits(), pre_change.to_bits());
        assert_eq!(current_release.to_bits(), pre_change.to_bits());

        let live_timestamp = (chrono::Utc::now() - chrono::Duration::days(14)).to_rfc3339();
        let raw = |content: &str, conversation_id: &str, chunk_id: &str| RawResult {
            content: content.into(),
            score: 0.8,
            source: "chunk".into(),
            timestamp: Some(live_timestamp.clone()),
            files: vec![],
            error_patterns: vec![],
            tags: vec![],
            conversation_id: Some(conversation_id.into()),
            memory_id: Some(chunk_id.into()),
        };
        let ranked = rank_results_with_continuity_and_ancestry(
            vec![
                raw("released", "conv-released", "chunk-released"),
                raw("fresh", "conv-fresh", "chunk-fresh"),
            ],
            &[],
            &[],
            None,
            None,
            &[("chunk-released".into(), 5)].into_iter().collect(),
        );
        let released = ranked.iter().find(|r| r.content == "released").unwrap();
        let fresh = ranked.iter().find(|r| r.content == "fresh").unwrap();
        assert!(released.final_score < fresh.final_score);
        assert!(released
            .signals
            .iter()
            .any(|signal| matches!(signal, Signal::AncestryDecay(5))));
        assert!(!fresh
            .signals
            .iter()
            .any(|signal| matches!(signal, Signal::AncestryDecay(_))));
    }

    #[test]
    fn scaffold_prompt_candidate_suppresses_release_ancestry() {
        let timestamp = (chrono::Utc::now() - chrono::Duration::days(14)).to_rfc3339();
        let raw = || RawResult {
            content: "<command-message>quoted workflow</command-message>".into(),
            score: 0.8,
            source: "chunk".into(),
            timestamp: Some(timestamp.clone()),
            files: vec![],
            error_patterns: vec![],
            tags: vec![],
            conversation_id: Some("conv-scaffold".into()),
            memory_id: None,
        };
        let pre_change = rank_results(vec![raw()], &[], &[], None);
        let missing = rank_results_with_continuity_and_ancestry(
            vec![raw()],
            &[],
            &[],
            None,
            None,
            &std::collections::HashMap::new(),
        );
        let current_release = rank_results_with_continuity_and_ancestry(
            vec![raw()],
            &[],
            &[],
            None,
            None,
            &[("conv-scaffold".into(), 0)].into_iter().collect(),
        );
        let shipped = rank_results_with_continuity_and_ancestry(
            vec![raw()],
            &[],
            &[],
            None,
            None,
            &[("conv-scaffold".into(), 5)].into_iter().collect(),
        );

        for actual in [&missing, &current_release, &shipped] {
            assert_eq!(
                actual[0].final_score.to_bits(),
                pre_change[0].final_score.to_bits()
            );
            assert!(!actual[0]
                .signals
                .iter()
                .any(|signal| matches!(signal, Signal::AncestryDecay(_))));
        }
    }

    #[test]
    fn prompt_scoring_none_and_current_release_match_pre_ancestry_bits() {
        let timestamp = (chrono::Utc::now() - chrono::Duration::days(14)).to_rfc3339();
        let raw = || RawResult {
            content: "organic conversation".into(),
            score: 0.8,
            source: "chunk".into(),
            timestamp: Some(timestamp.clone()),
            files: vec![],
            error_patterns: vec![],
            tags: vec![],
            conversation_id: Some("conv-neutral".into()),
            memory_id: None,
        };
        let weights = super::super::weights::WeightProfile::for_phase(
            super::super::weights::HookPhase::PromptSubmit,
        );
        let parsed = crate::temporal::parse_timestamp(&timestamp).unwrap();
        let age_days = (chrono::Utc::now() - parsed).num_days().max(0) as f64;
        let recency = (2.0_f64.powf(-age_days / 14.0)) as f32;
        let phase_boost = super::super::weights::compute_phase_boost(
            "chunk",
            &[],
            super::super::weights::HookPhase::PromptSubmit,
        );
        let pre_change = 0.8 * weights.semantic
            + recency * weights.recency
            + 0.0 * weights.file_overlap
            + 0.0 * weights.error_match
            + phase_boost * weights.phase_boost;
        let missing = rank_results_with_continuity_and_ancestry(
            vec![raw()],
            &[],
            &[],
            None,
            None,
            &std::collections::HashMap::new(),
        );
        let current_release = rank_results_with_continuity_and_ancestry(
            vec![raw()],
            &[],
            &[],
            None,
            None,
            &[("conv-neutral".into(), 0)].into_iter().collect(),
        );

        assert_eq!(missing[0].final_score.to_bits(), pre_change.to_bits());
        assert_eq!(
            current_release[0].final_score.to_bits(),
            pre_change.to_bits()
        );
    }

    #[test]
    fn test_recency_boost() {
        let now = chrono::Utc::now().to_rfc3339();
        // Use a timestamp ~60 days ago (clearly old, will have very low recency)
        let old = (chrono::Utc::now() - chrono::Duration::days(60)).to_rfc3339();

        let results = vec![
            RawResult {
                content: "recent".into(),
                score: 0.7,
                source: "chunk".into(),
                timestamp: Some(now),
                files: vec![],
                error_patterns: vec![],
                tags: vec![],
                conversation_id: None,
                memory_id: None,
            },
            RawResult {
                content: "old".into(),
                score: 0.7,
                source: "chunk".into(),
                timestamp: Some(old),
                files: vec![],
                error_patterns: vec![],
                tags: vec![],
                conversation_id: None,
                memory_id: None,
            },
        ];

        let scored = rank_results(results, &[], &[], None);
        assert!(
            scored[0].final_score > scored[1].final_score,
            "recent result should rank higher with equal semantic score"
        );
        assert_eq!(scored[0].content, "recent");
    }

    #[test]
    fn test_file_overlap_boost() {
        let results = vec![
            RawResult {
                content: "with file overlap".into(),
                score: 0.7,
                source: "chunk".into(),
                timestamp: None,
                files: vec!["src/auth.rs".into()],
                error_patterns: vec![],
                tags: vec![],
                conversation_id: None,
                memory_id: None,
            },
            RawResult {
                content: "no overlap".into(),
                score: 0.7,
                source: "chunk".into(),
                timestamp: None,
                files: vec!["src/other.rs".into()],
                error_patterns: vec![],
                tags: vec![],
                conversation_id: None,
                memory_id: None,
            },
        ];

        let current_files = vec!["src/auth.rs".into()];
        let scored = rank_results(results, &current_files, &[], None);
        assert!(scored[0].final_score > scored[1].final_score);
        assert_eq!(scored[0].content, "with file overlap");
    }

    #[test]
    fn test_error_match_boost() {
        let results = vec![
            RawResult {
                content: "matching error".into(),
                score: 0.7,
                source: "reflection".into(),
                timestamp: None,
                files: vec![],
                error_patterns: vec!["connection reset".into()],
                tags: vec![],
                conversation_id: None,
                memory_id: None,
            },
            RawResult {
                content: "no error match".into(),
                score: 0.7,
                source: "reflection".into(),
                timestamp: None,
                files: vec![],
                error_patterns: vec!["out of memory".into()],
                tags: vec![],
                conversation_id: None,
                memory_id: None,
            },
        ];

        let current_errors = vec!["connection reset".into()];
        let scored = rank_results(results, &[], &current_errors, None);
        assert!(scored[0].final_score > scored[1].final_score);
        assert_eq!(scored[0].content, "matching error");
    }

    #[test]
    fn test_files_match_partial_paths() {
        assert!(files_match("src/auth.rs", "src/auth.rs"));
        assert!(files_match("/full/path/auth.rs", "auth.rs")); // one is just filename
        assert!(!files_match("src/auth.rs", "src/other.rs"));
        // 2-component matching avoids mod.rs false positives
        assert!(!files_match("hooks/mod.rs", "search/mod.rs"));
        assert!(files_match("hooks/mod.rs", "hooks/mod.rs"));
    }

    #[test]
    fn test_empty_inputs() {
        let scored = rank_results(vec![], &[], &[], None);
        assert!(scored.is_empty());
    }

    #[test]
    fn test_error_match_exact_containment() {
        let result_errors = vec!["connection reset by peer".into()];
        let current_errors = vec!["connection reset".into()];
        let score = compute_error_match(&result_errors, &current_errors);
        assert!(score >= 0.7, "containment score={score} should be >= 0.7");
        assert!(score <= 1.0, "containment score={score} should be <= 1.0");
    }

    #[test]
    fn test_error_match_word_overlap() {
        let result_errors = vec!["timeout waiting for response from server".into()];
        let current_errors = vec!["timeout error on server connection".into()];
        let score = compute_error_match(&result_errors, &current_errors);
        assert!(score > 0.0, "word overlap score={score} should be > 0.0");
        assert!(score < 0.7, "word overlap score={score} should be < 0.7");
    }

    #[test]
    fn test_error_match_no_overlap() {
        let result_errors = vec!["out of memory".into()];
        let current_errors = vec!["permission denied".into()];
        let score = compute_error_match(&result_errors, &current_errors);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_error_match_empty_inputs() {
        assert_eq!(compute_error_match(&[], &["error".into()]), 0.0);
        assert_eq!(compute_error_match(&["error".into()], &[]), 0.0);
    }

    #[test]
    fn test_continuity_boost_promotes_recent_session() {
        let results = vec![
            RawResult {
                content: "from continued session".into(),
                score: 0.7,
                source: "chunk".into(),
                timestamp: None,
                files: vec![],
                error_patterns: vec![],
                tags: vec![],
                conversation_id: Some("session-abc".into()),
                memory_id: None,
            },
            RawResult {
                content: "from older session".into(),
                score: 0.75,
                source: "chunk".into(),
                timestamp: None,
                files: vec![],
                error_patterns: vec![],
                tags: vec![],
                conversation_id: Some("session-xyz".into()),
                memory_id: None,
            },
        ];

        // Without continuity boost: "from older session" wins (higher raw score)
        let scored_no_boost = rank_results(results.clone(), &[], &[], None);
        assert_eq!(scored_no_boost[0].content, "from older session");

        // With continuity boost: "from continued session" wins (1.5x multiplier)
        let scored_with_boost =
            rank_results_with_continuity(results, &[], &[], None, Some("session-abc"));
        assert_eq!(scored_with_boost[0].content, "from continued session");
        assert!(
            scored_with_boost[0]
                .signals
                .iter()
                .any(|s| matches!(s, Signal::ContinuityBoost(_))),
            "should have ContinuityBoost signal"
        );
    }

    #[test]
    fn test_outcome_multiplier_boosts_successful_memories() {
        let boosted = apply_outcome_multiplier(0.5, 5, 1);
        assert!(boosted > 0.5, "score={boosted} should be > 0.5");
        assert!(boosted < 0.7, "score={boosted} should be < 0.7");
    }

    #[test]
    fn test_outcome_multiplier_penalizes_failed_memories() {
        let penalized = apply_outcome_multiplier(0.5, 1, 5);
        assert!(penalized < 0.5, "score={penalized} should be < 0.5");
        assert!(penalized > 0.3, "score={penalized} should be > 0.3");
    }

    #[test]
    fn test_outcome_multiplier_requires_minimum_events() {
        let unchanged = apply_outcome_multiplier(0.5, 1, 0);
        assert_eq!(unchanged, 0.5);
        let unchanged2 = apply_outcome_multiplier(0.5, 2, 0);
        assert_eq!(unchanged2, 0.5);
    }

    #[test]
    fn test_continuity_boost_no_match() {
        let results = vec![RawResult {
            content: "unrelated session".into(),
            score: 0.7,
            source: "chunk".into(),
            timestamp: None,
            files: vec![],
            error_patterns: vec![],
            tags: vec![],
            conversation_id: Some("session-xyz".into()),
            memory_id: None,
        }];

        let scored = rank_results_with_continuity(results, &[], &[], None, Some("session-abc"));
        // No boost applied — conversation_id doesn't match
        assert!(
            !scored[0]
                .signals
                .iter()
                .any(|s| matches!(s, Signal::ContinuityBoost(_))),
            "should NOT have ContinuityBoost signal for non-matching session"
        );
    }
}
