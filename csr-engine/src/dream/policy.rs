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
//! # Budget cap — per NIGHT, not per pass
//!
//! Locked decision 8 caps spend **per night**. A per-pass counter alone does
//! not do that: an idle machine can tick several times before the floor
//! boundary, and a daemon restart hands the next tick a fresh allowance, so
//! `n` passes would each spend the full cap. The cap is therefore anchored in
//! a durable **night ledger** ([`claim_night`], stored under
//! [`META_NIGHT_LEDGER`] and keyed by the local-night bucket
//! [`night_key`]):
//!
//! * each invocation **claims** one unit from tonight's remainder immediately
//!   before it runs — two ticks in one night therefore share one cap, and a
//!   restart mid-night reads the same persisted balance rather than refilling
//!   it;
//! * unused allowance is never claimed, so an early-finishing pass does not
//!   burn the night's remaining budget;
//! * a pass killed after a claim does not refund it, which is the conservative
//!   direction: allowance is never handed out twice.
//!
//! Within a pass, [`Budget`] is still the hard invocation counter — every
//! actor call, including chain fallbacks and the verifier's re-prompt retry,
//! spends one unit. It cannot be exceeded across retries because the cap is
//! enforced at the single choke point every invocation passes through
//! ([`BudgetedActor::invoke`]), not at the call sites that decide to retry.
//!
//! When the budget runs out mid-pass the remaining candidates are simply not
//! attempted: nothing is cached for them (the producers only cache a reply
//! that an actor actually returned), so they are **queued for the next pass**
//! by construction. Candidates are consumed newest-first by the existing
//! producers' own ordering.
//!
//! # Accounting cannot fail open
//!
//! The same choke point writes a durable **usage reservation**
//! (`storage::usage_reservation`) BEFORE the inner actor is invoked. A
//! reservation that cannot be written refuses the invocation outright — an
//! unrecordable call must not happen at all — and a reservation that is never
//! settled stays `reserved`, which `status` reports as an unaccounted call
//! rather than rounding to zero spend. `dream::threads::record_attempts`
//! settles it against the `narrative_usage` row that measured the call, in
//! the same transaction that writes that row.

use std::cell::{Cell, RefCell};

use anyhow::Result;

use crate::dream::threads::{ActorAttempt, NightActor};
use crate::storage::{queries, usage_reservation, Storage};

/// `meta` key: cumulative count of invalid `CSR_DREAM_EFFORT` values observed.
pub const META_INVALID_EFFORT: &str = "dream_effort_invalid_count";

/// Default hard cap on model invocations **per night** (locked decision 8).
pub const DEFAULT_BUDGET: usize = 25;
/// Documented override for [`DEFAULT_BUDGET`].
pub const BUDGET_ENV: &str = "CSR_DREAM_BUDGET";
/// Documented effort-tier selector.
pub const EFFORT_ENV: &str = "CSR_DREAM_EFFORT";

/// `meta` key: the current local night's invocation ledger, as JSON
/// [`NightLedger`]. One row, rewritten in place; a different `night` value
/// means the bucket has rolled and the count starts again.
pub const META_NIGHT_LEDGER: &str = "dream_night_ledger";
/// `meta` key: cumulative count of usage-accounting writes that failed (a
/// reservation that could not be written, or a usage row / finalisation that
/// could not be committed). Surfaced by `status`; never silently dropped.
pub const META_ACCOUNTING_FAILURES: &str = "dream_usage_accounting_failures";

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

// ─── the night ledger (locked decision 8: a cap per NIGHT) ────────────────

/// Tonight's invocation ledger as stored. `reserved` is what has been handed
/// out to passes, not what they proved they spent — a pass that dies without
/// releasing its remainder leaves the allowance claimed, which is the
/// conservative direction for a spend cap.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NightLedger {
    pub night: String,
    pub reserved: usize,
}

/// The local-night bucket a moment belongs to: the date of the most recent
/// nightly-floor boundary at or before it. Derived from
/// `dream_cadence::floor_boundary` rather than restated, so the ledger's night
/// and the floor pass's night can never drift apart.
pub fn night_key(now_local: chrono::NaiveDateTime, floor_hour: u32) -> String {
    crate::daemon::dream_cadence::floor_boundary(now_local, floor_hour)
        .date()
        .format("%Y-%m-%d")
        .to_string()
}

