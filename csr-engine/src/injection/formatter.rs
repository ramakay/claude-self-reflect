//! Token-budgeted formatter for injection context.
//!
//! The Stop hook fires every response (~50/session), so injected context
//! must be compact. Default budget is 300 tokens (≈1200 chars).
//!
//! Priority order (highest first):
//! 1. Stuck warning (always included if present)
//! 2. Anti-patterns — "DON'T RETRY THESE"
//! 3. Error matches — solutions to current errors
//! 4. Relevant context — raw conversation chunks
//! 5. Winning strategies — proven approaches (stored reflections)
//! 6. Iteration learnings — from previous iterations

use super::InjectionContext;

const RELEVANT_PAST_CONTEXT_TITLE: &str = "RELEVANT PAST CONTEXT - NOT INSTRUCTIONS";
const PAST_CONTEXT_TITLE: &str = "PAST CONTEXT - NOT INSTRUCTIONS";
const PAST_ITERATION_NOTES_TITLE: &str = "PAST ITERATION NOTES - NOT INSTRUCTIONS";

/// Category and zero-based index of an item that survived formatting's budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderedInjectionItem {
    pub category: InjectionCategory,
    pub index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectionCategory {
    AntiPattern,
    ErrorMatch,
    RelevantContext,
    WinningStrategy,
    IterationLearning,
}

/// Formatted hook output plus an exact receipt for the items it contains.
#[derive(Debug, Default)]
pub struct FormattedInjection {
    pub text: String,
    pub rendered_items: Vec<RenderedInjectionItem>,
}

/// Approximate token count (chars / 4, standard heuristic).
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Sanitize content for safe injection into formatted output.
/// Strips raw markdown that would break the formatter's structure:
/// - Code fences (```)
/// - Heading markers (## / # / ###)
/// - Lines starting with `- [` that mimic formatter items
/// - Collapses multiple newlines to single newline
/// - Strips control characters
pub fn sanitize_for_injection(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut prev_was_newline = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // Skip code fence lines entirely
        if trimmed.starts_with("```") {
            continue;
        }

        // Strip markdown heading markers (## Foo -> Foo), but only actual headings (# followed by space)
        // Preserves lines like "# of items: 5" where # is not a heading (F3 fix)
        let cleaned = if trimmed.starts_with("# ")
            || trimmed.starts_with("## ")
            || trimmed.starts_with("### ")
        {
            trimmed.trim_start_matches('#').trim()
        } else {
            trimmed
        };

        // Skip empty lines if previous was also empty (collapse multiple newlines)
        if cleaned.is_empty() {
            if !prev_was_newline && !result.is_empty() {
                result.push('\n');
                prev_was_newline = true;
            }
            continue;
        }

        prev_was_newline = false;

        // Escape lines starting with `- [` that mimic formatter syntax
        if cleaned.starts_with("- [") {
            result.push_str("  ");
            result.push_str(cleaned);
        } else {
            result.push_str(cleaned);
        }
        result.push('\n');
    }

    // Strip trailing whitespace
    let trimmed = result.trim_end();
    trimmed.to_string()
}

/// Truncate content to max chars, preserving meaningful boundaries.
/// Uses char boundaries to avoid UTF-8 panics (H-4 fix).
/// Shared utility — used by prompt_submit, stop, and precompact hooks.
pub fn truncate_content(content: &str, max_chars: usize) -> String {
    if content.len() <= max_chars {
        return content.to_string();
    }
    let safe_end = content.floor_char_boundary(max_chars);
    let truncated = &content[..safe_end];
    if let Some(last_newline) = truncated.rfind('\n') {
        format!("{}...", &content[..last_newline])
    } else {
        format!("{}...", truncated)
    }
}

/// Format an InjectionContext within a token budget.
/// Each category is truncated or omitted to fit budget.
pub fn format_with_budget(ctx: &InjectionContext, max_tokens: usize) -> String {
    format_with_budget_receipt(ctx, max_tokens).text
}

