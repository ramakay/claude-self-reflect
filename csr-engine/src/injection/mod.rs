//! Injection engine — assembles context for injection into Claude's context.
//!
//! Used by both the Stop hook (per-iteration) and SessionStart hook.
//! Token-budgeted formatting ensures compact output (≤300 tokens by default).

pub mod anti_pattern;
pub mod formatter;
pub mod predictor;
pub mod weights;

/// Context assembled for injection into Claude's context.
#[derive(Debug, Default)]
pub struct InjectionContext {
    pub anti_patterns: Vec<InjectionItem>,
    pub error_matches: Vec<InjectionItem>,
    pub relevant_context: Vec<InjectionItem>,
    pub winning_strategies: Vec<InjectionItem>,
    pub iteration_learnings: Vec<InjectionItem>,
    pub stuck_warning: Option<String>,
}

/// A single item to inject, with provenance metadata.
#[derive(Debug, Clone)]
pub struct InjectionItem {
    pub content: String,
    pub score: f32,
    pub source: String, // "past_session", "iteration_3", etc.
}

impl InjectionContext {
    /// Format to string with token budget.
    /// Anti-patterns always come first. Truncates lowest-priority items
    /// if total exceeds budget.
    pub fn format(&self, max_tokens: usize) -> String {
        formatter::format_with_budget(self, max_tokens)
    }

    pub fn is_empty(&self) -> bool {
        self.anti_patterns.is_empty()
            && self.error_matches.is_empty()
            && self.relevant_context.is_empty()
            && self.winning_strategies.is_empty()
            && self.iteration_learnings.is_empty()
            && self.stuck_warning.is_none()
    }

    pub fn total_items(&self) -> usize {
        self.anti_patterns.len()
            + self.error_matches.len()
            + self.relevant_context.len()
            + self.winning_strategies.len()
            + self.iteration_learnings.len()
    }
}