/// [`night_key`] for an offset-bearing local timestamp. Keeping the UTC
/// offset attached until this boundary makes daylight-saving transitions
/// explicit in tests while the bucket itself remains a local calendar date.
pub fn night_key_at<Tz: chrono::TimeZone>(
    now_local: chrono::DateTime<Tz>,
    floor_hour: u32,
) -> String {
    night_key(now_local.naive_local(), floor_hour)
}

/// The night bucket for the system clock and configured floor hour.
pub fn current_night_key() -> String {
    night_key_at(
        chrono::Utc::now().with_timezone(&chrono::Local),
        crate::daemon::dream_cadence::floor_hour(),
    )
}

/// The stored ledger, or `None` when none was ever written. A read or parse
/// failure is an `Err`, never a `None` — a ledger that cannot be read must not
/// be mistaken for a night with nothing spent yet.
pub fn read_night_ledger(storage: &Storage) -> Result<Option<NightLedger>> {
    let Some(raw) = storage.get_meta(META_NIGHT_LEDGER)? else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_str::<NightLedger>(&raw)?))
}

/// How much of `night`'s allowance is already claimed. A ledger written for a
/// *different* night reads as 0 for this one — that is the bucket rolling, not
/// an inference from absence. A ledger that cannot be read is an `Err`.
pub fn night_claimed(storage: &Storage, night: &str) -> Result<usize> {
    Ok(read_night_ledger(storage)?
        .filter(|ledger| ledger.night == night)
        .map(|ledger| ledger.reserved)
        .unwrap_or(0))
}

/// Invocations still available tonight under `cap`.
pub fn night_remaining(storage: &Storage, night: &str, cap: usize) -> Result<usize> {
    Ok(cap.saturating_sub(night_claimed(storage, night)?))
}

/// Claim up to `want` invocations from `night`'s remaining allowance under
/// `cap`, and return how many were actually granted. This is the one place
/// the nightly cap is enforced, and it is durable: the claim survives the
/// pass, the daemon process and the machine.
///
/// The read-modify-write runs inside one `BEGIN IMMEDIATE` transaction (the
/// idiom `witness_verdicts::insert_verdict_if_changed` uses) so two processes
/// against the same database cannot both be granted the same last invocation.
/// Within a process the `Storage` mutex already serializes it.
pub fn claim_night(storage: &Storage, night: &str, cap: usize, want: usize) -> Result<usize> {
    storage.with_connection(|conn| {
        let tx =
            rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
        let claimed = match queries::get_meta(&tx, META_NIGHT_LEDGER)? {
            Some(raw) => serde_json::from_str::<NightLedger>(&raw)?,
            None => NightLedger {
                night: night.to_string(),
                reserved: 0,
            },
        };
        let already = if claimed.night == night {
            claimed.reserved
        } else {
            0
        };
        let granted = cap.saturating_sub(already).min(want);
        if granted == 0 && claimed.night == night {
            return Ok(0); // nothing to write; tonight is spent.
        }
        let next = NightLedger {
            night: night.to_string(),
            reserved: already.saturating_add(granted),
        };
        queries::set_meta(&tx, META_NIGHT_LEDGER, &serde_json::to_string(&next)?)?;
        tx.commit()?;
        Ok(granted)
    })
}

// ─── usage-accounting failure counter ─────────────────────────────────────

/// How many usage-accounting writes have failed. `None` when the counter was
/// never written — which is "never observed", not "observed zero".
pub fn accounting_failure_count(storage: &Storage) -> Option<i64> {
    storage
        .get_meta(META_ACCOUNTING_FAILURES)
        .ok()
        .flatten()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
}

/// Record one accounting failure. Best-effort by necessity (the storage that
/// just failed is the same storage this writes to), but never silent: the
/// caller logs at error level as well.
pub fn note_accounting_failure(storage: &Storage) {
    let next = accounting_failure_count(storage)
        .unwrap_or(0)
        .saturating_add(1);
    let _ = storage.set_meta(META_ACCOUNTING_FAILURES, &next.to_string());
}

