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
        if s.len() < 10 {
            continue;
        } // Skip tiny fragments
        let lower = s.to_lowercase();

        // Architectural decision detection
        if lower.contains("decided")
            || lower.contains("chose")
            || lower.contains("switched to")
            || lower.contains("instead of")
            || lower.contains("migrated to")
            || lower.contains("replaced")
            || lower.contains("adopted")
        {
            facts.push(ConsolidatedFact {
                fact_type: "architectural_decision".into(),
                content: s.to_string(),
                confidence: 0.7,
            });
            continue;
        }

        // Convention detection
        if lower.contains("convention")
            || lower.contains("rule:")
            || (lower.contains("must") && lower.contains("not"))
            || (lower.contains("should") && lower.contains("not"))
            || lower.contains("never ")
            || lower.contains("always ")
            || lower.contains("pattern:")
            || lower.contains("standard:")
        {
            facts.push(ConsolidatedFact {
                fact_type: "convention".into(),
                content: s.to_string(),
                confidence: 0.7,
            });
            continue;
        }

        // Bug pattern detection
        if lower.contains("bug")
            || lower.contains("recurring")
            || lower.contains("keeps happening")
            || lower.contains("off-by-one")
            || lower.contains("regression")
            || lower.contains("broke again")
            || lower.contains("flaky")
            || lower.contains("race condition")
        {
            facts.push(ConsolidatedFact {
                fact_type: "bug_pattern".into(),
                content: s.to_string(),
                confidence: 0.6,
            });
            continue;
        }

        // Preference detection
        if lower.contains("prefer")
            || lower.contains("likes to")
            || lower.contains("rather than")
            || lower.contains("style:")
            || lower.contains("favorite")
            || lower.contains("approach:")
        {
            facts.push(ConsolidatedFact {
                fact_type: "preference".into(),
                content: s.to_string(),
                confidence: 0.5,
            });
        }
    }

    // Dedup by content similarity (first 100 chars)
    facts.dedup_by(|a, b| {
        let a_prefix: String = a.content.chars().take(100).collect();
        let b_prefix: String = b.content.chars().take(100).collect();
        a_prefix == b_prefix
    });

    facts
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
        let narrative = "User decided to use Redis for caching. Convention: all cache keys must be namespaced. Recurring regression in cache invalidation.";
        let facts = extract_facts(narrative);
        assert!(
            facts.len() >= 3,
            "should find 3+ facts, got {}: {:?}",
            facts.len(),
            facts
        );
    }
}
