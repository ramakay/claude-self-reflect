//! Journal v4 Phase 5 — spend policy for a night pass: the **effort tier**
//! (locked decision 14) and the **hard budget cap** (locked decision 8).
//!
//! # Effort tiers
//!
//! `CSR_DREAM_EFFORT` = `less` | `balanced` | `max`, default `balanced`. A
//! tier controls two things and says exactly which:
//!
//! * **Episodes per pass** — how many candidate episodes / dream items the
//!   night pass will look at at all ([`EffortTier::episodes_per_pass`]).
//! * **Reasoning effort** — delivered as the model the night actor runs at
//!   ([`EffortTier::model`]), because `claude -p` exposes no separate
//!   reasoning-effort flag. `CSR_DREAM_THREAD_MODEL` still wins over the
//!   tier: an explicit model choice is never silently overridden by a tier
//!   default. The tier's model is folded into `dream::threads::episode_hash`
//!   like any other target model, so changing tier re-derives rather than
//!   serving work done at a different effort.
//!
//! An **invalid** value is never a panic and never a silent default: it falls
//! back to `balanced` AND increments a counter surfaced in
//! `csr-engine status` ([`record_invalid_effort`] / [`invalid_effort_count`]),
//! so a typo in a shell profile is visible rather than quietly halving spend.
//!
//! # Budget cap
//!
//! [`Budget`] is a hard cap on **model invocations per pass** — every actor
//! call, including chain fallbacks and the verifier's re-prompt retry, spends
//! one unit. It cannot be exceeded across retries because the cap is enforced
//! at the single choke point every invocation passes through
//! ([`BudgetedActor::invoke`]), not at the call sites that decide to retry.
//!
//! When the budget runs out mid-pass the remaining candidates are simply not
//! attempted: nothing is cached for them (the producers only cache a reply
//! that an actor actually returned), so they are **queued for the next pass**
//! by construction. Candidates are consumed newest-first by the existing
//! producers' own ordering.

use std::cell::Cell;

use crate::dream::threads::{ActorAttempt, NightActor};
use crate::storage::Storage;

/// `meta` key: cumulative count of invalid `CSR_DREAM_EFFORT` values observed.
pub const META_INVALID_EFFORT: &str = "dream_effort_invalid_count";

/// Default hard cap on model invocations per pass (locked decision 8).
pub const DEFAULT_BUDGET: usize = 25;
/// Documented override for [`DEFAULT_BUDGET`].
pub const BUDGET_ENV: &str = "CSR_DREAM_BUDGET";
/// Documented effort-tier selector.
pub const EFFORT_ENV: &str = "CSR_DREAM_EFFORT";

/// The three effort tiers. `balanced` is the default and is deliberately the
/// tier whose model equals `dream::threads`'s historical default, so enabling
/// tiers changes nothing for a user who never sets the variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EffortTier {
    Less,
    #[default]
    Balanced,
    Max,
}

impl EffortTier {
    /// Canonical lowercase name — what `status` prints and what the setup
    /// screen echoes back.
    pub fn as_str(self) -> &'static str {
        match self {
            EffortTier::Less => "less",
            EffortTier::Balanced => "balanced",
            EffortTier::Max => "max",
        }
    }

    /// Parse a configured value. `None` for anything that is not one of the
    /// three documented names (the caller decides whether that is a
    /// fallback-with-counter or a hard error).
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "less" => Some(EffortTier::Less),
            "balanced" => Some(EffortTier::Balanced),
            "max" => Some(EffortTier::Max),
            _ => None,
        }
    }

    /// Candidate episodes / dream items considered in one pass.
    pub fn episodes_per_pass(self) -> usize {
        match self {
            EffortTier::Less => 8,
            EffortTier::Balanced => 20,
            EffortTier::Max => 40,
        }
    }

    /// The model the night actor runs at — this tier's "reasoning effort".
    /// Overridden by `CSR_DREAM_THREAD_MODEL` (see
    /// `dream::threads::thread_model_candidates`).
    pub fn model(self) -> &'static str {
        match self {
            EffortTier::Less => "haiku-4",
            EffortTier::Balanced => "sonnet-5",
            EffortTier::Max => "opus-5",
        }
    }

    /// The documented label for the effort this tier buys. Descriptive only
    /// — the mechanism is [`EffortTier::model`].
    pub fn reasoning_effort(self) -> &'static str {
        match self {
            EffortTier::Less => "low",
            EffortTier::Balanced => "medium",
            EffortTier::Max => "high",
        }
    }

    /// Default invocation budget for this tier, before the
    /// [`BUDGET_ENV`] override. A tier that looks at more episodes needs room
    /// to actually reach them; the cap is still hard.
    pub fn default_budget(self) -> usize {
        match self {
            EffortTier::Less => 10,
            EffortTier::Balanced => DEFAULT_BUDGET,
            EffortTier::Max => 50,
        }
    }
}