/// Invocations that started and never had their spend measured — reservations
/// still in `reserved` state. A non-zero count is a known unknown.
pub fn unaccounted_invocations(storage: &Storage) -> Option<i64> {
    storage
        .with_connection(usage_reservation::unaccounted_count)
        .ok()
}

// ─── reservation hand-off (choke point → usage writer) ────────────────────

thread_local! {
    /// The attempt key [`BudgetedActor::invoke`] just reserved, waiting for
    /// the caller that collects the attempt to take it.
    ///
    /// A single slot rather than a queue, because the handoff is immediate:
    /// `dream::threads::invoke_chain` takes the key on the statement right
    /// after `actor.invoke` returns, before any other invocation can happen
    /// (one pass runs single-threaded inside one `spawn_blocking` closure).
    /// If a key is ever overwritten before being taken, the older reservation
    /// is simply never settled — it stays `reserved` and is counted as an
    /// unaccounted call, which is the honest outcome rather than a mispaired
    /// usage row.
    static PENDING_RESERVATION: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub(crate) fn push_pending_reservation(key: String) {
    PENDING_RESERVATION.with(|slot| {
        if let Some(stale) = slot.borrow_mut().replace(key) {
            tracing::warn!(
                attempt_key = %stale,
                "dream usage reservation was never settled; it stays unaccounted"
            );
        }
    });
}

/// Take the reservation key belonging to the invocation that just returned.
pub(crate) fn take_pending_reservation() -> Option<String> {
    PENDING_RESERVATION.with(|slot| slot.borrow_mut().take())
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
/// the tier, the nightly budget and the cadence interval.
///
/// `cap` is the NIGHTLY cap, so it bounds the whole night and not each pass:
/// several passes in one night share it (see the night ledger above), and the
/// estimate must say the same thing the enforcement does. Modelling
/// `passes × cap` would promise a ceiling the implementation refuses to spend.
///
/// Deliberately an upper bound within that cap: it assumes every allowed
/// invocation happens and every prompt is full-size. Convergence (an unchanged
/// corpus costs nothing on a re-run) means real spend is usually far lower —
/// which is why this is presented as a ceiling, never as a forecast.
pub fn estimate_night(
    candidates: usize,
    tier: EffortTier,
    cap: usize,
    interval_secs: u64,
) -> NightEstimate {
    let per_pass = candidates.min(tier.episodes_per_pass());
    let passes = (EST_NIGHT_HOURS * 3_600)
        .checked_div(interval_secs)
        .unwrap_or(1)
        .max(1) as usize;
    let invocations = per_pass.saturating_mul(passes).min(cap);
    NightEstimate {
        candidates,
        passes,
        invocations,
        input_tokens: invocations as u64 * EST_INPUT_TOKENS_PER_CALL,
        output_tokens: invocations as u64 * EST_OUTPUT_TOKENS_PER_CALL,
    }
}

/// What a pass measured about its own spend, detached from the borrow the
/// live [`Budget`] holds — what the daemon persists after the pass has ended
/// and its budget has been dropped (and its remainder released).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BudgetSnapshot {
    pub cap: usize,
    pub used: usize,
    pub queued: usize,
}

/// The night ledger a [`Budget`] spends against, plus the accounting context
/// every invocation through it is recorded under.
struct PassAccounting<'a> {
    storage: &'a Storage,
    night: String,
    /// The NIGHTLY cap this pass debits against — not the pass's own cap.
    nightly_cap: usize,
    /// Per-pass unique prefix for reservation keys, so a retry after a
    /// restart claims a NEW row rather than silently reusing (and
    /// under-counting) the one a previous, unfinished pass left behind.
    nonce: String,
}

/// A hard invocation counter for one pass, spending against the durable night
/// ledger when it was built with [`Budget::for_night`]. Single-threaded by
/// construction — one pass runs inside one `spawn_blocking` closure — so a
/// `Cell` is enough and no lock can be contended on the spend path.
pub struct Budget<'a> {
    cap: usize,
    used: Cell<usize>,
    /// Candidates skipped because the budget was already gone — the measured
    /// remainder queued for the next pass.
    queued: Cell<usize>,
    /// Invocation counter feeding reservation keys (never reset).
    seq: Cell<usize>,
    /// Set when the night ledger refused a debit — the pass is finished
    /// spending even though its own local counter has room. Kept separate
    /// from `used` so the recorded usage stays a measurement.
    closed: Cell<bool>,
    accounting: Option<PassAccounting<'a>>,
}

