//! Dreamer v1 — consolidates session narratives into typed facts.
//!
//! Runs as part of the daemon. Scans V3/AI narrative reflections and extracts
//! durable facts: architectural decisions, conventions, preferences, bug patterns.
//! Facts are stored as additional tagged reflections — NOT replacing the source narrative.

/// Fact types produced by the Dreamer.
#[derive(Debug, Clone)]
pub struct ConsolidatedFact {
    pub fact_type: String,
    pub content: String,
    pub confidence: f32,
}

/// Extract typed facts from a narrative or enriched text.
/// Uses keyword heuristics and sentence structure — no LLM calls (Layer 0 Dreamer).
pub fn extract_facts(narrative: &str) -> Vec<ConsolidatedFact> {
    let mut facts = Vec::new();

    for candidate in fact_candidates(narrative) {
        let s = candidate.trim();
        if word_count(s) < 4 {
            continue;
        }

        // Skip V3 metadata noise — JSON blobs, signatures, field labels
        if is_metadata_noise(s) {
            continue;
        }

        if let Some((fact_type, confidence)) = classify_fact(s) {
            facts.push(ConsolidatedFact {
                fact_type: fact_type.into(),
                content: s.to_string(),
                confidence,
            });
        }
    }

    // Dedup using a set of prefixes (not just adjacent — catches non-adjacent duplicates)
    let mut seen = std::collections::HashSet::new();
    facts.retain(|f| {
        let prefix: String = f.content.chars().take(80).collect();
        seen.insert(prefix)
    });

    facts
}

/// Extract sentence candidates only from prose-bearing parts of V3 narratives.
fn fact_candidates(narrative: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut paragraph = String::new();
    let mut in_code_context = false;

    for raw_line in narrative.lines() {
        let line = raw_line.trim();

        if line == "---" {
            flush_paragraph(&mut paragraph, &mut candidates);
            break;
        }

        if line.starts_with("##") {
            flush_paragraph(&mut paragraph, &mut candidates);
            in_code_context = line.eq_ignore_ascii_case("## Code Context");
            continue;
        }

        if in_code_context {
            continue;
        }

        if line.is_empty() {
            flush_paragraph(&mut paragraph, &mut candidates);
            continue;
        }

        if is_structural_noise_line(line) {
            flush_paragraph(&mut paragraph, &mut candidates);
            continue;
        }

        let cleaned = clean_candidate_text(line);
        if cleaned.is_empty() {
            continue;
        }

        if !paragraph.is_empty() {
            paragraph.push(' ');
        }
        paragraph.push_str(&cleaned);
    }

    flush_paragraph(&mut paragraph, &mut candidates);
    candidates
}

fn flush_paragraph(paragraph: &mut String, candidates: &mut Vec<String>) {
    if paragraph.trim().is_empty() {
        paragraph.clear();
        return;
    }

    let cleaned = clean_candidate_text(paragraph);
    for sentence in split_sentences(&cleaned) {
        let candidate = clean_candidate_text(sentence);
        if !candidate.is_empty() {
            candidates.push(candidate);
        }
    }
    paragraph.clear();
}

