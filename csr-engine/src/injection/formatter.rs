//! Token-budgeted formatter for injection context.
//!
//! The Stop hook fires every response (~50/session), so injected context
//! must be compact. Default budget is 300 tokens (≈1200 chars).
//!
//! Priority order (highest first):
//! 1. Stuck warning (always included if present)
//! 2. Anti-patterns — "DON'T RETRY THESE"
//! 3. Error matches — solutions to current errors
//! 4. Winning strategies — proven approaches
//! 5. Iteration learnings — from previous iterations

use super::InjectionContext;

/// Approximate token count (chars / 4, standard heuristic).
pub fn estimate_tokens(text: &str) -> usize {
    (text.len() + 3) / 4
}

/// Format an InjectionContext within a token budget.
/// Each category is truncated or omitted to fit budget.
pub fn format_with_budget(ctx: &InjectionContext, max_tokens: usize) -> String {
    if ctx.is_empty() {
        return String::new();
    }

    let max_chars = max_tokens * 4;
    let mut output = String::new();
    let mut remaining = max_chars;

    // 1. Stuck warning — always included
    if let Some(ref warning) = ctx.stuck_warning {
        let section = format!("## STUCK WARNING\n{}\n\n", warning);
        if section.len() <= remaining {
            output.push_str(&section);
            remaining -= section.len();
        } else {
            // Truncate but always include some warning
            let truncated = truncate_item(warning, remaining.saturating_sub(25));
            let section = format!("## STUCK WARNING\n{}\n\n", truncated);
            output.push_str(&section);
            return output;
        }
    }

    // 2. Anti-patterns
    if !ctx.anti_patterns.is_empty() && remaining > 40 {
        let section = format_category("DON'T RETRY", &ctx.anti_patterns, remaining);
        remaining = remaining.saturating_sub(section.len());
        output.push_str(&section);
    }

    // 3. Error matches
    if !ctx.error_matches.is_empty() && remaining > 40 {
        let section = format_category("ERROR SOLUTIONS", &ctx.error_matches, remaining);
        remaining = remaining.saturating_sub(section.len());
        output.push_str(&section);
    }

    // 4. Winning strategies
    if !ctx.winning_strategies.is_empty() && remaining > 40 {
        let section = format_category("PROVEN APPROACHES", &ctx.winning_strategies, remaining);
        remaining = remaining.saturating_sub(section.len());
        output.push_str(&section);
    }

    // 5. Iteration learnings
    if !ctx.iteration_learnings.is_empty() && remaining > 40 {
        let section = format_category("ITERATION NOTES", &ctx.iteration_learnings, remaining);
        let _ = remaining;
        output.push_str(&section);
    }

    output
}

/// Format a category of items within a character budget.
fn format_category(
    title: &str,
    items: &[super::InjectionItem],
    max_chars: usize,
) -> String {
    let header = format!("## {}\n", title);
    if header.len() >= max_chars {
        return String::new();
    }

    let mut section = header;
    let mut budget = max_chars - section.len();

    for item in items {
        if budget < 10 {
            break;
        }
        let line = format!("- [{}] {}\n", item.source, item.content);
        if line.len() <= budget {
            section.push_str(&line);
            budget -= line.len();
        } else {
            // Truncate this item to fit remaining budget
            let truncated = truncate_item(&item.content, budget.saturating_sub(item.source.len() + 8));
            let line = format!("- [{}] {}\n", item.source, truncated);
            section.push_str(&line);
            break;
        }
    }

    section.push('\n');
    section
}

/// Truncate a single item to fit within a character limit,
/// preserving the first line and adding ellipsis.
/// Uses char boundaries to avoid UTF-8 panics (H-4 fix).
pub fn truncate_item(content: &str, max_chars: usize) -> String {
    if content.len() <= max_chars || max_chars < 4 {
        return content.to_string();
    }

    // Find safe char boundary (H-4 fix: prevents panic on multi-byte UTF-8)
    let safe_end = content.floor_char_boundary(max_chars.saturating_sub(3));
    let truncated = &content[..safe_end];
    // Try to break at a word boundary
    if let Some(last_space) = truncated.rfind(' ') {
        if last_space > safe_end / 2 {
            return format!("{}...", &content[..last_space]);
        }
    }
    format!("{}...", truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::injection::InjectionItem;

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("hello"), 2); // 5 chars / 4 = 1.25, rounded up
        assert_eq!(estimate_tokens("hello world, how are you"), 6);
    }

    #[test]
    fn test_truncate_item_short() {
        assert_eq!(truncate_item("short", 100), "short");
    }

    #[test]
    fn test_truncate_item_at_word_boundary() {
        let result = truncate_item("hello world how are you doing today", 20);
        assert!(result.ends_with("..."));
        assert!(result.len() <= 23); // max_chars + "..."
    }

    #[test]
    fn test_format_empty_context() {
        let ctx = InjectionContext::default();
        assert_eq!(ctx.format(300), "");
    }

    #[test]
    fn test_format_respects_priority() {
        let ctx = InjectionContext {
            anti_patterns: vec![InjectionItem {
                content: "Don't use hack approach".into(),
                score: 0.8,
                source: "past_session".into(),
            }],
            winning_strategies: vec![InjectionItem {
                content: "Use proper fix".into(),
                score: 0.7,
                source: "past_session".into(),
            }],
            ..Default::default()
        };

        let output = ctx.format(300);
        let anti_pos = output.find("DON'T RETRY").unwrap();
        let win_pos = output.find("PROVEN APPROACHES").unwrap();
        assert!(anti_pos < win_pos, "anti-patterns must come before winning strategies");
    }
}