impl std::fmt::Debug for Budget<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Budget")
            .field("cap", &self.cap)
            .field("used", &self.used.get())
            .field("queued", &self.queued.get())
            .field(
                "night",
                &self.accounting.as_ref().map(|acc| acc.night.as_str()),
            )
            .finish()
    }
}

impl Budget<'static> {
    /// A pass-local budget with no night ledger and no durable accounting —
    /// the shape tests and one-shot manual runs use.
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            used: Cell::new(0),
            queued: Cell::new(0),
            seq: Cell::new(0),
            closed: Cell::new(false),
            accounting: None,
        }
    }

    /// A budget sized from the environment for `tier`, with no night ledger.
    /// Prefer [`Budget::for_night`] anywhere the nightly cap must hold.
    pub fn for_tier(tier: EffortTier) -> Self {
        Self::new(budget_cap(tier))
    }
}

impl<'a> Budget<'a> {
    /// A budget that spends against **tonight's remaining allowance**, one
    /// durable debit per invocation. Two passes in one night therefore share
    /// one cap, and a daemon restart reads the same persisted balance instead
    /// of being handed a fresh one. Nothing is claimed up front, so a pass
    /// that dies mid-flight forfeits only what it actually spent.
    ///
    /// A ledger that cannot be read starts the pass at **zero** — an
    /// unenforceable cap must not become an unlimited one. The pass then does
    /// no model work at all and every candidate is counted as queued.
    pub fn for_night(storage: &'a Storage, tier: EffortTier, night: &str) -> Self {
        Self::for_night_with_cap(storage, night, budget_cap(tier))
    }

