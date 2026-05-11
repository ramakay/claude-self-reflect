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

    for sentence in split_sentences(narrative) {
        let s = sentence.trim();
        if s.len() < 20 {
            continue;
        } // Need enough substance to be a useful fact

        // Skip V3 metadata noise — JSON blobs, signatures, field labels
        if is_metadata_noise(s) {
            continue;
        }

        let lower = s.to_lowercase();

        // Architectural decision detection — needs a subject doing the deciding
        if (lower.contains("decided to")
            || lower.contains("chose ")
            || lower.contains("switched to")
            || lower.contains("instead of")
            || lower.contains("migrated to")
            || lower.contains("replaced ") && lower.contains(" with "))
            || lower.contains("adopted ")
        {
            facts.push(ConsolidatedFact {
                fact_type: "architectural_decision".into(),
                content: s.to_string(),
                confidence: 0.7,
            });
            continue;
        }

        // Convention detection — tightened to require actionable phrasing
        if lower.contains("convention:")
            || lower.contains("convention established")
            || lower.contains("rule:")
            || (lower.contains("must") && lower.contains("not") && lower.len() > 30)
            || (lower.contains("should") && lower.contains("not") && lower.len() > 30)
            || (lower.contains("never ") && lower.contains(" when "))
            || (lower.contains("always ") && lower.contains(" before "))
            || (lower.contains("always ") && lower.contains(" after "))
            || lower.contains("standard:")
        {
            facts.push(ConsolidatedFact {
                fact_type: "convention".into(),
                content: s.to_string(),
                confidence: 0.7,
            });
            continue;
        }

        // Bug pattern detection — require enough context to be actionable
        if (lower.contains("recurring")
            && (lower.contains("bug") || lower.contains("error") || lower.contains("issue")))
            || lower.contains("keeps happening")
            || lower.contains("off-by-one")
            || (lower.contains("regression") && lower.len() > 30)
            || lower.contains("broke again")
            || lower.contains("flaky test")
            || lower.contains("race condition")
            || (lower.contains("error recovery") && lower.contains("handled") && lower.len() > 50)
        {
            facts.push(ConsolidatedFact {
                fact_type: "bug_pattern".into(),
                content: s.to_string(),
                confidence: 0.6,
            });
            continue;
        }

        // Preference detection — user expressing a choice with reasoning
        if (lower.contains("prefer") && (lower.contains("over") || lower.contains("instead")))
            || lower.contains("likes to")
            || (lower.contains("rather than") && lower.len() > 30)
        {
            facts.push(ConsolidatedFact {
                fact_type: "preference".into(),
                content: s.to_string(),
                confidence: 0.5,
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

/// Detect V3 metadata noise that should not become facts.
fn is_metadata_noise(s: &str) -> bool {
    let lower = s.to_lowercase();

    // JSON-like content (signatures, structured data)
    if s.contains('{') && s.contains('}') && (s.contains('"') || s.contains(':')) {
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
        || lower.starts_with("expansion:")
        || lower.starts_with("creation:")
    {
        return true;
    }

    // Comma-separated lists (function names, imports, file paths) — 3+ commas = list
    let comma_count = s.chars().filter(|c| *c == ',').count();
    if comma_count > 2 {
        return true;
    }

    // Lines containing code-like content (import statements, file paths)
    if lower.starts_with("import ") || lower.starts_with("from ") || lower.starts_with("use ") {
        return true;
    }

    // Markdown headers and bullet-only lines
    if s.starts_with('#') || (s.starts_with('-') && s.len() < 60) || s.starts_with("##") {
        return true;
    }

    // Lines that are mostly field names (key: value pairs with short values)
    let colon_count = s.chars().filter(|c| *c == ':').count();
    let word_count = s.split_whitespace().count();
    if colon_count > 0 && word_count < 6 {
        return true;
    }

    false
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
    fn test_quality_over_quantity() {
        // Short fragments below 20 chars should be skipped
        let short = "Bug in auth. Fix it.";
        let facts = extract_facts(short);
        assert!(
            facts.is_empty(),
            "short fragments should be skipped, got: {:?}",
            facts
        );
    }
}