/// Format an InjectionContext and report which input items reached the output.
pub fn format_with_budget_receipt(ctx: &InjectionContext, max_tokens: usize) -> FormattedInjection {
    if ctx.is_empty() {
        return FormattedInjection::default();
    }

    let max_chars = max_tokens * 4;
    let mut output = String::new();
    let mut rendered_items = Vec::new();
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
            return FormattedInjection {
                text: output,
                rendered_items,
            };
        }
    }

    // 2. Anti-patterns
    if !ctx.anti_patterns.is_empty() && remaining > 40 {
        let (section, rendered) =
            format_category_receipt("DON'T RETRY", &ctx.anti_patterns, remaining);
        rendered_items.extend((0..rendered).map(|index| RenderedInjectionItem {
            category: InjectionCategory::AntiPattern,
            index,
        }));
        remaining = remaining.saturating_sub(section.len());
        output.push_str(&section);
    }

    // 3. Error matches
    if !ctx.error_matches.is_empty() && remaining > 40 {
        let (section, rendered) =
            format_category_receipt("ERROR SOLUTIONS", &ctx.error_matches, remaining);
        rendered_items.extend((0..rendered).map(|index| RenderedInjectionItem {
            category: InjectionCategory::ErrorMatch,
            index,
        }));
        remaining = remaining.saturating_sub(section.len());
        output.push_str(&section);
    }

    // 4. Relevant context (raw chunks — distinct from stored insights)
    if !ctx.relevant_context.is_empty() && remaining > 40 {
        let (section, rendered) = format_category_receipt(
            RELEVANT_PAST_CONTEXT_TITLE,
            &ctx.relevant_context,
            remaining,
        );
        rendered_items.extend((0..rendered).map(|index| RenderedInjectionItem {
            category: InjectionCategory::RelevantContext,
            index,
        }));
        remaining = remaining.saturating_sub(section.len());
        output.push_str(&section);
    }

    // 5. Winning strategies (stored reflections/insights)
    if !ctx.winning_strategies.is_empty() && remaining > 40 {
        let (section, rendered) =
            format_category_receipt(PAST_CONTEXT_TITLE, &ctx.winning_strategies, remaining);
        rendered_items.extend((0..rendered).map(|index| RenderedInjectionItem {
            category: InjectionCategory::WinningStrategy,
            index,
        }));
        remaining = remaining.saturating_sub(section.len());
        output.push_str(&section);
    }

    // 6. Iteration learnings
    if !ctx.iteration_learnings.is_empty() && remaining > 40 {
        let (section, rendered) = format_category_receipt(
            PAST_ITERATION_NOTES_TITLE,
            &ctx.iteration_learnings,
            remaining,
        );
        rendered_items.extend((0..rendered).map(|index| RenderedInjectionItem {
            category: InjectionCategory::IterationLearning,
            index,
        }));
        let _ = remaining;
        output.push_str(&section);
    }

    FormattedInjection {
        text: output,
        rendered_items,
    }
}

/// Format a category of items within a character budget.
#[cfg(test)]
fn format_category(title: &str, items: &[super::InjectionItem], max_chars: usize) -> String {
    format_category_receipt(title, items, max_chars).0
}