fn clean_candidate_text(s: &str) -> String {
    let mut text = s.trim().to_string();

    loop {
        let trimmed = text.trim_start();
        let stripped = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("> "));
        if let Some(rest) = stripped {
            text = rest.trim_start().to_string();
        } else {
            text = trimmed.to_string();
            break;
        }
    }

    text = text
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .replace("\\n", " ")
        .replace("\\\"", "\"");

    collapse_whitespace(&text)
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn classify_fact(s: &str) -> Option<(&'static str, f32)> {
    let lower = s.to_lowercase();

    // Architectural decision detection — needs a subject doing the deciding
    if contains_indicator(&lower, "decided to")
        || contains_indicator(&lower, "chose")
        || contains_indicator(&lower, "switched to")
        || contains_indicator(&lower, "instead of")
        || contains_indicator(&lower, "migrated to")
        || (contains_indicator(&lower, "replaced") && contains_indicator(&lower, "with"))
        || contains_indicator(&lower, "adopted")
    {
        return Some(("architectural_decision", 0.7));
    }

    // Convention detection — tightened to require actionable phrasing
    if contains_indicator(&lower, "convention:")
        || contains_indicator(&lower, "convention established")
        || contains_indicator(&lower, "rule:")
        || contains_indicator(&lower, "must not")
        || contains_indicator(&lower, "should not")
        || (contains_indicator(&lower, "never") && contains_indicator(&lower, "when"))
        || (contains_indicator(&lower, "always") && contains_indicator(&lower, "before"))
        || (contains_indicator(&lower, "always") && contains_indicator(&lower, "after"))
        || contains_indicator(&lower, "standard:")
    {
        return Some(("convention", 0.7));
    }

    // Bug pattern detection — require enough context to be actionable
    if (contains_indicator(&lower, "recurring")
        && (contains_indicator(&lower, "bug")
            || contains_indicator(&lower, "bugs")
            || contains_indicator(&lower, "error")
            || contains_indicator(&lower, "errors")
            || contains_indicator(&lower, "issue")
            || contains_indicator(&lower, "issues")))
        || contains_indicator(&lower, "keeps happening")
        || contains_indicator(&lower, "off-by-one")
        || contains_indicator(&lower, "regression")
        || contains_indicator(&lower, "broke again")
        || contains_indicator(&lower, "flaky test")
        || contains_indicator(&lower, "race condition")
        || (contains_indicator(&lower, "error recovery")
            && contains_indicator(&lower, "handled")
            && word_count(s) >= 8)
    {
        return Some(("bug_pattern", 0.6));
    }

    // Preference detection — user expressing a choice with reasoning
    if ((contains_indicator(&lower, "prefer")
        || contains_indicator(&lower, "prefers")
        || contains_indicator(&lower, "preferred"))
        && (contains_indicator(&lower, "over") || contains_indicator(&lower, "instead")))
        || contains_indicator(&lower, "likes to")
        || contains_indicator(&lower, "rather than")
    {
        return Some(("preference", 0.5));
    }

    None
}

/// Detect V3 metadata noise that should not become facts.
fn is_metadata_noise(s: &str) -> bool {
    let trimmed = s.trim();
    let lower = trimmed.to_lowercase();

    if trimmed == "---" || trimmed.starts_with("##") || is_structural_noise_line(trimmed) {
        return true;
    }

    // JSON-like content (signatures, structured data)
    if trimmed.contains('{')
        && trimmed.contains('}')
        && (trimmed.contains('"') || trimmed.contains(':'))
    {
        return true;
    }

    // V3 template field labels and structured output (anywhere in text, not just start)
    if lower.contains("functions:")
        || lower.contains("imports:")
        || lower.contains("languages:")
        || lower.contains("patterns:")
        || lower.starts_with("pattern:")
        || lower.starts_with("signature:")
        || lower.starts_with("context:")
        || lower.starts_with("status:")
        || lower.starts_with("**context**")
        || lower.starts_with("**error recovery**")
        || lower.starts_with("modification:")
        || lower.starts_with("deletion:")
        || lower.starts_with("expansion:")
        || lower.starts_with("creation:")
    {
        return true;
    }

    // Comma-separated lists (function names, imports, file paths) — 3+ commas = list
    let comma_count = trimmed.chars().filter(|c| *c == ',').count();
    if comma_count > 2 {
        return true;
    }

    // Lines containing code-like content (import statements, file paths)
    if lower.starts_with("import ") || lower.starts_with("from ") || lower.starts_with("use ") {
        return true;
    }

    // Markdown headers and bullet-only lines
    if trimmed.starts_with('#') || (trimmed.starts_with('-') && trimmed.len() < 60) {
        return true;
    }

    // Lines that are mostly field names (key: value pairs with short values)
    let colon_count = trimmed.chars().filter(|c| *c == ':').count();
    let words = word_count(trimmed);
    if colon_count > 0 && words < 6 && !starts_with_fact_label(&lower) {
        return true;
    }

    false
}

fn is_structural_noise_line(line: &str) -> bool {
    let lower = line.trim_start().to_lowercase();
    lower.starts_with("pattern:")
        || lower.starts_with("creation:")
        || lower.starts_with("modification:")
        || lower.starts_with("deletion:")
        || lower.starts_with("expansion:")
        || lower.starts_with("signature:")
        || lower.starts_with("context:")
        || lower.starts_with("status:")
}

