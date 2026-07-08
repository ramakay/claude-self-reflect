//! Pillar 4 — the closed-loop Governor.
//!
//! Tracks downstream *reuse* of injected facts and shrinks the injection budget
//! where injections go unused, growing it where they pay off. Honest measurement
//! is the whole point:
//!
//! - **Reuse rates only.** "Tokens saved" is counterfactual — it requires a
//!   holdout (a random ~10% of sessions get NO injection) to compare exploration
//!   spend. Without that holdout we report reuse rates and NEVER savings.
//! - **Anti-flap.** No budget change below a minimum sample size, and budget
//!   *decays* toward a floor rather than hard-cutting — one coincidental miss
//!   never silences a useful project.
//!
//! All functions are pure; the persisted reuse counter lives in the
//! `derivation_ledger` (`increment_ledger_reuse`).

/// Minimum observations before the governor adjusts anything (anti-flap).
pub const MIN_SAMPLE: u32 = 10;
/// Default / maximum injection budget in tokens.
pub const DEFAULT_BUDGET: usize = 300;
/// Floor the governor will never shrink below (memory stays alive, just quiet).
pub const MIN_BUDGET: usize = 50;

/// Fraction of injected facts that were later reused, in [0, 1]. Zero injections
/// → 0.0 (nothing to measure).
pub fn reuse_rate(reused: u32, injected: u32) -> f32 {
    if injected == 0 {
        return 0.0;
    }
    (reused as f32 / injected as f32).clamp(0.0, 1.0)
}

/// Adjust the injection budget from observed reuse. Below `MIN_SAMPLE` the budget
/// is unchanged (anti-flap). Otherwise it moves halfway toward a target that
/// scales with reuse — decay toward `MIN_BUDGET` when reuse is low, growth toward
/// `DEFAULT_BUDGET` when high. Never a hard cutoff.
pub fn adjust_budget(current: usize, reuse_rate: f32, sample_size: u32) -> usize {
    if sample_size < MIN_SAMPLE {
        return current;
    }
    let span = (DEFAULT_BUDGET - MIN_BUDGET) as f32;
    let target = MIN_BUDGET as f32 + span * reuse_rate.clamp(0.0, 1.0);
    let next = current as f32 + 0.5 * (target - current as f32);
    (next.round() as usize).clamp(MIN_BUDGET, DEFAULT_BUDGET)
}

/// Deterministic holdout assignment: ~`holdout_pct`% of sessions suppress
/// injection so reuse/savings can be measured against a counterfactual. Stable
/// (FNV-1a hash of the session id) — the same session is always on the same side,
/// and no RNG is needed (RNG is unavailable in this codebase by design).
pub fn is_holdout(session_id: &str, holdout_pct: u32) -> bool {
    if holdout_pct == 0 {
        return false;
    }
    (fnv1a(session_id) % 100) < holdout_pct as u64
}

fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Detect which injected facts were reused this session. A fact counts as reused
/// when its AST anchor name shows up among the symbols the session subsequently
/// touched (edited functions, referenced types). Returns reused fact ids.
pub fn detect_reused<'a>(
    injected: &'a [(String, Option<String>)],
    touched_symbols: &[String],
) -> Vec<&'a str> {
    injected
        .iter()
        .filter_map(|(id, anchor)| match anchor {
            Some(a) if touched_symbols.iter().any(|s| s == a) => Some(id.as_str()),
            _ => None,
        })
        .collect()
}

/// What the governor may honestly report.
#[derive(Debug, Clone, PartialEq)]
pub struct GovernorReport {
    pub injected: u32,
    pub reused: u32,
    pub reuse_rate: f32,
    pub budget: usize,
}

impl GovernorReport {
    pub fn new(injected: u32, reused: u32, budget: usize) -> Self {
        Self {
            injected,
            reused,
            reuse_rate: reuse_rate(reused, injected),
            budget,
        }
    }

    /// A savings claim is permitted ONLY when a holdout arm has enough sessions
    /// to compare against. Otherwise: reuse rates only.
    pub fn savings_claim_allowed(&self, holdout_sample: u32) -> bool {
        holdout_sample >= MIN_SAMPLE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reuse_rate_handles_zero_and_clamps() {
        assert_eq!(reuse_rate(0, 0), 0.0);
        assert_eq!(reuse_rate(3, 10), 0.3);
        assert_eq!(reuse_rate(10, 10), 1.0);
    }

    #[test]
    fn budget_unchanged_below_min_sample() {
        // Even with zero reuse, too few samples → no change (anti-flap).
        assert_eq!(adjust_budget(300, 0.0, MIN_SAMPLE - 1), 300);
    }

    #[test]
    fn budget_decays_toward_floor_when_unused() {
        // Low reuse, enough samples → shrink, but never below the floor and never
        // in one hard step.
        let next = adjust_budget(300, 0.0, 50);
        assert!(next < 300, "should shrink");
        assert!(next >= MIN_BUDGET, "never below floor");
        // Repeated low-reuse rounds converge toward the floor (asymptotic decay,
        // not an instant cut).
        let mut b = 300;
        for _ in 0..10 {
            b = adjust_budget(b, 0.0, 50);
        }
        assert!(
            (MIN_BUDGET..=MIN_BUDGET + 2).contains(&b),
            "should converge near floor, got {b}"
        );
    }

    #[test]
    fn budget_grows_back_when_reused() {
        let next = adjust_budget(MIN_BUDGET, 1.0, 50);
        assert!(next > MIN_BUDGET, "high reuse should grow budget");
        assert!(next <= DEFAULT_BUDGET);
    }

    #[test]
    fn holdout_is_deterministic_and_proportional() {
        // Same id → same arm every time.
        let id = "session-abc";
        assert_eq!(is_holdout(id, 10), is_holdout(id, 10));
        // 0% holdout → nobody suppressed.
        assert!(!is_holdout(id, 0));
        // ~10% over many ids lands in a sane band (not 0, not everyone).
        let in_holdout = (0..1000)
            .filter(|i| is_holdout(&format!("s{i}"), 10))
            .count();
        assert!(
            (40..160).contains(&in_holdout),
            "expected ~10% holdout, got {in_holdout}/1000"
        );
    }

    #[test]
    fn detect_reused_matches_touched_anchors() {
        let injected = vec![
            ("f1".to_string(), Some("validate_token".to_string())),
            ("f2".to_string(), Some("refresh_session".to_string())),
            ("f3".to_string(), None), // no anchor — never auto-counted
        ];
        let touched = vec!["validate_token".to_string(), "other".to_string()];
        assert_eq!(detect_reused(&injected, &touched), vec!["f1"]);
    }

    #[test]
    fn savings_claim_requires_holdout_sample() {
        let r = GovernorReport::new(10, 4, 300);
        assert_eq!(r.reuse_rate, 0.4);
        assert!(!r.savings_claim_allowed(MIN_SAMPLE - 1)); // no/thin holdout → reuse only
        assert!(r.savings_claim_allowed(MIN_SAMPLE)); // enough holdout → savings allowed
    }
}