/// Resolve the configured tier from a raw value. `Ok(tier)` for a valid or
/// absent value; `Err(tier)` carries the `balanced` fallback for an invalid
/// one so the caller can count it.
pub fn resolve_effort(raw: Option<&str>) -> Result<EffortTier, EffortTier> {
    match raw.map(str::trim) {
        None | Some("") => Ok(EffortTier::default()),
        Some(value) => EffortTier::parse(value).ok_or(EffortTier::default()),
    }
}

/// The configured tier, reading `CSR_DREAM_EFFORT`. An invalid value falls
/// back to `balanced` and logs; use [`effort_tier_counted`] where a storage
/// handle is available so the fallback is also counted for `status`.
pub fn effort_tier() -> EffortTier {
    match resolve_effort(std::env::var(EFFORT_ENV).ok().as_deref()) {
        Ok(tier) => tier,
        Err(fallback) => {
            tracing::warn!(
                env = EFFORT_ENV,
                "invalid dream effort tier; using {}",
                fallback.as_str()
            );
            fallback
        }
    }
}

/// [`effort_tier`], plus a persisted counter when the value was invalid.
pub fn effort_tier_counted(storage: &Storage) -> EffortTier {
    match resolve_effort(std::env::var(EFFORT_ENV).ok().as_deref()) {
        Ok(tier) => tier,
        Err(fallback) => {
            record_invalid_effort(storage);
            tracing::warn!(
                env = EFFORT_ENV,
                "invalid dream effort tier; using {}",
                fallback.as_str()
            );
            fallback
        }
    }
}

/// Read the invalid-tier counter. `None` when the key was never written —
/// the caller renders nothing rather than a zero it did not measure.
pub fn invalid_effort_count(storage: &Storage) -> Option<i64> {
    storage
        .get_meta(META_INVALID_EFFORT)
        .ok()
        .flatten()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
}

/// Increment the invalid-tier counter. Fail-soft: a storage error is dropped,
/// never propagated into a hook or daemon loop.
pub fn record_invalid_effort(storage: &Storage) {
    let next = invalid_effort_count(storage).unwrap_or(0).saturating_add(1);
    let _ = storage.set_meta(META_INVALID_EFFORT, &next.to_string());
}

/// The pass's invocation cap: `CSR_DREAM_BUDGET` when it parses as a positive
/// integer, else the tier's default. `0` and garbage both fall back rather
/// than silently disabling the pass — an explicit "no dreaming" is
/// `CSR_NO_DREAMING=1`, not a zero budget.
pub fn budget_cap(tier: EffortTier) -> usize {
    budget_cap_from(std::env::var(BUDGET_ENV).ok().as_deref(), tier)
}

/// Pure core of [`budget_cap`] — the tests drive this with explicit values
/// rather than mutating process-global environment (two independent env test
/// locks already exist in this crate; adding a third variable to the race
/// surface would be the flakiness, not the coverage).
pub fn budget_cap_from(raw: Option<&str>, tier: EffortTier) -> usize {
    let Some(raw) = raw else {
        return tier.default_budget();
    };
    raw.trim()
        .parse::<usize>()
        .ok()
        .filter(|&value| value > 0)
        .unwrap_or_else(|| {
            tracing::warn!(value = %raw, env = BUDGET_ENV, "invalid dream budget; using tier default");
            tier.default_budget()
        })
}

// ─── per-night token estimate (locked decision 15) ────────────────────────

/// Prompt ceiling in `dream::threads` is 8 KiB; at the ~4 chars/token rule of
/// thumb that is ~2,048 input tokens for a full-size prompt.
pub const EST_INPUT_TOKENS_PER_CALL: u64 = 2_048;
/// A reply is at most 4 threads of one sentence each plus quotes — ~400
/// output tokens at the cap.
pub const EST_OUTPUT_TOKENS_PER_CALL: u64 = 400;
/// Hours of a night an idle-triggered pass could plausibly land in. Used
/// only to bound how many passes a night can hold.
pub const EST_NIGHT_HOURS: u64 = 8;