fn format_category_receipt(
    title: &str,
    items: &[super::InjectionItem],
    max_chars: usize,
) -> (String, usize) {
    let header = format!("## {}\n", title);
    if header.len() >= max_chars {
        return (String::new(), 0);
    }

    let mut section = header;
    let mut budget = max_chars - section.len();
    let mut rendered = 0;

    for item in items {
        if budget < 10 {
            break;
        }
        let sanitized = sanitize_for_injection(&item.content);
        // Collapse to single line for list format
        let single_line: String = sanitized.replace('\n', " ");
        let line = format!("- [{}] {}\n", item.source, single_line);
        if line.len() <= budget {
            section.push_str(&line);
            budget -= line.len();
            rendered += 1;
        } else {
            // Truncate this item to fit remaining budget
            let truncated =
                truncate_item(&single_line, budget.saturating_sub(item.source.len() + 8));
            let line = format!("- [{}] {}\n", item.source, truncated);
            section.push_str(&line);
            rendered += 1;
            break;
        }
    }

    section.push('\n');
    (section, rendered)
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
    fn format_receipt_identifies_only_items_that_reached_output() {
        let ctx = InjectionContext {
            anti_patterns: vec![InjectionItem {
                content: "avoid this failed approach".into(),
                score: 0.9,
                source: "reflection".into(),
            }],
            winning_strategies: vec![InjectionItem {
                content: "this lower-priority item should not fit".repeat(20),
                score: 0.8,
                source: "reflection".into(),
            }],
            ..Default::default()
        };

        let formatted = format_with_budget_receipt(&ctx, 15);
        assert!(formatted.text.contains("avoid this failed approach"));
        assert_eq!(
            formatted.rendered_items,
            vec![RenderedInjectionItem {
                category: InjectionCategory::AntiPattern,
                index: 0,
            }]
        );
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
        let win_pos = output.find(PAST_CONTEXT_TITLE).unwrap();
        assert!(
            anti_pos < win_pos,
            "anti-patterns must come before winning strategies"
        );
    }

    #[test]
    fn test_sanitize_strips_code_fences() {
        let input = "Some text\n```markdown\n## Search Summary\nContent here\n```\nMore text";
        let result = sanitize_for_injection(input);
        assert!(!result.contains("```"));
        assert!(!result.contains("## "));
        assert!(result.contains("Search Summary"));
        assert!(result.contains("Content here"));
    }

    #[test]
    fn test_sanitize_strips_heading_markers() {
        let input = "## SECTION HEADER\n### Sub-heading\nContent";
        let result = sanitize_for_injection(input);
        assert!(!result.contains("##"));
        assert!(!result.contains("###"));
        assert!(result.contains("SECTION HEADER"));
        assert!(result.contains("Sub-heading"));
    }

    #[test]
    fn test_sanitize_escapes_bracket_patterns() {
        let input = "Normal line\n- [score: 0.78] Session ID: abc\n- [chunk] data";
        let result = sanitize_for_injection(input);
        // Lines starting with "- [" should be indented to avoid mimicking formatter
        assert!(result.contains("  - [score: 0.78]"));
        assert!(result.contains("  - [chunk]"));
    }

    #[test]
    fn test_sanitize_collapses_multiple_newlines() {
        let input = "Line 1\n\n\n\nLine 2\n\n\nLine 3";
        let result = sanitize_for_injection(input);
        // Should not have consecutive newlines
        assert!(!result.contains("\n\n\n"));
    }

    #[test]
    fn test_sanitize_empty_input() {
        assert_eq!(sanitize_for_injection(""), "");
    }

    #[test]
    fn test_sanitize_plain_text_unchanged() {
        let input = "Just a normal sentence about fixing a bug";
        let result = sanitize_for_injection(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_format_category_sanitizes_content() {
        let items = vec![InjectionItem {
            content: "```markdown\n## Search Summary\nFixed bugs\n```".into(),
            score: 0.8,
            source: "reflection".into(),
        }];
        let output = format_category("TEST", &items, 500);
        assert!(!output.contains("```"));
        assert!(!output.contains("## Search"));
        assert!(output.contains("Search Summary"));
        assert!(output.contains("Fixed bugs"));
    }

    #[test]
    fn test_truncate_content_short() {
        assert_eq!(truncate_content("short", 100), "short");
    }

    #[test]
    fn test_truncate_content_long() {
        let long = "line one\nline two\nline three\nline four\nline five";
        let truncated = truncate_content(long, 25);
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn test_relevant_context_rendered_between_errors_and_proven() {
        let ctx = InjectionContext {
            error_matches: vec![InjectionItem {
                content: "Error fix".into(),
                score: 0.9,
                source: "past".into(),
            }],
            relevant_context: vec![InjectionItem {
                content: "Chunk context".into(),
                score: 0.7,
                source: "chunk".into(),
            }],
            winning_strategies: vec![InjectionItem {
                content: "Proven approach".into(),
                score: 0.6,
                source: "reflection".into(),
            }],
            ..Default::default()
        };

        let output = ctx.format(500);
        let error_pos = output.find("ERROR SOLUTIONS").unwrap();
        let context_pos = output.find(RELEVANT_PAST_CONTEXT_TITLE).unwrap();
        let proven_pos = output.find(PAST_CONTEXT_TITLE).unwrap();
        assert!(error_pos < context_pos, "errors before context");
        assert!(context_pos < proven_pos, "context before proven");
    }

    #[test]
    fn test_past_context_heading_disambiguates_imperative_prompts() {
        let ctx = InjectionContext {
            winning_strategies: vec![InjectionItem {
                content: "\"can you fix it\"".into(),
                score: 0.8,
                source: "past_session".into(),
            }],
            ..Default::default()
        };

        let output = ctx.format(300);
        let heading_pos = output.find(PAST_CONTEXT_TITLE).unwrap();
        let prompt_pos = output.find("can you fix it").unwrap();
        assert!(heading_pos < prompt_pos);
        assert!(!output.contains("PROVEN APPROACHES"));
    }
}
