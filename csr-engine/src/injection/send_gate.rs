//! The prompt hook's send gate. A content-path block goes out only when the
//! best ranked score reaches [`TOP_SCORE_FLOOR`] and at least
//! [`MIN_RENDERED_ITEMS`] chunk or reflection items render. Anything else is
//! an abstain, and an abstain sends nothing: no PICKUP line, dream line,
//! anchor notice, anti-pattern, code-evolution or code-graph line either.
//!
//! The score is the ranked `final_score` (cosine, recency, file, error and
//! phase terms), not the raw cosine, taken over the items that actually
//! rendered. The timing log's `top=` is the best of the first three ranked
//! results before the hook's own filters; the floor was measured on that
//! field, and the rendered best is normally equal or lower, so the replay's
//! 25 sends are an upper bound on what the hook sends. Replayed over the 60
//! hand-labelled pairs in `tests/fixtures/prompt_gate_pairs.json` (see
//! `send_gate_tests`), the gate sends 25 blocks and keeps all 4 known uses.
//! It loads no model.
//! The fallback render (daemon unreachable) applies the same two conditions.
//!
//! `CSR_NO_PROMPT_GATE=1` turns the gate off. The daemon's render policy
//! carries the switch, so a client and a daemon that disagree abstain.

use super::formatter::{FormattedInjection, InjectionCategory};
use super::InjectionItem;

/// Lowest best ranked score that may send a block.
pub const TOP_SCORE_FLOOR: f32 = 0.45;
/// Fewest chunk or reflection items a sent block carries.
pub const MIN_RENDERED_ITEMS: usize = 3;
/// Set to `1` to turn the gate off.
pub const OFF_SWITCH: &str = "CSR_NO_PROMPT_GATE";

/// The gate's verdict on one render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Send,
    /// No ranked result, or the best one is under [`TOP_SCORE_FLOOR`].
    BelowFloor,
    /// The score clears the floor, but fewer than [`MIN_RENDERED_ITEMS`] render.
    ThinPool,
}

/// The verdict for a best ranked score and a count of rendered items.
pub fn decide(top_score: Option<f32>, rendered_items: usize) -> Gate {
    if !matches!(top_score, Some(score) if score >= TOP_SCORE_FLOOR) {
        Gate::BelowFloor
    } else if rendered_items < MIN_RENDERED_ITEMS {
        Gate::ThinPool
    } else {
        Gate::Send
    }
}

/// Whether the gate applies to this process.
pub fn enabled() -> bool {
    std::env::var(OFF_SWITCH).as_deref() != Ok("1")
}

/// Chunk and reflection items that reached the formatted block.
pub fn rendered_items(formatted: &FormattedInjection) -> usize {
    formatted
        .rendered_items
        .iter()
        .filter(|item| item.category == InjectionCategory::WinningStrategy)
        .count()
}

/// The best score among the chunk and reflection items that reached the
/// block. A result the hook's own filters dropped, or the budget cut, cannot
/// vouch for the items that did render.
pub fn top_rendered_score(
    winning: &[InjectionItem],
    formatted: &FormattedInjection,
) -> Option<f32> {
    formatted
        .rendered_items
        .iter()
        .filter(|item| item.category == InjectionCategory::WinningStrategy)
        .filter_map(|item| winning.get(item.index))
        .map(|item| item.score)
        .reduce(f32::max)
}

/// Sets or clears [`OFF_SWITCH`] and restores it on drop. Hold
/// `env_test_guard` while it lives.
#[cfg(test)]
pub(crate) struct Switch(Option<std::ffi::OsString>);

#[cfg(test)]
impl Switch {
    pub(crate) fn off() -> Self {
        let saved = std::env::var_os(OFF_SWITCH);
        std::env::set_var(OFF_SWITCH, "1");
        Self(saved)
    }

    pub(crate) fn on() -> Self {
        let saved = std::env::var_os(OFF_SWITCH);
        std::env::remove_var(OFF_SWITCH);
        Self(saved)
    }
}

#[cfg(test)]
impl Drop for Switch {
    fn drop(&mut self) {
        match &self.0 {
            Some(value) => std::env::set_var(OFF_SWITCH, value),
            None => std::env::remove_var(OFF_SWITCH),
        }
    }
}

#[cfg(test)]
#[path = "send_gate_tests.rs"]
mod tests;
