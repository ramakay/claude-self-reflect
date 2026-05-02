//! Multi-signal relevance scorer for search results.
//!
//! Raw semantic similarity alone isn't enough — recency, file overlap,
//! and error pattern matching improve relevance for injection.
//!
//! Scoring formula: `final = semantic * 0.5 + recency * 0.2 + file_overlap * 0.2 + error_match * 0.1`

use std::collections::HashSet;

/// A search result enriched with multi-signal scoring.
#[derive(Debug, Clone)]
pub struct ScoredResult {
    pub content: String,
    pub raw_score: f32,
    pub final_score: f32,
    pub source: String,
    pub signals: Vec<Signal>,
}

/// What contributed to a result's score.
#[derive(Debug, Clone)]
pub enum Signal {
    SemanticMatch(f32),
    RecencyBoost(f32),
    FileOverlap(f32),
    ErrorPatternMatch(f32),
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
    let weights = phase
        .map(super::weights::WeightProfile::for_phase)
        .unwrap_or(super::weights::WeightProfile::for_phase(
            super::weights::HookPhase::PromptSubmit,
        ));
    let phase = phase.unwrap_or(super::weights::HookPhase::PromptSubmit);

    let mut scored: Vec<ScoredResult> = results
        .into_iter()
        .map(|r| score_result(r, current_files, current_errors, &weights, phase))
        .collect();

    scored.sort_by(|a, b| {
        b.final_score
            .partial_cmp(&a.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    scored
}

fn score_result(
    result: RawResult,
    current_files: &[String],
    current_errors: &[String],
    weights: &super::weights::WeightProfile,
    phase: super::weights::HookPhase,
) -> ScoredResult {
    let mut signals = Vec::new();

    // 1. Semantic match (raw HNSW score, already 0.0-1.0)
    let semantic = result.score;
    signals.push(Signal::SemanticMatch(semantic));

    // 2. Recency boost (0.0-1.0 based on age)
    let recency = compute_recency_boost(result.timestamp.as_deref());
    signals.push(Signal::RecencyBoost(recency));

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
    }
}

/// Compute recency boost: 1.0 for today, decaying with age.
/// Uses formula: 2^(-age_days / 30) — halves every 30 days.
fn compute_recency_boost(timestamp: Option<&str>) -> f32 {
    let ts = match timestamp {
        Some(ts) => ts,
        None => return 0.5, // No timestamp → neutral boost
    };

    let parsed = match crate::temporal::parse_timestamp(ts) {
        Some(dt) => dt,
        None => return 0.5,
    };

    let now = chrono::Utc::now();
    let age_days = (now - parsed).num_days().max(0) as f64;

    // 2^(-age/30): 1.0 today, 0.5 at 30 days, 0.25 at 60 days
    (2.0_f64.powf(-age_days / 30.0)) as f32
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
            },
            RawResult {
                content: "low match".into(),
                score: 0.5,
                source: "chunk".into(),
                timestamp: None,
                files: vec![],
                error_patterns: vec![],
                tags: vec![],
            },
        ];

        let scored = rank_results(results, &[], &[], None);
        assert_eq!(scored.len(), 2);
        assert!(scored[0].final_score > scored[1].final_score);
        assert_eq!(scored[0].content, "high match");
    }

    #[test]
    fn test_recency_boost() {
        let now = chrono::Utc::now().to_rfc3339();
        let old = "2025-01-01T00:00:00Z".to_string();

        let results = vec![
            RawResult {
                content: "recent".into(),
                score: 0.7,
                source: "chunk".into(),
                timestamp: Some(now),
                files: vec![],
                error_patterns: vec![],
                tags: vec![],
            },
            RawResult {
                content: "old".into(),
                score: 0.7,
                source: "chunk".into(),
                timestamp: Some(old),
                files: vec![],
                error_patterns: vec![],
                tags: vec![],
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
            },
            RawResult {
                content: "no overlap".into(),
                score: 0.7,
                source: "chunk".into(),
                timestamp: None,
                files: vec!["src/other.rs".into()],
                error_patterns: vec![],
                tags: vec![],
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
            },
            RawResult {
                content: "no error match".into(),
                score: 0.7,
                source: "reflection".into(),
                timestamp: None,
                files: vec![],
                error_patterns: vec!["out of memory".into()],
                tags: vec![],
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
}