/// A per-night spend estimate, with the numbers it was derived from carried
/// alongside so the reader can check it. Every field is a bound, not a
/// measurement — [`NightEstimate::label`] says so in words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NightEstimate {
    /// Candidate episodes measured in the corpus right now.
    pub candidates: usize,
    /// Passes a night can hold at the configured cadence (at least the one
    /// guaranteed nightly floor pass).
    pub passes: usize,
    /// Upper bound on model invocations across those passes.
    pub invocations: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl NightEstimate {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    /// One line, explicitly labelled an estimate and naming its basis.
    pub fn label(&self, tier: EffortTier) -> String {
        format!(
            "~{} tokens/night (estimate: up to {} model calls at effort '{}',              from {} candidate episodes in your corpus; a call is capped at              ~{} in / ~{} out)",
            self.total_tokens(),
            self.invocations,
            tier.as_str(),
            self.candidates,
            EST_INPUT_TOKENS_PER_CALL,
            EST_OUTPUT_TOKENS_PER_CALL,
        )
    }
}

/// Estimate one night's dreaming spend from the **measured** candidate count,
/// the tier, the pass budget and the cadence interval.
///
/// Deliberately an upper bound: it assumes every allowed invocation happens
/// and every prompt is full-size. Convergence (an unchanged corpus costs
/// nothing on a re-run) means real spend is usually far lower — which is why
/// this is presented as a ceiling, never as a forecast.
pub fn estimate_night(
    candidates: usize,
    tier: EffortTier,
    cap: usize,
    interval_secs: u64,
) -> NightEstimate {
    let per_pass = candidates.min(tier.episodes_per_pass()).min(cap);
    let passes = (EST_NIGHT_HOURS * 3_600)
        .checked_div(interval_secs)
        .unwrap_or(1)
        .max(1) as usize;
    let invocations = per_pass.saturating_mul(passes);
    NightEstimate {
        candidates,
        passes,
        invocations,
        input_tokens: invocations as u64 * EST_INPUT_TOKENS_PER_CALL,
        output_tokens: invocations as u64 * EST_OUTPUT_TOKENS_PER_CALL,
    }
}

/// A hard per-pass invocation counter. Single-threaded by construction — one
/// pass runs inside one `spawn_blocking` closure — so a `Cell` is enough and
/// no lock can be contended on the spend path.
#[derive(Debug)]
pub struct Budget {
    cap: usize,
    used: Cell<usize>,
    /// Candidates skipped because the budget was already gone — the measured
    /// remainder queued for the next pass.
    queued: Cell<usize>,
}

impl Budget {
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            used: Cell::new(0),
            queued: Cell::new(0),
        }
    }

    /// A budget sized from the environment for `tier`.
    pub fn for_tier(tier: EffortTier) -> Self {
        Self::new(budget_cap(tier))
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn used(&self) -> usize {
        self.used.get()
    }

    pub fn remaining(&self) -> usize {
        self.cap.saturating_sub(self.used.get())
    }

    pub fn exhausted(&self) -> bool {
        self.remaining() == 0
    }

    /// Claim one invocation. `false` means the cap is reached — the caller
    /// must NOT invoke anything.
    pub fn try_spend(&self) -> bool {
        if self.remaining() == 0 {
            return false;
        }
        self.used.set(self.used.get() + 1);
        true
    }

    /// Record that one candidate was skipped for want of budget.
    pub fn note_queued(&self) {
        self.queued.set(self.queued.get() + 1);
    }

    pub fn queued(&self) -> usize {
        self.queued.get()
    }
}

/// Wraps any [`NightActor`] so that **every** invocation — first attempt,
/// model-chain fallback, verifier retry — is charged to one [`Budget`]. Once
/// the cap is reached the wrapper returns [`ActorAttempt::Failed`] WITHOUT
/// invoking the inner actor, which the producers treat as "no usable reply",
/// so nothing is cached and the candidate is retried on the next pass.
pub(crate) struct BudgetedActor<'a> {
    inner: &'a dyn NightActor,
    budget: &'a Budget,
}

impl<'a> BudgetedActor<'a> {
    pub(crate) fn new(inner: &'a dyn NightActor, budget: &'a Budget) -> Self {
        Self { inner, budget }
    }
}