    /// [`Budget::for_night`] with the nightly cap passed explicitly rather
    /// than resolved from the environment — the shape the ledger tests drive,
    /// and the seam that keeps them off process-global state.
    pub fn for_night_with_cap(storage: &'a Storage, night: &str, cap: usize) -> Self {
        let remaining = match night_remaining(storage, night, cap) {
            Ok(remaining) => remaining,
            Err(error) => {
                tracing::error!(
                    %error,
                    "dream night ledger unreadable; refusing to spend this pass"
                );
                note_accounting_failure(storage);
                0
            }
        };
        Self {
            cap: remaining,
            used: Cell::new(0),
            queued: Cell::new(0),
            seq: Cell::new(0),
            closed: Cell::new(false),
            accounting: Some(PassAccounting {
                storage,
                night: night.to_string(),
                nightly_cap: cap,
                nonce: uuid::Uuid::new_v4().to_string(),
            }),
        }
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn used(&self) -> usize {
        self.used.get()
    }

    pub fn remaining(&self) -> usize {
        if self.closed.get() {
            return 0;
        }
        self.cap.saturating_sub(self.used.get())
    }

    pub fn exhausted(&self) -> bool {
        self.remaining() == 0
    }

    /// Claim one invocation. `false` means the cap is reached — the caller
    /// must NOT invoke anything.
    ///
    /// For a night-scoped budget the claim is **durable**: one row-level debit
    /// against the night ledger, committed before this returns `true`. That is
    /// what makes the cap hold across passes, retries and daemon restarts
    /// rather than only within one pass. A ledger that refuses or fails closes
    /// the budget out instead of spending on an unenforceable cap.
    pub fn try_spend(&self) -> bool {
        if self.remaining() == 0 {
            return false;
        }
        if let Some(acc) = &self.accounting {
            match claim_night(acc.storage, &acc.night, acc.nightly_cap, 1) {
                Ok(1) => {}
                Ok(_) => {
                    tracing::info!(
                        night = %acc.night,
                        cap = acc.nightly_cap,
                        "tonight's dream invocation allowance is spent; queueing the remainder"
                    );
                    self.closed.set(true);
                    return false;
                }
                Err(error) => {
                    tracing::error!(
                        %error,
                        "dream night ledger debit failed; refusing to spend on an unenforceable cap"
                    );
                    note_accounting_failure(acc.storage);
                    self.closed.set(true);
                    return false;
                }
            }
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

    /// The measured cap/used/queued triple, copyable out of the pass.
    pub fn snapshot(&self) -> BudgetSnapshot {
        BudgetSnapshot {
            cap: self.cap,
            used: self.used.get(),
            queued: self.queued.get(),
        }
    }

    /// The night bucket this budget is spending against, if any.
    pub fn night(&self) -> Option<&str> {
        self.accounting.as_ref().map(|acc| acc.night.as_str())
    }

    /// Reserve one durable attempt row before an invocation. `Err` means the
    /// call must not happen: spending without a reservation is precisely the
    /// fail-open the reservation exists to prevent.
    fn reserve_attempt(&self, acc: &PassAccounting<'a>, model: Option<&str>) -> Result<String> {
        let seq = self.seq.get().saturating_add(1);
        self.seq.set(seq);
        let key = format!("dream:{}:{seq}", acc.nonce);
        acc.storage.with_connection(|conn| {
            usage_reservation::reserve(conn, &key, "dream_actor", None, model)
        })?;
        Ok(key)
    }
}

impl Drop for Budget<'_> {
    /// Nothing to return to the ledger — allowance is debited per invocation,
    /// so a pass never holds any. What a pass CAN leave behind is a
    /// reservation whose usage row never landed; say so rather than clearing
    /// it, since it stays `reserved` and counts as an unaccounted call.
    fn drop(&mut self) {
        if self.accounting.is_some() {
            if let Some(stale) = take_pending_reservation() {
                tracing::warn!(
                    attempt_key = %stale,
                    "dream pass ended with an unsettled usage reservation; it stays unaccounted"
                );
            }
        }
    }
}

/// Wraps any [`NightActor`] so that **every** invocation — first attempt,
/// model-chain fallback, verifier retry — is charged to one [`Budget`] and
/// carries a durable usage reservation. Once the cap is reached the wrapper
/// returns [`ActorAttempt::Failed`] WITHOUT invoking the inner actor, which
/// the producers treat as "no usable reply", so nothing is cached and the
/// candidate is retried on the next pass.
///
/// Order of operations, which is the whole point of the type:
/// 1. refuse outright if the cap is reached — nothing is written, nothing is
///    spent;
/// 2. write the reservation; if that fails, refuse WITHOUT invoking and
///    WITHOUT charging the budget (an unrecordable call must not happen);
/// 3. charge the budget, invoke, and hand the reservation key to the caller
///    that will settle it against the measured `narrative_usage` row.
pub(crate) struct BudgetedActor<'a> {
    inner: &'a dyn NightActor,
    budget: &'a Budget<'a>,
}

impl<'a> BudgetedActor<'a> {
    pub(crate) fn new(inner: &'a dyn NightActor, budget: &'a Budget<'a>) -> Self {
        Self { inner, budget }
    }
}

impl NightActor for BudgetedActor<'_> {
    fn invoke(&self, model: Option<&str>, prompt: &str) -> ActorAttempt {
        if self.budget.exhausted() {
            return ActorAttempt::Failed(format!(
                "dream budget exhausted ({} of {} invocations used)",
                self.budget.used(),
                self.budget.cap()
            ));
        }
        let reservation = match &self.budget.accounting {
            Some(acc) => match self.budget.reserve_attempt(acc, model) {
                Ok(key) => Some(key),
                Err(error) => {
                    tracing::error!(
                        %error,
                        "dream usage reservation failed; refusing to invoke the actor"
                    );
                    note_accounting_failure(acc.storage);
                    return ActorAttempt::Failed(
                        "usage reservation failed; invocation refused".to_string(),
                    );
                }
            },
            None => None,
        };
        if !self.budget.try_spend() {
            // Unreachable while the pass is single-threaded (the cap was
            // checked above), but if it ever is reached the reservation
            // describes a call that provably did not happen — say so rather
            // than leaving it as an unknown.
            if let (Some(key), Some(acc)) = (reservation.as_deref(), &self.budget.accounting) {
                let _ = acc.storage.with_connection(|conn| {
                    usage_reservation::abandon(conn, key, "budget exhausted before invoking")
                });
            }
            return ActorAttempt::Failed(format!(
                "dream budget exhausted ({} of {} invocations used)",
                self.budget.used(),
                self.budget.cap()
            ));
        }
        if let Some(key) = reservation {
            push_pending_reservation(key);
        }
        self.inner.invoke(model, prompt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::narrative::ParsedNarrative;

    fn naive(text: &str) -> chrono::NaiveDateTime {
        chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").unwrap()
    }

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
        assert_eq!(one.invocations, 10);
        let four = estimate_night(10, EffortTier::Balanced, 25, 2 * 3600);
        assert_eq!(four.passes, 4);
        // A cadence longer than the night still guarantees the floor pass.
        let long = estimate_night(10, EffortTier::Balanced, 25, 7 * 24 * 3600);
        assert_eq!(long.passes, 1);
    }

    #[test]
    fn the_estimate_never_promises_more_than_the_nightly_cap() {
        // Four passes × 10 reachable candidates each would be 40 invocations,
        // but the cap is per NIGHT and the enforcement (the night ledger)
        // stops at 25 — the estimate has to say the same thing.
        let four = estimate_night(10, EffortTier::Balanced, 25, 2 * 3600);
        assert_eq!(four.passes, 4);
        assert_eq!(
            four.invocations, 25,
            "the estimate must not model several full-cap passes per night"
        );
        assert_eq!(
            four.total_tokens(),
            25 * (EST_INPUT_TOKENS_PER_CALL + EST_OUTPUT_TOKENS_PER_CALL)
        );
        // The whole night's spend is bounded by the cap whatever the cadence.
        for interval in [600_u64, 1800, 3600, 6 * 3600] {
            let estimate = estimate_night(500, EffortTier::Max, 25, interval);
            assert!(
                estimate.invocations <= 25,
                "interval {interval}s estimated {} invocations against a cap of 25",
                estimate.invocations
            );
        }
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

    // ── the night ledger: one cap per night, across passes and restarts ──

    #[test]
    fn two_ticks_in_one_night_cannot_exceed_one_nightly_cap() {
        let storage = Storage::open_memory().unwrap();
        let night = "2026-08-11";
        let mut spent = 0;

        // First tick: spends 3 of the night's 5.
        {
            let budget = Budget::for_night_with_cap(&storage, night, 5);
            assert_eq!(budget.cap(), 5);
            for _ in 0..3 {
                assert!(budget.try_spend());
                spent += 1;
            }
        }

        // Second tick in the SAME night gets only what is left, never a
        // fresh cap.
        {
            let budget = Budget::for_night_with_cap(&storage, night, 5);
            assert_eq!(
                budget.cap(),
                2,
                "a second pass must draw down the same night's allowance"
            );
            for _ in 0..10 {
                if budget.try_spend() {
                    spent += 1;
                }
            }
            assert_eq!(budget.used(), 2);
        }

        // Third tick: the night is spent, whatever the pass wants.
        let budget = Budget::for_night_with_cap(&storage, night, 5);
        assert_eq!(budget.cap(), 0);
        assert!(!budget.try_spend());
        assert_eq!(
            spent, 5,
            "three ticks in one night must not exceed one nightly cap"
        );
        assert_eq!(night_claimed(&storage, night).unwrap(), 5);
        assert_eq!(night_remaining(&storage, night, 5).unwrap(), 0);
    }

    #[test]
    fn a_daemon_restart_mid_night_does_not_refill_the_allowance() {
        let storage = Storage::open_memory().unwrap();
        let night = "2026-08-11";

        // A pass spends 2 of 4 and is then killed — forgotten rather than
        // dropped, which is what a SIGKILLed daemon looks like to the
        // persisted ledger (no orderly shutdown ran).
        let budget = Budget::for_night_with_cap(&storage, night, 4);
        assert!(budget.try_spend());
        assert!(budget.try_spend());
        std::mem::forget(budget);

        assert_eq!(
            night_claimed(&storage, night).unwrap(),
            2,
            "the debits a killed pass already made stay debited"
        );
        let after_restart = Budget::for_night_with_cap(&storage, night, 4);
        assert_eq!(
            after_restart.cap(),
            2,
            "a restart reads the persisted balance, never a fresh cap"
        );
        assert!(after_restart.try_spend());
        assert!(after_restart.try_spend());
        assert!(
            !after_restart.try_spend(),
            "the night's total is still 4, not 4 per daemon lifetime"
        );
        assert_eq!(night_claimed(&storage, night).unwrap(), 4);
    }

    #[test]
    fn a_second_process_cannot_be_granted_the_same_last_invocation() {
        // Two live budgets over one database, as two `csr-engine` processes
        // against one night would be: each debit is atomic, so the total is
        // bounded even though both were told the same starting balance.
        let storage = Storage::open_memory().unwrap();
        let night = "2026-08-11";
        let first = Budget::for_night_with_cap(&storage, night, 3);
        let second = Budget::for_night_with_cap(&storage, night, 3);
        assert_eq!(first.cap(), 3);
        assert_eq!(second.cap(), 3, "both start from the same measured balance");

        let mut granted = 0;
        for _ in 0..5 {
            if first.try_spend() {
                granted += 1;
            }
            if second.try_spend() {
                granted += 1;
            }
        }
        assert_eq!(granted, 3, "the ledger, not the local counter, is the cap");
        assert_eq!(night_claimed(&storage, night).unwrap(), 3);
    }

    #[test]
    fn the_night_bucket_rolls_at_the_local_floor_boundary() {
        // Before the floor hour still belongs to the previous night's bucket;
        // at and after it, to the new one.
        assert_eq!(night_key(naive("2026-08-11 02:59:59"), 3), "2026-08-10");
        assert_eq!(night_key(naive("2026-08-11 03:00:00"), 3), "2026-08-11");
        assert_eq!(night_key(naive("2026-08-11 23:30:00"), 3), "2026-08-11");
        assert_eq!(night_key(naive("2026-08-12 01:00:00"), 3), "2026-08-11");
        assert_eq!(
            night_key(naive("2026-08-11 09:00:00"), 3),
            crate::daemon::dream_cadence::floor_boundary(naive("2026-08-11 09:00:00"), 3)
                .date()
                .format("%Y-%m-%d")
                .to_string(),
            "the ledger's night and the floor pass's night must be the same boundary"
        );

        let storage = Storage::open_memory().unwrap();
        {
            let budget = Budget::for_night_with_cap(&storage, "2026-08-10", 3);
            assert_eq!(budget.cap(), 3);
            for _ in 0..3 {
                assert!(budget.try_spend());
            }
            assert!(!budget.try_spend());
        }
        let next_night = Budget::for_night_with_cap(&storage, "2026-08-11", 3);
        assert_eq!(
            next_night.cap(),
            3,
            "the allowance is restored when the bucket rolls, not before"
        );
        assert!(next_night.try_spend());
        assert_eq!(
            night_claimed(&storage, "2026-08-10").unwrap(),
            0,
            "last night's ledger is not counted against tonight"
        );
        assert_eq!(night_claimed(&storage, "2026-08-11").unwrap(), 1);
    }

    #[test]
    fn the_night_bucket_rolls_once_across_spring_and_fall_dst_transitions() {
        fn local_at(
            wall_clock: &str,
            offset_seconds: i32,
        ) -> chrono::DateTime<chrono::FixedOffset> {
            let zone = chrono::FixedOffset::east_opt(offset_seconds).unwrap();
            chrono::TimeZone::from_local_datetime(&zone, &naive(wall_clock))
                .single()
                .unwrap()
        }

        // America/Los_Angeles springs from 01:59:59 PST to 03:00:00 PDT.
        // The missing 02:00 hour neither creates nor skips a local-night
        // bucket: the configured 03:00 floor rolls exactly once.
        assert_eq!(
            night_key_at(local_at("2026-03-08 01:59:59", -8 * 3600), 3),
            "2026-03-07"
        );
        assert_eq!(
            night_key_at(local_at("2026-03-08 03:00:00", -7 * 3600), 3),
            "2026-03-08"
        );

        // The fall-back hour occurs twice, once in PDT and once in PST. Both
        // representations are still before the same 03:00 local boundary;
        // changing UTC offset must not refill the nightly allowance.
        assert_eq!(
            night_key_at(local_at("2026-11-01 01:30:00", -7 * 3600), 3),
            "2026-10-31"
        );
        assert_eq!(
            night_key_at(local_at("2026-11-01 01:30:00", -8 * 3600), 3),
            "2026-10-31"
        );
        assert_eq!(
            night_key_at(local_at("2026-11-01 03:00:00", -8 * 3600), 3),
            "2026-11-01"
        );
    }

    #[test]
    fn an_unreadable_ledger_grants_nothing_rather_than_everything() {
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                conn.execute("DROP TABLE meta", [])?;
                Ok(())
            })
            .unwrap();
        let budget = Budget::for_night_with_cap(&storage, "2026-08-11", 25);
        assert_eq!(
            budget.cap(),
            0,
            "a cap that cannot be enforced must not become an unlimited one"
        );
        assert!(!budget.try_spend());
    }

