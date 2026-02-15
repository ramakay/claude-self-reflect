//! Multi-signal relevance scorer for search results.
//!
//! Raw semantic similarity alone isn't enough — recency, file overlap,
//! and error pattern matching improve relevance for injection.
//!
//! Scoring formula: `final = semantic * 0.5 + recency * 0.2 + file_overlap * 0.2 + error_match * 0.1`

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
}

/// Score and rank results for injection.
///
/// Applies multi-signal scoring:
/// - semantic: raw HNSW cosine similarity (weight 0.5)
/// - recency: newer results score higher (weight 0.2)
/// - file_overlap: results mentioning current session files (weight 0.2)
/// - error_match: results matching current error patterns (weight 0.1)
pub fn rank_results(
    results: Vec<RawResult>,
    current_files: &[String],
    current_errors: &[String],
) -> Vec<ScoredResult> {
    let mut scored: Vec<ScoredResult> = results
        .into_iter()
        .map(|r| score_result(r, current_files, current_errors))
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

    // Weighted combination
    let final_score = semantic * 0.5 + recency * 0.2 + file_overlap * 0.2 + error_match * 0.1;

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

    let parsed = match chrono::DateTime::parse_from_rfc3339(ts) {
        Ok(dt) => dt,
        Err(_) => return 0.5,
    };

    let now = chrono::Utc::now();
    let age_days = (now - parsed.with_timezone(&chrono::Utc))
        .num_days()
        .max(0) as f64;

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
        .filter(|cf| {
            result_files
                .iter()
                .any(|rf| files_match(rf, cf))
        })
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

/// Compute error pattern match: 1.0 if any current error appears in result, 0.0 otherwise.
fn compute_error_match(result_errors: &[String], current_errors: &[String]) -> f32 {
    if current_errors.is_empty() || result_errors.is_empty() {
        return 0.0;
    }

    let has_match = current_errors.iter().any(|ce| {
        result_errors
            .iter()
            .any(|re| re.contains(ce.as_str()) || ce.contains(re.as_str()))
    });

    if has_match {
        1.0
    } else {
        0.0
    }
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
            },
            RawResult {
                content: "low match".into(),
                score: 0.5,
                source: "chunk".into(),
                timestamp: None,
                files: vec![],
                error_patterns: vec![],
            },
        ];

        let scored = rank_results(results, &[], &[]);
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
            },
            RawResult {
                content: "old".into(),
                score: 0.7,
                source: "chunk".into(),
                timestamp: Some(old),
                files: vec![],
                error_patterns: vec![],
            },
        ];

        let scored = rank_results(results, &[], &[]);
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
            },
            RawResult {
                content: "no overlap".into(),
                score: 0.7,
                source: "chunk".into(),
                timestamp: None,
                files: vec!["src/other.rs".into()],
                error_patterns: vec![],
            },
        ];

        let current_files = vec!["src/auth.rs".into()];
        let scored = rank_results(results, &current_files, &[]);
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
            },
            RawResult {
                content: "no error match".into(),
                score: 0.7,
                source: "reflection".into(),
                timestamp: None,
                files: vec![],
                error_patterns: vec!["out of memory".into()],
            },
        ];

        let current_errors = vec!["connection reset".into()];
        let scored = rank_results(results, &[], &current_errors);
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
        let scored = rank_results(vec![], &[], &[]);
        assert!(scored.is_empty());
    }
}