impl NightActor for BudgetedActor<'_> {
    fn invoke(&self, model: Option<&str>, prompt: &str) -> ActorAttempt {
        if !self.budget.try_spend() {
            return ActorAttempt::Failed(format!(
                "dream budget exhausted ({} of {} invocations used)",
                self.budget.used(),
                self.budget.cap()
            ));
        }
        self.inner.invoke(model, prompt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::narrative::ParsedNarrative;

    fn parsed(text: &str) -> ActorAttempt {
        ActorAttempt::Parsed(ParsedNarrative {
            text: text.to_string(),
            model: "sonnet-5".to_string(),
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        })
    }

    // ── tier parsing / fallback ──

    #[test]
    fn each_documented_tier_parses_to_its_own_behaviour() {
        for (raw, tier, episodes, model, effort) in [
            ("less", EffortTier::Less, 8, "haiku-4", "low"),
            ("balanced", EffortTier::Balanced, 20, "sonnet-5", "medium"),
            ("max", EffortTier::Max, 40, "opus-5", "high"),
        ] {
            let parsed = EffortTier::parse(raw).expect("documented tier must parse");
            assert_eq!(parsed, tier);
            assert_eq!(parsed.as_str(), raw);
            assert_eq!(parsed.episodes_per_pass(), episodes);
            assert_eq!(parsed.model(), model);
            assert_eq!(parsed.reasoning_effort(), effort);
        }
    }

    #[test]
    fn tier_names_are_case_and_whitespace_insensitive() {
        assert_eq!(EffortTier::parse(" MAX "), Some(EffortTier::Max));
        assert_eq!(EffortTier::parse("Balanced"), Some(EffortTier::Balanced));
    }

    #[test]
    fn tiers_are_strictly_ordered_in_episodes_and_budget() {
        assert!(EffortTier::Less.episodes_per_pass() < EffortTier::Balanced.episodes_per_pass());
        assert!(EffortTier::Balanced.episodes_per_pass() < EffortTier::Max.episodes_per_pass());
        assert!(EffortTier::Less.default_budget() < EffortTier::Balanced.default_budget());
        assert!(EffortTier::Balanced.default_budget() < EffortTier::Max.default_budget());
        assert_eq!(EffortTier::Balanced.default_budget(), DEFAULT_BUDGET);
    }

    #[test]
    fn absent_or_empty_value_is_the_default_tier_not_an_error() {
        assert_eq!(resolve_effort(None), Ok(EffortTier::Balanced));
        assert_eq!(resolve_effort(Some("   ")), Ok(EffortTier::Balanced));
        assert_eq!(EffortTier::default(), EffortTier::Balanced);
    }

    #[test]
    fn invalid_value_falls_back_to_balanced_instead_of_panicking() {
        for raw in ["maximum", "high", "0", "🙂", "less "] {
            let resolved = resolve_effort(Some(raw));
            if raw.trim() == "less" {
                assert_eq!(resolved, Ok(EffortTier::Less));
            } else {
                assert_eq!(
                    resolved,
                    Err(EffortTier::Balanced),
                    "{raw:?} must be an invalid value that falls back"
                );
            }
        }
    }

    #[test]
    fn invalid_value_increments_a_status_counter() {
        let storage = Storage::open_memory().unwrap();
        assert_eq!(
            invalid_effort_count(&storage),
            None,
            "an unwritten counter must read as None, never 0"
        );
        record_invalid_effort(&storage);
        record_invalid_effort(&storage);
        assert_eq!(invalid_effort_count(&storage), Some(2));
    }

    // ── budget cap ──

    #[test]
    fn budget_cap_defaults_to_the_tier_when_unset() {
        assert_eq!(budget_cap_from(None, EffortTier::Less), 10);
        assert_eq!(budget_cap_from(None, EffortTier::Balanced), DEFAULT_BUDGET);
        assert_eq!(budget_cap_from(None, EffortTier::Max), 50);
    }

    #[test]
    fn budget_cap_env_override_wins_and_garbage_falls_back() {
        assert_eq!(budget_cap_from(Some("3"), EffortTier::Balanced), 3);
        assert_eq!(
            budget_cap_from(Some("0"), EffortTier::Balanced),
            DEFAULT_BUDGET,
            "0 must fall back — an explicit off switch is CSR_NO_DREAMING"
        );
        assert_eq!(
            budget_cap_from(Some("not-a-number"), EffortTier::Balanced),
            DEFAULT_BUDGET
        );
        assert_eq!(budget_cap_from(Some(" 7 "), EffortTier::Max), 7);
    }

    #[test]
    fn budget_counts_down_and_stops_at_zero() {
        let budget = Budget::new(2);
        assert_eq!(budget.remaining(), 2);
        assert!(budget.try_spend());
        assert!(budget.try_spend());
        assert!(!budget.try_spend());
        assert_eq!(budget.used(), 2, "a refused spend must not be counted");
        assert!(budget.exhausted());
    }

    #[test]
    fn budgeted_actor_never_exceeds_the_cap_across_retries() {
        // The producers retry (verifier re-prompt) and walk a model chain, so
        // the cap has to hold at the invocation choke point, not at the call
        // site: drive 50 attempts through a cap of 4.
        let calls = Cell::new(0_usize);
        let inner = |_model: Option<&str>, _prompt: &str| {
            calls.set(calls.get() + 1);
            parsed("[]")
        };
        let budget = Budget::new(4);
        let actor = BudgetedActor::new(&inner, &budget);
        let mut refusals = 0;
        for _ in 0..50 {
            if let ActorAttempt::Failed(msg) = actor.invoke(Some("sonnet-5"), "prompt") {
                assert!(
                    msg.contains("budget exhausted"),
                    "unexpected failure: {msg}"
                );
                refusals += 1;
            }
        }
        assert_eq!(calls.get(), 4, "inner actor invoked past the cap");
        assert_eq!(refusals, 46);
        assert_eq!(budget.used(), 4);
        assert!(budget.exhausted());
    }

    #[test]
    fn a_refused_invocation_does_not_reach_the_inner_actor_at_all() {
        let calls = Cell::new(0_usize);
        let inner = |_m: Option<&str>, _p: &str| {
            calls.set(calls.get() + 1);
            parsed("[]")
        };
        let budget = Budget::new(0);
        let actor = BudgetedActor::new(&inner, &budget);
        assert!(matches!(
            actor.invoke(None, "prompt"),
            ActorAttempt::Failed(_)
        ));
        assert_eq!(calls.get(), 0);
    }

    // ── per-night estimate ──

    #[test]
    fn estimate_is_bounded_by_the_corpus_the_tier_and_the_cap() {
        // Corpus smaller than the tier: the corpus binds.
        let small = estimate_night(3, EffortTier::Max, 50, 6 * 3600);
        assert_eq!(small.candidates, 3);
        assert_eq!(small.invocations, 3);
        assert_eq!(
            small.total_tokens(),
            3 * (EST_INPUT_TOKENS_PER_CALL + EST_OUTPUT_TOKENS_PER_CALL)
        );

        // Corpus larger than the tier: the tier binds.
        let tier_bound = estimate_night(500, EffortTier::Less, 50, 6 * 3600);
        assert_eq!(
            tier_bound.invocations,
            EffortTier::Less.episodes_per_pass(),
            "the tier must bound the estimate"
        );

        // Cap smaller than both: the cap binds.
        let cap_bound = estimate_night(500, EffortTier::Max, 5, 6 * 3600);
        assert_eq!(cap_bound.invocations, 5);
    }

    #[test]
    fn estimate_scales_with_how_many_passes_a_night_holds() {
        let one = estimate_night(10, EffortTier::Balanced, 25, 8 * 3600);
        assert_eq!(one.passes, 1);
        let four = estimate_night(10, EffortTier::Balanced, 25, 2 * 3600);
        assert_eq!(four.passes, 4);
        assert_eq!(four.invocations, 40);
        // A cadence longer than the night still guarantees the floor pass.
        let long = estimate_night(10, EffortTier::Balanced, 25, 7 * 24 * 3600);
        assert_eq!(long.passes, 1);
    }

    #[test]
    fn an_empty_corpus_estimates_zero_tokens_from_a_measured_zero() {
        let none = estimate_night(0, EffortTier::Balanced, 25, 6 * 3600);
        assert_eq!(none.invocations, 0);
        assert_eq!(none.total_tokens(), 0);
        assert!(none
            .label(EffortTier::Balanced)
            .contains("from 0 candidate"));
    }

    #[test]
    fn the_estimate_label_says_it_is_an_estimate_and_names_its_basis() {
        let estimate = estimate_night(12, EffortTier::Balanced, 25, 6 * 3600);
        let label = estimate.label(EffortTier::Balanced);
        assert!(label.contains("estimate"), "must be labelled: {label}");
        assert!(label.contains("tokens/night"));
        assert!(label.contains("balanced"));
        assert!(
            label.contains("12 candidate episodes"),
            "the corpus basis must be shown: {label}"
        );
    }

    #[test]
    fn queued_remainder_is_counted_not_inferred() {
        let budget = Budget::new(1);
        assert_eq!(budget.queued(), 0);
        budget.note_queued();
        budget.note_queued();
        assert_eq!(budget.queued(), 2);
    }
}