    // ── accounting cannot fail open ──

    #[test]
    fn every_invocation_reserves_a_durable_row_before_it_happens() {
        let storage = Storage::open_memory().unwrap();
        let budget = Budget::for_night_with_cap(&storage, "2026-08-11", 3);
        let seen = Cell::new(0_usize);
        let inner = |_m: Option<&str>, _p: &str| {
            // The reservation must already be durable while the call is in
            // flight — that is the window the whole mechanism exists for.
            seen.set(
                seen.get()
                    + storage
                        .with_connection(usage_reservation::unaccounted_count)
                        .unwrap() as usize,
            );
            parsed("[]")
        };
        let actor = BudgetedActor::new(&inner, &budget);
        assert!(matches!(
            actor.invoke(Some("sonnet-5"), "prompt"),
            ActorAttempt::Parsed(_)
        ));
        assert_eq!(
            seen.get(),
            1,
            "the invocation ran without a durable reservation behind it"
        );
        let key = take_pending_reservation().expect("the key is handed to the usage writer");
        let row = storage
            .with_connection(|conn| usage_reservation::load(conn, &key))
            .unwrap()
            .expect("row");
        assert_eq!(row.state, "reserved");
        assert_eq!(row.model.as_deref(), Some("sonnet-5"));
    }

    #[test]
    fn a_reservation_that_cannot_be_written_refuses_the_invocation() {
        let storage = Storage::open_memory().unwrap();
        let budget = Budget::for_night_with_cap(&storage, "2026-08-11", 5);
        storage
            .with_connection(|conn| {
                conn.execute("DROP TABLE narrative_reservations", [])?;
                Ok(())
            })
            .unwrap();
        let calls = Cell::new(0_usize);
        let inner = |_m: Option<&str>, _p: &str| {
            calls.set(calls.get() + 1);
            parsed("[]")
        };
        let actor = BudgetedActor::new(&inner, &budget);
        let attempt = actor.invoke(Some("sonnet-5"), "prompt");
        match attempt {
            ActorAttempt::Failed(msg) => assert!(
                msg.contains("reservation"),
                "the refusal must name its cause: {msg}"
            ),
            _ => panic!("an unrecordable call must not happen"),
        }
        assert_eq!(calls.get(), 0, "the actor was invoked without accounting");
        assert_eq!(budget.used(), 0, "a refused call must not be charged");
        assert_eq!(
            accounting_failure_count(&storage),
            Some(1),
            "the failure must be counted for status, never swallowed"
        );
    }

    #[test]
    fn an_unbudgeted_actor_reserves_nothing_and_hands_over_no_key() {
        // `Budget::new` is the no-ledger, no-accounting shape (tests, manual
        // one-shots): it must not fabricate reservations.
        let storage = Storage::open_memory().unwrap();
        let budget = Budget::new(2);
        let inner = |_m: Option<&str>, _p: &str| parsed("[]");
        let actor = BudgetedActor::new(&inner, &budget);
        assert!(matches!(
            actor.invoke(None, "prompt"),
            ActorAttempt::Parsed(_)
        ));
        assert_eq!(take_pending_reservation(), None);
        assert_eq!(
            storage
                .with_connection(usage_reservation::unaccounted_count)
                .unwrap(),
            0
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