fn starts_with_fact_label(lower: &str) -> bool {
    lower.starts_with("convention:") || lower.starts_with("rule:") || lower.starts_with("standard:")
}

fn contains_indicator(lower: &str, indicator: &str) -> bool {
    lower.match_indices(indicator).any(|(start, _)| {
        let before = lower[..start].chars().next_back();
        let after = lower[start + indicator.len()..].chars().next();
        is_indicator_boundary(before) && is_indicator_boundary(after)
    })
}

fn is_indicator_boundary(c: Option<char>) -> bool {
    match c {
        None => true,
        Some(c) => !(c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '.' | '-')),
    }
}

fn word_count(s: &str) -> usize {
    s.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .count()
}

/// Split text into sentences on `. `, `.\n`, or standalone `\n`.
fn split_sentences(text: &str) -> Vec<&str> {
    let mut sentences = Vec::new();
    let mut start = 0;

    for (i, c) in text.char_indices() {
        if c == '.' {
            // Check if followed by space, newline, or end
            let next = text[i + 1..].chars().next();
            if next.is_none() || next == Some(' ') || next == Some('\n') {
                let s = &text[start..=i];
                if !s.trim().is_empty() {
                    sentences.push(s.trim());
                }
                start = i + 1;
            }
        } else if c == '\n' {
            let s = &text[start..i];
            if !s.trim().is_empty() && s.trim().len() > 10 {
                sentences.push(s.trim());
            }
            start = i + 1;
        }
    }

    // Remainder
    let remainder = &text[start..];
    if !remainder.trim().is_empty() && remainder.trim().len() > 10 {
        sentences.push(remainder.trim());
    }

    sentences
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_architectural_decision() {
        let narrative = "User decided to use axum instead of warp for the web server. The migration was straightforward.";
        let facts = extract_facts(narrative);
        assert!(
            facts
                .iter()
                .any(|f| f.fact_type == "architectural_decision"),
            "should detect architectural decision, got: {:?}",
            facts
        );
    }

    #[test]
    fn test_extract_convention() {
        let narrative = "Convention established: handlers must not query the database directly.";
        let facts = extract_facts(narrative);
        assert!(
            facts.iter().any(|f| f.fact_type == "convention"),
            "should detect convention, got: {:?}",
            facts
        );
    }

    #[test]
    fn test_extract_bug_pattern() {
        let narrative =
            "Recurring bug with off-by-one errors in pagination when offset equals total count.";
        let facts = extract_facts(narrative);
        assert!(
            facts.iter().any(|f| f.fact_type == "bug_pattern"),
            "should detect bug pattern, got: {:?}",
            facts
        );
    }

    #[test]
    fn test_extract_preference() {
        let narrative =
            "User prefers small functions over monolithic handlers. Each function under 30 lines.";
        let facts = extract_facts(narrative);
        assert!(
            facts.iter().any(|f| f.fact_type == "preference"),
            "should detect preference, got: {:?}",
            facts
        );
    }

    #[test]
    fn test_empty_narrative() {
        let facts = extract_facts("");
        assert!(facts.is_empty());
    }

    #[test]
    fn test_no_facts_in_generic_text() {
        let narrative =
            "The code was modified to handle edge cases. Tests were updated accordingly.";
        let facts = extract_facts(narrative);
        assert!(
            facts.is_empty(),
            "generic text should not produce facts, got: {:?}",
            facts
        );
    }

    #[test]
    fn test_multiple_facts() {
        let narrative = "User decided to use Redis for caching. Convention: all cache keys must be namespaced. Recurring regression in cache invalidation logic.";
        let facts = extract_facts(narrative);
        assert!(
            facts.len() >= 3,
            "should find 3+ facts, got {}: {:?}",
            facts.len(),
            facts
        );
    }

    #[test]
    fn test_metadata_noise_filtered() {
        // V3 signature JSON should not produce facts
        let noise = r#"Signature: {"completion_status": "partial", "frameworks": ["nextjs"], "pattern_reusability": "medium"}"#;
        let facts = extract_facts(noise);
        assert!(
            facts.is_empty(),
            "JSON metadata should be filtered, got: {:?}",
            facts
        );

        // Generic "Pattern:" label should not produce convention facts
        let pattern_noise = "Pattern: New file creation\nPattern: Code expansion/feature addition";
        let facts2 = extract_facts(pattern_noise);
        assert!(
            facts2.is_empty(),
            "Pattern: labels should be filtered, got: {:?}",
            facts2
        );
    }

    #[test]
    fn test_is_metadata_noise_real_v3_lines() {
        assert!(is_metadata_noise(
            "FUNCTIONS: cancel_task, cleanup_expired, complete, test_prompt_submit_prefers_semantic"
        ));
        assert!(is_metadata_noise(
            "IMPORTS: import './globals.css';, import reportError from './error-boundary';"
        ));
        assert!(is_metadata_noise("LANGUAGES: TypeScript"));
        assert!(is_metadata_noise("Pattern: New file creation"));
        assert!(is_metadata_noise("creation: tasks.rs"));
        assert!(is_metadata_noise("modification: mod.rs"));
        assert!(is_metadata_noise("deletion: old_tasks.rs"));
        assert!(is_metadata_noise(
            r#"Signature: {"completion_status":"partial","frameworks":["nextjs"]}"#
        ));
    }

    #[test]
    fn test_complete_v3_code_context_does_not_extract_facts() {
        let narrative = r#"
## User Request
"Run cleanup."

## Solution Pattern
creation: tasks.rs
  New file creation
modification: mod.rs

## Code Context
FUNCTIONS: cancel_task, cleanup_expired, complete, test_prompt_submit_prefers_semantic
LANGUAGES: TypeScript
IMPORTS: import './globals.css';, import reportError from './error-boundary';, import regression from './regression-helper';

---
Signature: {"completion_status":"partial","pattern_reusability":"medium"}
"#;

        let facts = extract_facts(narrative);
        assert!(
            facts.is_empty(),
            "Code Context and signature metadata should not produce facts, got: {:?}",
            facts
        );
    }

    #[test]
    fn test_function_names_with_prefer_do_not_create_preferences() {
        let narrative =
            "The test_prompt_submit_prefers_semantic helper changed over time in the test suite.";

        let facts = extract_facts(narrative);
        assert!(
            !facts.iter().any(|f| f.fact_type == "preference"),
            "embedded prefers in function names should not produce preference facts, got: {:?}",
            facts
        );
    }

    #[test]
    fn test_import_paths_with_error_do_not_create_bug_patterns() {
        let narrative =
            "Import path './error-boundary' appeared during recurring cleanup work in the bundle.";

        let facts = extract_facts(narrative);
        assert!(
            !facts.iter().any(|f| f.fact_type == "bug_pattern"),
            "embedded error in import paths should not produce bug facts, got: {:?}",
            facts
        );
    }

    #[test]
    fn test_genuine_prose_facts_still_extract_from_v3() {
        let narrative = r#"
## User Request
"Convention established: handlers must not query the database directly. Recurring bug with off-by-one errors in pagination keeps happening."

## Solution Pattern
creation: storage.rs
  We adopted a repository layer instead of direct SQL calls.

## Code Context
FUNCTIONS: test_prompt_submit_prefers_semantic, cleanup_expired, handle_error
IMPORTS: import reportError from './error-boundary';

---
Signature: {"completion_status":"complete"}
"#;

        let facts = extract_facts(narrative);
        assert!(
            facts
                .iter()
                .any(|f| f.fact_type == "architectural_decision"),
            "should detect architectural decision from prose, got: {:?}",
            facts
        );
        assert!(
            facts.iter().any(|f| f.fact_type == "convention"),
            "should detect convention from prose, got: {:?}",
            facts
        );
        assert!(
            facts.iter().any(|f| f.fact_type == "bug_pattern"),
            "should detect bug pattern from prose, got: {:?}",
            facts
        );
    }

    #[test]
    fn test_quality_over_quantity() {
        // Short fragments below four words should be skipped
        let short = "Bug in auth. Fix it.";
        let facts = extract_facts(short);
        assert!(
            facts.is_empty(),
            "short fragments should be skipped, got: {:?}",
            facts
        );
    }
}
