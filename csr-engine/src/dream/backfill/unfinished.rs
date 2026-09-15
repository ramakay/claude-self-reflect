//! Dream backfill — unfinished scan (`.plans/dream-backfill-design.md` §3
//! "Stage 1", with the D1/D4/D10 round-2 deltas from §8 folded in, since
//! that section overrides §3-6 wherever they conflict).
//!
//! Scans `episode_index` (materialized by
//! [`crate::storage::dream_backfill`]) for episodes whose work looks
//! unfinished, and classifies each one deterministically into exactly one
//! disposition:
//!
//! 1. **Chain-picked** (D4 — a hard gate applied BEFORE any scoring): the
//!    seed's own session chain (`storage::dream_backfill::prev_chain_pairs`)
//!    reaches a strictly-later episode, directly or transitively. No
//!    similarity bonus is awarded for this any more (D4 deleted it) — it is
//!    a binary gate.
//! 2. **Obsolescence-converted** (D3): the seed is provably dead code — every
//!    touched file resolved absent at HEAD and the freshest touch is more
//!    than [`OBSOLESCENCE_DAYS_THRESHOLD`] days old. Counted, never dreamed;
//!    checked before scoring runs, since a dead-code seed can never satisfy
//!    the eventual "≥1 live file" claim requirement regardless of its score.
//! 3. **Picked-up-by-score**: the best pickup score among strictly-later
//!    same-project episodes meets or exceeds the fitted τ (see
//!    [`fit_tau`]).
//! 4. **Never-picked-up**: everything else. A [`NegativeReceipt`] is always
//!    persisted for these (`backfill_unfinished_receipts`, full-corpus
//!    idempotent refresh — see [`reconcile_negative_receipts`]), and a
//!    subset additionally qualifies for Queue U (see [`queue_eligible`] /
//!    [`queue_u_priority`]).
//!
//! Zero LLM calls anywhere in this module.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::daemon::dream_cadence::dreaming_disabled;
use crate::hooks::intent::cosine_sim;
use crate::storage::dream_backfill::{episode_vectors, prev_chain_pairs};
use crate::temporal::parse_timestamp;

/// D3: absent at HEAD *and* the freshest touch predates this many days ⇒
/// `resolved_by_obsolescence`. The design's own number (§8 D3).
const OBSOLESCENCE_DAYS_THRESHOLD: i64 = 60;

/// D10: τ_pickup is fit to hold the false-positive rate at or below this
/// fraction of the matched negative sample — the task's literal "FPR <= 2%"
/// (the design's §8 D10 text allows a 1-2% band; 2% is the pinned value).
const TAU_TARGET_FPR: f64 = 0.02;

/// Conservative fallback τ when there are too few chain-completion positives
/// or matched negatives to fit anything meaningful (e.g. a brand-new or
/// tiny corpus). High and precision-biased on purpose — the kill criterion
/// (design §0) demands precision over coverage, so an unfit τ should err
/// toward calling things "never picked up" less often, not more.
const DEFAULT_TAU_FALLBACK: f64 = 0.75;

/// P6 (F10): below this many matched negatives, the empirical FPR the fit
/// targets is not a meaningful statistic — a single negative sample can
/// swing the "10th/2nd-percentile" style fit to a degenerate τ that is
/// technically consistent with that one point but generalizes to nothing.
/// The design's own fallback language ("too few... matched negatives to fit
/// anything meaningful", §8 D10) already anticipates exactly this guard;
/// this pins the threshold. Below it, [`fit_tau`] falls back to
/// [`DEFAULT_TAU_FALLBACK`] the same way it already does for zero negatives.
const TAU_MIN_NEGATIVES: usize = 3;

/// Judgment call (undocumented by the design): a soft cap on how many open
/// todos contribute to the Queue U "todos" term before it saturates at 1.0.
const QUEUE_U_TODO_CAP: f64 = 5.0;

/// D1: "age > 90d is a mild bonus (forgetting proxy)" — the design's own
/// number.
const QUEUE_U_AGE_BONUS_THRESHOLD_DAYS: i64 = 90;

/// Backfill-mode Queue U weights (design §3 Stage 3, with D1's edit: the
/// original 45-day recency-decay term is DELETED and its 0.25 weight slot is
/// reused, unchanged, by the age>90d mild-bonus term — D1 gives the other
/// three weights explicitly (0.35/0.20/0.20) but does not restate the third
/// slot's number, so this keeps the original allocation rather than
/// inventing a new one). Deliberately a separate const block from any future
/// nightly-mode weights, per D1's explicit instruction.
const QUEUE_U_WEIGHT_SEVERITY: f64 = 0.35;
const QUEUE_U_WEIGHT_TODOS: f64 = 0.20;
const QUEUE_U_WEIGHT_AGE_BONUS: f64 = 0.25;
const QUEUE_U_WEIGHT_ALIVENESS: f64 = 0.20;

/// One materialized `episode_index` row, narrowed to exactly the columns
/// this scan needs — following `storage::dream_backfill`'s own convention
/// of querying narrower slices directly rather than sharing one wide loader.
#[derive(Debug, Clone)]
struct EpisodeRow {
    episode_id: String,
    project: String,
    ts: String,
    outcome: String,
    todo_count: i64,
    blockers: Option<String>,
    files: HashSet<String>,
    present_at_head: Option<bool>,
    days_since_last_touch: Option<i64>,
}

/// A [`fit_tau`] outcome, self-contained enough to serialize into every
/// negative receipt without a join (D10).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TauFit {
    pub tau: f64,
    pub positive_count: usize,
    pub negative_count: usize,
    pub confusion: ConfusionMatrix,
}

/// TP/FN/FP/TN of the fitted τ against the very positives/negatives used to
/// fit it — a self-consistency check, not held-out validation (there is no
/// held-out set yet; D10 notes τ gets refit after the first human-verdict
/// batch, which will be the real validation signal).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfusionMatrix {
    pub true_positive: usize,
    pub false_negative: usize,
    pub false_positive: usize,
    pub true_negative: usize,
}

/// The design's negative receipt shape (§3 Stage 1): `{corpus_scanned,
/// max_score, argmax_episode}`, plus D10's addition of the confusion matrix.
#[derive(Debug, Clone, Serialize)]
pub struct NegativeReceipt {
    pub seed_episode_id: String,
    pub project: String,
    pub corpus_scanned: usize,
    pub max_score: f64,
    pub argmax_episode: Option<String>,
    pub confusion_matrix: ConfusionMatrix,
}

/// One seed's final classification. Every seed matching [`is_seed`] gets
/// exactly one of these.
#[derive(Debug, Clone)]
pub enum SeedDisposition {
    PickedUpByChain {
        episode_id: String,
    },
    PickedUpByScore {
        episode_id: String,
        score: f64,
        argmax_episode: String,
    },
    ResolvedByObsolescence {
        episode_id: String,
        days_since_last_touch: i64,
    },
    NeverPickedUp {
        receipt: NegativeReceipt,
        queue_eligible: bool,
    },
}

impl SeedDisposition {
    pub fn episode_id(&self) -> &str {
        match self {
            SeedDisposition::PickedUpByChain { episode_id } => episode_id,
            SeedDisposition::PickedUpByScore { episode_id, .. } => episode_id,
            SeedDisposition::ResolvedByObsolescence { episode_id, .. } => episode_id,
            SeedDisposition::NeverPickedUp { receipt, .. } => &receipt.seed_episode_id,
        }
    }
}

/// A never-picked-up seed that additionally cleared [`queue_eligible`],
/// ranked by [`queue_u_priority`].
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedSeed {
    pub episode_id: String,
    pub project: String,
    pub priority: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ScanStats {
    pub seeds_considered: usize,
    pub picked_up_by_chain: usize,
    pub picked_up_by_score: usize,
    pub resolved_by_obsolescence: usize,
    pub never_picked_up: usize,
    pub excluded_from_queue: usize,
}

/// Full result of one [`scan_unfinished`] pass.
#[derive(Debug, Clone)]
pub struct UnfinishedScanReport {
    /// `true` when `CSR_NO_DREAMING` was set — every other field is empty
    /// and no rows were written.
    pub disabled: bool,
    pub tau_fit: TauFit,
    pub dispositions: Vec<SeedDisposition>,
    /// Descending by priority (ties broken by `episode_id` for
    /// determinism).
    pub queue: Vec<QueuedSeed>,
    pub stats: ScanStats,
}

impl UnfinishedScanReport {
    fn disabled_report() -> Self {
        Self {
            disabled: true,
            tau_fit: TauFit {
                tau: DEFAULT_TAU_FALLBACK,
                positive_count: 0,
                negative_count: 0,
                confusion: ConfusionMatrix::default(),
            },
            dispositions: Vec::new(),
            queue: Vec::new(),
            stats: ScanStats::default(),
        }
    }
}

/// Entry point. Respects `CSR_NO_DREAMING` (house rule) — returns a
/// `disabled` report and writes nothing when set, same idiom as
/// `daemon::dream_cadence::decide`'s kill-switch check.
///
/// Read-only against `episode_index`; the only table this writes to is
/// `backfill_unfinished_receipts` (design's crash-safety contract: "writes
/// only to new tables"). Production wrapper around [`scan_unfinished_at`],
/// supplying the real wall clock.
pub fn scan_unfinished(conn: &Connection) -> Result<UnfinishedScanReport> {
    scan_unfinished_at(conn, Utc::now())
}

/// P5: core scan with `now` injected — the D1 age bonus and any future
/// recency-style term need a caller-supplied reference time to be
/// reproducible (a fixed-date golden fixture, or a test asserting the
/// age>90d bonus without depending on the wall clock the test happens to run
/// on). [`scan_unfinished`] is the production entry point.
pub fn scan_unfinished_at(conn: &Connection, now: DateTime<Utc>) -> Result<UnfinishedScanReport> {
    if dreaming_disabled() {
        return Ok(UnfinishedScanReport::disabled_report());
    }

    let rows = load_episode_rows(conn)?;
    let vectors: HashMap<String, Vec<f32>> = episode_vectors(conn)?.into_iter().collect();

    // `BTreeMap` (not `HashMap`) so project iteration order — and therefore
    // the order dispositions are appended — is deterministic across runs,
    // which unit tests rely on.
    let mut by_project: BTreeMap<String, Vec<EpisodeRow>> = BTreeMap::new();
    for row in rows {
        by_project.entry(row.project.clone()).or_default().push(row);
    }

    let tau_fit = fit_tau_over_corpus(conn, &by_project, &vectors)?;

    let mut dispositions = Vec::new();
    let mut queue = Vec::new();
    let mut stats = ScanStats::default();
    let mut fresh_receipts: Vec<NegativeReceipt> = Vec::new();

    for (project, project_rows) in &by_project {
        let chain_roots: HashSet<String> = prev_chain_pairs(conn, project)?
            .into_iter()
            .map(|(root, _)| root)
            .collect();

        for seed in project_rows.iter().filter(|r| is_seed(r)) {
            stats.seeds_considered += 1;

            if chain_roots.contains(&seed.episode_id) {
                stats.picked_up_by_chain += 1;
                dispositions.push(SeedDisposition::PickedUpByChain {
                    episode_id: seed.episode_id.clone(),
                });
                continue;
            }

            if is_obsolete(seed) {
                stats.resolved_by_obsolescence += 1;
                dispositions.push(SeedDisposition::ResolvedByObsolescence {
                    episode_id: seed.episode_id.clone(),
                    days_since_last_touch: seed
                        .days_since_last_touch
                        .expect("is_obsolete only true when this is Some"),
                });
                continue;
            }

            let (scanned, max_score, argmax) = best_pickup(seed, project_rows, &vectors);
            if max_score >= tau_fit.tau {
                stats.picked_up_by_score += 1;
                dispositions.push(SeedDisposition::PickedUpByScore {
                    episode_id: seed.episode_id.clone(),
                    score: max_score,
                    argmax_episode: argmax.expect("score >= tau implies at least one candidate"),
                });
                continue;
            }

            let receipt = NegativeReceipt {
                seed_episode_id: seed.episode_id.clone(),
                project: project.clone(),
                corpus_scanned: scanned,
                max_score,
                argmax_episode: argmax,
                confusion_matrix: tau_fit.confusion,
            };
            let eligible = queue_eligible(seed);
            stats.never_picked_up += 1;
            if eligible {
                queue.push(QueuedSeed {
                    episode_id: seed.episode_id.clone(),
                    project: project.clone(),
                    priority: queue_u_priority(seed, now),
                });
            } else {
                stats.excluded_from_queue += 1;
            }
            fresh_receipts.push(receipt.clone());
            dispositions.push(SeedDisposition::NeverPickedUp {
                receipt,
                queue_eligible: eligible,
            });
        }
    }

    queue.sort_by(|a, b| {
        b.priority
            .partial_cmp(&a.priority)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.episode_id.cmp(&b.episode_id))
    });

    reconcile_negative_receipts(conn, &fresh_receipts)?;

    Ok(UnfinishedScanReport {
        disabled: false,
        tau_fit,
        dispositions,
        queue,
        stats,
    })
}

/// Design §3 Stage 1 seed condition, unchanged by any round-2 delta:
/// `outcome ∈ (partial, failed) OR todo_count > 0`.
fn is_seed(row: &EpisodeRow) -> bool {
    row.outcome == "partial" || row.outcome == "failed" || row.todo_count > 0
}

/// D3 obsolescence conversion: absent at HEAD *and* stale past the
/// threshold. `present_at_head == None` (unresolvable — no local repo) is
/// deliberately excluded here, never collapsed into "absent" — same
/// "absence of evidence is not deadness" contract `storage::dream_backfill`
/// documents for the columns themselves.
fn is_obsolete(row: &EpisodeRow) -> bool {
    row.present_at_head == Some(false)
        && row
            .days_since_last_touch
            .is_some_and(|d| d > OBSOLESCENCE_DAYS_THRESHOLD)
}

/// Design §3 Stage 1's claim requirement, carried forward unamended by §8:
/// "Claim requires ≥1 open todo or non-empty blockers AND ≥1 live file."
/// `present_at_head == None` (unresolvable) fails this on purpose — the
/// claim cannot be substantiated without a resolved "still alive" signal,
/// so an unresolvable seed is recorded (the negative receipt still gets
/// written) but held back from Queue U rather than asserted anyway.
fn queue_eligible(row: &EpisodeRow) -> bool {
    let has_open_signal = row.todo_count > 0 || row.blockers.is_some();
    has_open_signal && row.present_at_head == Some(true)
}

/// Plain cosine over episode vectors — 0.0 if either side has no vector.
/// `0.6·cos(vec) + 0.4·jaccard(files)`, clamped to `[0,1]` (defensive; the
/// chain bonus that could push the original design's formula above 1 is
/// deleted per D4, so the two terms alone can never exceed 1 in practice).
fn pickup_score(
    vec_a: Option<&[f32]>,
    files_a: &HashSet<String>,
    vec_b: Option<&[f32]>,
    files_b: &HashSet<String>,
) -> f64 {
    let cos = match (vec_a, vec_b) {
        (Some(a), Some(b)) => cosine_sim(a, b) as f64,
        _ => 0.0,
    };
    let jac = jaccard(files_a, files_b);
    (0.6 * cos + 0.4 * jac).clamp(0.0, 1.0)
}

/// Jaccard over two file sets. Two seeds that both touched zero files share
/// no positive evidence, so this is 0.0, not 1.0 — an empty intersection
/// over an empty union is not "identical", it is "no evidence".
fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

fn parse_ts(row_ts: &str) -> Option<DateTime<Utc>> {
    parse_timestamp(row_ts)
}

/// Every row in `project_rows` strictly later (by parsed `ts`) than `seed`,
/// excluding `seed` itself. Rows on either side whose `ts` fails to parse
/// are excluded — no ordering can be asserted, so they cannot contribute
/// evidence either way.
///
/// P10 (F11): sorted here by PARSED timestamp (then `episode_id` as a final
/// tie-break) rather than trusting the caller's `ORDER BY ts ASC` — that SQL
/// ordering is a lexical/string sort, which agrees with chronological order
/// only when every row's `ts` shares one exact format. A corpus mixing
/// `Z`-suffixed and `+00:00`-suffixed (or otherwise differently formatted)
/// timestamps string-sorts differently than it parses, which would make
/// [`best_pickup`]'s tie-break argmax (and therefore the negative receipt's
/// `argmax_episode`) depend on incidental formatting rather than actual
/// chronology.
fn later_candidates<'a>(seed: &EpisodeRow, project_rows: &'a [EpisodeRow]) -> Vec<&'a EpisodeRow> {
    let Some(seed_ts) = parse_ts(&seed.ts) else {
        return Vec::new();
    };
    let mut candidates: Vec<(DateTime<Utc>, &'a EpisodeRow)> = project_rows
        .iter()
        .filter(|r| r.episode_id != seed.episode_id)
        .filter_map(|r| parse_ts(&r.ts).filter(|t| *t > seed_ts).map(|t| (t, r)))
        .collect();
    candidates.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.episode_id.cmp(&b.1.episode_id))
    });
    candidates.into_iter().map(|(_, r)| r).collect()
}

/// Best pickup score for `seed` over its strictly-later same-project
/// candidates. Returns `(corpus_scanned, max_score, argmax_episode)` —
/// `corpus_scanned` is exactly the candidate count considered, matching the
/// design's negative-receipt field of the same name. Ties keep the first
/// candidate encountered (strict `>` update), which combined with the
/// caller's sorted row order makes this deterministic.
fn best_pickup(
    seed: &EpisodeRow,
    project_rows: &[EpisodeRow],
    vectors: &HashMap<String, Vec<f32>>,
) -> (usize, f64, Option<String>) {
    let candidates = later_candidates(seed, project_rows);
    let seed_vec = vectors.get(&seed.episode_id).map(|v| v.as_slice());

    let mut best: Option<(f64, String)> = None;
    for cand in &candidates {
        let cand_vec = vectors.get(&cand.episode_id).map(|v| v.as_slice());
        let score = pickup_score(seed_vec, &seed.files, cand_vec, &cand.files);
        if best.as_ref().is_none_or(|(b, _)| score > *b) {
            best = Some((score, cand.episode_id.clone()));
        }
    }

    match best {
        Some((score, id)) => (candidates.len(), score, Some(id)),
        None => (candidates.len(), 0.0, None),
    }
}

/// D10 τ-fitting, pooled across every project in the current corpus
/// (undocumented by the design whether the fit is per-project or global;
/// pooling is the judgment call here — a lone project rarely has enough
/// chain-completions to fit a percentile-style threshold on its own, and
/// nothing in the design scopes τ to a project).
fn fit_tau_over_corpus(
    conn: &Connection,
    by_project: &BTreeMap<String, Vec<EpisodeRow>>,
    vectors: &HashMap<String, Vec<f32>>,
) -> Result<TauFit> {
    let mut positives = Vec::new();
    let mut negatives = Vec::new();

    for (project, rows) in by_project {
        let chain_pairs = prev_chain_pairs(conn, project)?;
        let mut descendants_of: HashMap<&str, HashSet<&str>> = HashMap::new();
        for (root, desc) in &chain_pairs {
            descendants_of
                .entry(root.as_str())
                .or_default()
                .insert(desc.as_str());
        }

        let row_by_id: HashMap<&str, &EpisodeRow> =
            rows.iter().map(|r| (r.episode_id.as_str(), r)).collect();

        for (root, desc) in &chain_pairs {
            // D10 positive definition: chain-completions with outcome ==
            // "completed". (The design's parenthetical FTS-todo-term
            // secondary signal is not implemented here — no todo-text FTS
            // index exists yet for this pipeline; out of this stage's
            // scope.)
            let Some(desc_row) = row_by_id.get(desc.as_str()) else {
                continue;
            };
            if desc_row.outcome != "completed" {
                continue;
            }
            let Some(root_row) = row_by_id.get(root.as_str()) else {
                continue;
            };

            let root_vec = vectors.get(root).map(|v| v.as_slice());
            let desc_vec = vectors.get(desc).map(|v| v.as_slice());
            let positive_score = pickup_score(root_vec, &root_row.files, desc_vec, &desc_row.files);
            positives.push(positive_score);

            // Matched negative: the same-project, strictly-later episode
            // (excluding any of root's own chain descendants, which are
            // known continuations, not negatives) with the LOWEST score
            // against root — a deliberately low-similarity match, per D10's
            // "matched same-project low-sim sample".
            let excluded = descendants_of.get(root.as_str());
            let Some(root_ts) = parse_ts(&root_row.ts) else {
                continue;
            };
            let mut lowest: Option<f64> = None;
            for cand in rows.iter() {
                if cand.episode_id == *root {
                    continue;
                }
                if excluded.is_some_and(|d| d.contains(cand.episode_id.as_str())) {
                    continue;
                }
                let Some(cand_ts) = parse_ts(&cand.ts) else {
                    continue;
                };
                if cand_ts <= root_ts {
                    continue;
                }
                let cand_vec = vectors.get(&cand.episode_id).map(|v| v.as_slice());
                let score = pickup_score(root_vec, &root_row.files, cand_vec, &cand.files);
                lowest = Some(lowest.map_or(score, |m: f64| m.min(score)));
            }
            if let Some(negative_score) = lowest {
                negatives.push(negative_score);
            }
        }
    }

    Ok(fit_tau(&positives, &negatives))
}

/// τ at the pinned false-positive rate: the smallest τ such that at most
/// `floor(TAU_TARGET_FPR * negatives.len())` of `negatives` score `>= τ`
/// (score `>= τ` is this module's "picked up" predicate throughout, so the
/// fit targets exactly the predicate that gets applied at scan time).
/// Falls back to [`DEFAULT_TAU_FALLBACK`] when there are fewer than
/// [`TAU_MIN_NEGATIVES`] matched negatives to fit against (P6/F10) —
/// including the zero-negatives case this already covered.
fn fit_tau(positives: &[f64], negatives: &[f64]) -> TauFit {
    if negatives.len() < TAU_MIN_NEGATIVES {
        return TauFit {
            tau: DEFAULT_TAU_FALLBACK,
            positive_count: positives.len(),
            negative_count: negatives.len(),
            confusion: confusion_matrix(positives, negatives, DEFAULT_TAU_FALLBACK),
        };
    }

    let mut sorted_desc = negatives.to_vec();
    sorted_desc.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted_desc.len();
    let allowed_fp = ((TAU_TARGET_FPR * n as f64).floor() as usize).min(n);

    let tau = if allowed_fp == 0 {
        // Zero negatives may exceed τ: set τ just above the single highest
        // negative score so even that one scores strictly below it.
        sorted_desc[0] + 1e-9
    } else {
        // The `allowed_fp`-th highest negative (0-indexed) becomes τ
        // itself: with `>= τ` as the picked-up predicate, exactly the
        // `allowed_fp` negatives ranked above it satisfy the predicate
        // (assuming no exact ties), landing the empirical FPR at
        // `allowed_fp / n <= TAU_TARGET_FPR`.
        sorted_desc[(allowed_fp - 1).min(n - 1)]
    };

    TauFit {
        tau,
        positive_count: positives.len(),
        negative_count: negatives.len(),
        confusion: confusion_matrix(positives, negatives, tau),
    }
}

fn confusion_matrix(positives: &[f64], negatives: &[f64], tau: f64) -> ConfusionMatrix {
    let true_positive = positives.iter().filter(|&&s| s >= tau).count();
    let false_positive = negatives.iter().filter(|&&s| s >= tau).count();
    ConfusionMatrix {
        true_positive,
        false_negative: positives.len() - true_positive,
        false_positive,
        true_negative: negatives.len() - false_positive,
    }
}

/// Severity term of Queue U priority. Not specified by the design (the
/// weight 0.35 is given, its input function is not) — a graded read of
/// `outcome`: an outright failure is the strongest unfinished-work signal,
/// a partial result weaker, and a nominally "completed" episode that is
/// still a seed only because it left open todos weaker still.
fn severity(outcome: &str) -> f64 {
    match outcome {
        "failed" => 1.0,
        "partial" => 0.6,
        _ => 0.3,
    }
}

/// Todos term: open-todo count saturating at [`QUEUE_U_TODO_CAP`].
fn todos_term(todo_count: i64) -> f64 {
    (todo_count as f64 / QUEUE_U_TODO_CAP).min(1.0)
}

/// D1 age-bonus term: a binary "mild bonus" (not a graded curve — the
/// design's own wording is "mild bonus", replacing the old continuous
/// recency-decay term) for a seed episode older than
/// [`QUEUE_U_AGE_BONUS_THRESHOLD_DAYS`]. An unparseable `ts` contributes 0,
/// same as "not yet old enough".
fn age_bonus_term(ts: &str, now: DateTime<Utc>) -> f64 {
    match parse_ts(ts) {
        Some(t) if (now - t).num_days() > QUEUE_U_AGE_BONUS_THRESHOLD_DAYS => 1.0,
        _ => 0.0,
    }
}

/// Queue U priority (design §3 Stage 3, D1 override): `0.35·severity +
/// 0.20·todos + 0.25·age_bonus + 0.20·aliveness`. `present_at_head == None`
/// (unresolvable aliveness) EXCLUDES the aliveness term and renormalizes
/// over the remaining weight, rather than folding it in as a zero — a
/// missing signal must never quietly deflate the score the way a confirmed
/// "not alive" reading legitimately does.
///
/// C (conformance review): this function's only call site
/// ([`scan_unfinished_at`]'s `if eligible { ... queue_u_priority(seed, now)
/// ... }`) is gated by [`queue_eligible`], which already requires
/// `present_at_head == Some(true)`. So a seed reaching here can never have
/// `present_at_head` be `None` or `Some(false)` — the "renormalize on
/// unresolvable aliveness" branch below is exercised directly by this
/// function's own unit tests (which call it standalone), but is structurally
/// unreachable via the real `scan_unfinished_at` call path. No behavior
/// change follows from this — the renormalization is correct defensive code
/// regardless — this is a documentation-only note per that review.
fn queue_u_priority(row: &EpisodeRow, now: DateTime<Utc>) -> f64 {
    let mut weighted_sum = QUEUE_U_WEIGHT_SEVERITY * severity(&row.outcome)
        + QUEUE_U_WEIGHT_TODOS * todos_term(row.todo_count)
        + QUEUE_U_WEIGHT_AGE_BONUS * age_bonus_term(&row.ts, now);
    let mut total_weight =
        QUEUE_U_WEIGHT_SEVERITY + QUEUE_U_WEIGHT_TODOS + QUEUE_U_WEIGHT_AGE_BONUS;

    if let Some(alive) = row.present_at_head {
        weighted_sum += QUEUE_U_WEIGHT_ALIVENESS * if alive { 1.0 } else { 0.0 };
        total_weight += QUEUE_U_WEIGHT_ALIVENESS;
    }

    weighted_sum / total_weight
}

fn load_episode_rows(conn: &Connection) -> Result<Vec<EpisodeRow>> {
    let mut stmt = conn.prepare(
        "SELECT episode_id, project, ts, outcome, todo_count, blockers, files_json,
                present_at_head, days_since_last_touch
         FROM episode_index
         ORDER BY ts ASC, episode_id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        let files_json: String = row.get(6)?;
        let present_at_head: Option<i64> = row.get(7)?;
        Ok(EpisodeRow {
            episode_id: row.get(0)?,
            project: row.get(1)?,
            ts: row.get(2)?,
            outcome: row.get(3)?,
            todo_count: row.get(4)?,
            blockers: row.get(5)?,
            files: serde_json::from_str::<Vec<String>>(&files_json)
                .unwrap_or_default()
                .into_iter()
                .collect(),
            present_at_head: present_at_head.map(|v| v != 0),
            days_since_last_touch: row.get(8)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// Writes `fresh` (`INSERT OR REPLACE`, keyed by `seed_episode_id`) then
/// deletes any pre-existing `backfill_unfinished_receipts` row whose id is
/// NOT in `fresh` — this scan is a full-corpus pass every time, so anything
/// not reconfirmed as "never picked up" this run (picked up since, obsolete
/// since, or no longer even a seed) must not linger as a stale claim.
fn reconcile_negative_receipts(conn: &Connection, fresh: &[NegativeReceipt]) -> Result<()> {
    let fresh_ids: HashSet<&str> = fresh.iter().map(|r| r.seed_episode_id.as_str()).collect();

    {
        let mut upsert = conn.prepare(
            "INSERT OR REPLACE INTO backfill_unfinished_receipts
                (seed_episode_id, project, corpus_scanned, max_score, argmax_episode,
                 confusion_matrix_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))",
        )?;
        for receipt in fresh {
            let confusion_json = serde_json::to_string(&receipt.confusion_matrix)?;
            upsert.execute(params![
                receipt.seed_episode_id,
                receipt.project,
                receipt.corpus_scanned as i64,
                receipt.max_score,
                receipt.argmax_episode,
                confusion_json,
            ])?;
        }
    }

    let existing: Vec<String> = {
        let mut stmt = conn.prepare("SELECT seed_episode_id FROM backfill_unfinished_receipts")?;
        let collected = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        collected
    };

    let mut delete =
        conn.prepare("DELETE FROM backfill_unfinished_receipts WHERE seed_episode_id = ?1")?;
    for id in existing {
        if !fresh_ids.contains(id.as_str()) {
            delete.execute(params![id])?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::storage::migrations::run(&conn).unwrap();
        conn
    }

    #[allow(clippy::too_many_arguments)] // pre-existing test helper, wide on purpose
    fn insert_episode(
        conn: &Connection,
        episode_id: &str,
        session_id: &str,
        project: &str,
        ts: &str,
        outcome: &str,
        todo_count: i64,
        blockers: Option<&str>,
        files: &[&str],
        prev_episode_id: Option<&str>,
        present_at_head: Option<bool>,
        days_since_last_touch: Option<i64>,
    ) {
        // `episode_index.episode_id` IS `reflections.id` in production
        // (`storage::dream_backfill::materialize_episode_index` upserts
        // keyed by the source `reflections` row's own id), and
        // `reflection_embeddings.reflection_id` carries a real FK back to
        // `reflections(id)` — a stub row here keeps that invariant true for
        // tests that also call [`insert_vector`].
        conn.execute(
            "INSERT OR IGNORE INTO reflections (id, content, tags, timestamp)
             VALUES (?1, '{}', '[]', ?2)",
            params![episode_id, ts],
        )
        .unwrap();

        let files_json = serde_json::to_string(files).unwrap();
        conn.execute(
            "INSERT INTO episode_index (
                episode_id, session_id, project, ts, outcome, request, completed,
                next_steps, blockers, todo_count, files_json, anchors_json,
                prev_episode_id, present_at_head, days_since_last_touch
            ) VALUES (?1, ?2, ?3, ?4, ?5, '', '', NULL, ?6, ?7, ?8, '[]', ?9, ?10, ?11)",
            params![
                episode_id,
                session_id,
                project,
                ts,
                outcome,
                blockers,
                todo_count,
                files_json,
                prev_episode_id,
                present_at_head.map(|b| b as i64),
                days_since_last_touch,
            ],
        )
        .unwrap();
    }

    fn insert_vector(conn: &Connection, episode_id: &str, v: &[f32]) {
        let bytes: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
        // Requires `insert_episode` (or an equivalent `reflections` stub) to
        // have already run for `episode_id` — `reflection_embeddings` has a
        // real FK back to `reflections(id)`.
        conn.execute(
            "INSERT INTO reflection_embeddings (reflection_id, embedding) VALUES (?1, ?2)",
            params![episode_id, bytes],
        )
        .unwrap();
    }

    #[test]
    fn is_seed_matches_outcome_or_open_todos() {
        let mut row = EpisodeRow {
            episode_id: "e".into(),
            project: "p".into(),
            ts: "2026-08-20T00:00:00Z".into(),
            outcome: "completed".into(),
            todo_count: 0,
            blockers: None,
            files: HashSet::new(),
            present_at_head: None,
            days_since_last_touch: None,
        };
        assert!(!is_seed(&row));
        row.todo_count = 1;
        assert!(is_seed(&row));
        row.todo_count = 0;
        row.outcome = "partial".into();
        assert!(is_seed(&row));
        row.outcome = "failed".into();
        assert!(is_seed(&row));
    }

    #[test]
    fn chain_gate_beats_score_even_when_similarity_is_zero() {
        // Guards against `CSR_NO_DREAMING`-toggling tests elsewhere in the
        // crate (e.g. `dream::backfill::compose`'s disabled-run tests)
        // running concurrently and racing this test's `scan_unfinished`
        // call via the shared process-global env var — see
        // `env_test_guard`'s own doc.
        let _g = crate::daemon::dream_cadence::env_test_guard();
        let conn = open();
        insert_episode(
            &conn,
            "ep-1",
            "sess-1",
            "proj",
            "2026-08-01T00:00:00Z",
            "partial",
            1,
            None,
            &["a.rs"],
            None,
            Some(true),
            Some(1),
        );
        // sess-2 resumes sess-1 and is itself fully resolved (not a seed).
        insert_episode(
            &conn,
            "ep-2",
            "sess-2",
            "proj",
            "2026-08-02T00:00:00Z",
            "completed",
            0,
            None,
            &["totally-unrelated.rs"],
            Some("sess-1"),
            Some(true),
            Some(1),
        );
        insert_vector(&conn, "ep-1", &[1.0, 0.0, 0.0]);
        insert_vector(&conn, "ep-2", &[0.0, 1.0, 0.0]);

        let report = scan_unfinished(&conn).unwrap();
        assert_eq!(report.stats.seeds_considered, 1);
        assert_eq!(report.stats.picked_up_by_chain, 1);
        assert_eq!(report.stats.never_picked_up, 0);
        assert!(matches!(
            report.dispositions[0],
            SeedDisposition::PickedUpByChain { .. }
        ));
    }

    #[test]
    fn never_picked_up_seed_gets_a_persisted_negative_receipt() {
        // See `chain_gate_beats_score_even_when_similarity_is_zero`'s guard comment.
        let _g = crate::daemon::dream_cadence::env_test_guard();
        let conn = open();
        insert_episode(
            &conn,
            "ep-1",
            "sess-1",
            "proj",
            "2026-08-01T00:00:00Z",
            "partial",
            1,
            None,
            &["a.rs"],
            None,
            Some(true),
            Some(1),
        );
        // No later episode at all in the project — nothing could have
        // picked ep-1 up.
        insert_vector(&conn, "ep-1", &[1.0, 0.0, 0.0]);

        let report = scan_unfinished(&conn).unwrap();
        assert_eq!(report.stats.never_picked_up, 1);
        let SeedDisposition::NeverPickedUp {
            receipt,
            queue_eligible,
        } = &report.dispositions[0]
        else {
            panic!("expected NeverPickedUp");
        };
        assert_eq!(receipt.seed_episode_id, "ep-1");
        assert_eq!(receipt.corpus_scanned, 0);
        assert_eq!(receipt.max_score, 0.0);
        assert_eq!(receipt.argmax_episode, None);
        assert!(queue_eligible, "todo_count=1 AND present_at_head=true");

        let persisted: (String, i64, f64) = conn
            .query_row(
                "SELECT project, corpus_scanned, max_score FROM backfill_unfinished_receipts
                 WHERE seed_episode_id = 'ep-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(persisted.0, "proj");
        assert_eq!(persisted.1, 0);
        assert_eq!(persisted.2, 0.0);
    }

    #[test]
    fn reconcile_deletes_stale_receipts_no_longer_never_picked_up() {
        // See `chain_gate_beats_score_even_when_similarity_is_zero`'s guard comment.
        let _g = crate::daemon::dream_cadence::env_test_guard();
        let conn = open();
        // First pass: ep-1 is never picked up (no candidates).
        insert_episode(
            &conn,
            "ep-1",
            "sess-1",
            "proj",
            "2026-08-01T00:00:00Z",
            "partial",
            1,
            None,
            &["a.rs"],
            None,
            Some(true),
            Some(1),
        );
        insert_vector(&conn, "ep-1", &[1.0, 0.0, 0.0]);
        let first = scan_unfinished(&conn).unwrap();
        assert_eq!(first.stats.never_picked_up, 1);
        let count_before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM backfill_unfinished_receipts",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count_before, 1);

        // Second pass: a strongly-matching later episode appears — ep-1 is
        // now picked up by score. Fit a permissive tau by NOT adding any
        // chain-completion positives (so DEFAULT_TAU_FALLBACK stays high,
        // ~0.75), and give the candidate a near-identical vector plus full
        // file overlap so it clears the fallback threshold easily.
        insert_episode(
            &conn,
            "ep-2",
            "sess-2",
            "proj",
            "2026-08-02T00:00:00Z",
            "completed",
            0,
            None,
            &["a.rs"],
            None,
            Some(true),
            Some(1),
        );
        insert_vector(&conn, "ep-2", &[1.0, 0.0, 0.0]);
        let second = scan_unfinished(&conn).unwrap();
        assert_eq!(second.stats.picked_up_by_score, 1);
        assert_eq!(second.stats.never_picked_up, 0);

        let count_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM backfill_unfinished_receipts",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count_after, 0,
            "stale receipt for ep-1 must be cleaned up once it is no longer never-picked-up"
        );
    }

    #[test]
    fn obsolescence_conversion_overrides_negative_receipt() {
        // See `chain_gate_beats_score_even_when_similarity_is_zero`'s guard comment.
        let _g = crate::daemon::dream_cadence::env_test_guard();
        let conn = open();
        insert_episode(
            &conn,
            "ep-1",
            "sess-1",
            "proj",
            "2026-08-01T00:00:00Z",
            "partial",
            1,
            None,
            &["a.rs"],
            None,
            Some(false), // absent at HEAD
            Some(61),    // stale past the 60-day threshold
        );
        insert_vector(&conn, "ep-1", &[1.0, 0.0, 0.0]);

        let report = scan_unfinished(&conn).unwrap();
        assert_eq!(report.stats.resolved_by_obsolescence, 1);
        assert_eq!(report.stats.never_picked_up, 0);
        assert!(matches!(
            report.dispositions[0],
            SeedDisposition::ResolvedByObsolescence {
                days_since_last_touch: 61,
                ..
            }
        ));

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM backfill_unfinished_receipts",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 0,
            "an obsolescence conversion is not a negative receipt"
        );
    }

    #[test]
    fn obsolescence_conversion_requires_both_absence_and_staleness() {
        // See `chain_gate_beats_score_even_when_similarity_is_zero`'s guard comment.
        let _g = crate::daemon::dream_cadence::env_test_guard();
        let conn = open();
        // Absent, but not stale enough yet (D3: > 60 days required).
        insert_episode(
            &conn,
            "ep-1",
            "sess-1",
            "proj",
            "2026-08-01T00:00:00Z",
            "partial",
            1,
            None,
            &["a.rs"],
            None,
            Some(false),
            Some(30),
        );
        insert_vector(&conn, "ep-1", &[1.0, 0.0, 0.0]);
        let report = scan_unfinished(&conn).unwrap();
        assert_eq!(report.stats.resolved_by_obsolescence, 0);
        assert_eq!(report.stats.never_picked_up, 1);
    }

    #[test]
    fn null_aliveness_is_never_treated_as_obsolete() {
        // See `chain_gate_beats_score_even_when_similarity_is_zero`'s guard comment.
        let _g = crate::daemon::dream_cadence::env_test_guard();
        let conn = open();
        insert_episode(
            &conn,
            "ep-1",
            "sess-1",
            "proj",
            "2026-08-01T00:00:00Z",
            "partial",
            1,
            None,
            &["a.rs"],
            None,
            None, // unresolvable
            None,
        );
        insert_vector(&conn, "ep-1", &[1.0, 0.0, 0.0]);
        let report = scan_unfinished(&conn).unwrap();
        assert_eq!(report.stats.resolved_by_obsolescence, 0);
        assert_eq!(report.stats.never_picked_up, 1);
        // Also excluded from the queue: aliveness could not be confirmed.
        assert!(report.queue.is_empty());
        assert_eq!(report.stats.excluded_from_queue, 1);
    }

    #[test]
    fn queue_u_priority_renormalizes_over_null_aliveness_instead_of_zeroing() {
        let now = DateTime::parse_from_rfc3339("2026-08-25T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let base = EpisodeRow {
            episode_id: "e".into(),
            project: "p".into(),
            ts: "2026-08-20T00:00:00Z".into(), // recent — no age bonus either way
            outcome: "failed".into(),
            todo_count: 2,
            blockers: None,
            files: HashSet::new(),
            present_at_head: None,
            days_since_last_touch: None,
        };

        let unresolved_score = queue_u_priority(&base, now);

        let mut confirmed_dead = base.clone();
        confirmed_dead.present_at_head = Some(false);
        let confirmed_dead_score = queue_u_priority(&confirmed_dead, now);

        let mut confirmed_alive = base.clone();
        confirmed_alive.present_at_head = Some(true);
        let confirmed_alive_score = queue_u_priority(&confirmed_alive, now);

        // Renormalized (no evidence) must sit strictly between a confirmed
        // "not alive" (aliveness term counts as 0 over the FULL weight) and
        // a confirmed "alive" (aliveness term counts as 1 over the full
        // weight) — never collapse to the same value as "not alive".
        assert!(unresolved_score > confirmed_dead_score);
        assert!(unresolved_score < confirmed_alive_score);

        // And the renormalized value should equal the weighted average of
        // the other three terms alone, dividing by their own weight sum —
        // not by the full 1.0 total weight.
        let expected = (QUEUE_U_WEIGHT_SEVERITY * severity(&base.outcome)
            + QUEUE_U_WEIGHT_TODOS * todos_term(base.todo_count)
            + QUEUE_U_WEIGHT_AGE_BONUS * age_bonus_term(&base.ts, now))
            / (QUEUE_U_WEIGHT_SEVERITY + QUEUE_U_WEIGHT_TODOS + QUEUE_U_WEIGHT_AGE_BONUS);
        assert!((unresolved_score - expected).abs() < 1e-9);
    }

    #[test]
    fn age_bonus_is_binary_not_graded() {
        let now = DateTime::parse_from_rfc3339("2026-08-25T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let recent = "2026-08-01T00:00:00Z"; // 24 days — under threshold
        let old = "2026-01-01T00:00:00Z"; // well over 90 days
        assert_eq!(age_bonus_term(recent, now), 0.0);
        assert_eq!(age_bonus_term(old, now), 1.0);
    }

    #[test]
    fn tau_fits_at_the_target_false_positive_rate() {
        // 100 negatives spread evenly across [0.00, 0.99]; a handful of
        // positives near the top. At a 2% target FPR, exactly 2 of the 100
        // negatives (the top 2) are allowed to sit at or above tau.
        let negatives: Vec<f64> = (0..100).map(|i| i as f64 / 100.0).collect();
        let positives = vec![0.95, 0.90];

        let fit = fit_tau(&positives, &negatives);
        let exceeding = negatives.iter().filter(|&&s| s >= fit.tau).count();
        assert!(
            exceeding as f64 / negatives.len() as f64 <= TAU_TARGET_FPR + 1e-9,
            "empirical FPR {exceeding}/100 must not exceed the 2% target"
        );
        assert_eq!(fit.confusion.false_positive, exceeding);
        assert_eq!(fit.confusion.true_negative, negatives.len() - exceeding);
        assert_eq!(
            fit.confusion.true_positive + fit.confusion.false_negative,
            positives.len()
        );
    }

    #[test]
    fn tau_falls_back_to_the_default_when_no_negatives_exist() {
        let fit = fit_tau(&[0.9, 0.8], &[]);
        assert_eq!(fit.tau, DEFAULT_TAU_FALLBACK);
        assert_eq!(fit.negative_count, 0);
        assert_eq!(fit.positive_count, 2);
    }

    #[test]
    fn tau_falls_back_when_fewer_than_the_minimum_negatives_exist() {
        // P6 (F10): a single matched negative is not enough to fit a
        // meaningful FPR-targeted tau -- must fall back exactly like the
        // zero-negatives case, but still report the true (non-zero)
        // negative_count in the fit.
        let fit = fit_tau(&[0.9], &[0.5]);
        assert_eq!(fit.tau, DEFAULT_TAU_FALLBACK);
        assert_eq!(fit.negative_count, 1);
        assert_eq!(fit.positive_count, 1);

        let fit_two = fit_tau(&[0.9], &[0.5, 0.6]);
        assert_eq!(fit_two.tau, DEFAULT_TAU_FALLBACK);
        assert_eq!(fit_two.negative_count, 2);

        // Exactly at the minimum -- must fit for real, not fall back.
        let fit_three = fit_tau(&[0.9], &[0.1, 0.2, 0.3]);
        assert_ne!(fit_three.tau, DEFAULT_TAU_FALLBACK);
        assert_eq!(fit_three.negative_count, 3);
    }

    #[test]
    fn confusion_matrix_counts_are_internally_consistent() {
        let positives = vec![0.9, 0.4];
        let negatives = vec![0.8, 0.1];
        let m = confusion_matrix(&positives, &negatives, 0.5);
        assert_eq!(m.true_positive, 1); // 0.9
        assert_eq!(m.false_negative, 1); // 0.4
        assert_eq!(m.false_positive, 1); // 0.8
        assert_eq!(m.true_negative, 1); // 0.1
    }

    #[test]
    fn later_candidates_orders_by_parsed_timestamp_not_string_sort() {
        // P10 (F11): mixed `Z`/`+00:00` formatting string-sorts differently
        // than it parses -- "...+00:00" text-sorts BEFORE "...Z" text for
        // the same date, but chronologically the earlier wall-clock instant
        // must still win the argmax tie-break, and `later_candidates` (not
        // whatever order the caller happened to hand it in) must be the one
        // enforcing that.
        fn row(id: &str, ts: &str) -> EpisodeRow {
            EpisodeRow {
                episode_id: id.to_string(),
                project: "p".to_string(),
                ts: ts.to_string(),
                outcome: "completed".to_string(),
                todo_count: 0,
                blockers: None,
                files: HashSet::new(),
                present_at_head: None,
                days_since_last_touch: None,
            }
        }
        let seed = row("seed", "2020-01-01T00:00:00Z");
        // Deliberately handed in an order that does NOT match chronological
        // order, using two different (but equally valid) ISO-8601 offset
        // spellings for the same instant class.
        let project_rows = vec![
            seed.clone(),
            row("later-3", "2020-01-05T00:00:00+00:00"),
            row("later-1", "2020-01-02T00:00:00Z"),
            row("later-2", "2020-01-03T00:00:00+00:00"),
        ];
        let candidates = later_candidates(&seed, &project_rows);
        let ids: Vec<&str> = candidates.iter().map(|r| r.episode_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["later-1", "later-2", "later-3"],
            "must be chronological regardless of input order or timestamp formatting"
        );
    }

    #[test]
    fn jaccard_of_two_empty_sets_is_zero_not_one() {
        let empty: HashSet<String> = HashSet::new();
        assert_eq!(jaccard(&empty, &empty), 0.0);
    }

    #[test]
    fn scan_unfinished_at_is_reproducible_under_an_injected_clock() {
        // P5: `scan_unfinished` alone hard-codes `Utc::now()`, which makes
        // the D1 age>90d bonus wall-clock-dependent and therefore
        // unreproducible for a golden fixture pinned to a fixed date.
        // `scan_unfinished_at` must give the SAME result no matter when the
        // test process actually runs.
        let _g = crate::daemon::dream_cadence::env_test_guard();
        let conn = open();
        insert_episode(
            &conn,
            "ep-1",
            "sess-1",
            "proj",
            "2026-01-01T00:00:00Z", // >90 days before the pinned `now` below
            "partial",
            1,
            None,
            &["a.rs"],
            None,
            Some(true),
            Some(1),
        );
        insert_vector(&conn, "ep-1", &[1.0, 0.0, 0.0]);

        let pinned_now = DateTime::parse_from_rfc3339("2026-08-25T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let report_a = scan_unfinished_at(&conn, pinned_now).unwrap();
        let report_b = scan_unfinished_at(&conn, pinned_now).unwrap();
        assert_eq!(report_a.queue.len(), 1);
        assert_eq!(report_a.queue[0].priority, report_b.queue[0].priority);
        // Age bonus term is 1.0 (>90 days old vs. the pinned `now`),
        // contributing its full 0.25 weight -- assert the exact expected
        // priority, independent of whatever the real wall clock reads right
        // now (the whole point of the injected clock).
        let expected = QUEUE_U_WEIGHT_SEVERITY * severity("partial")
            + QUEUE_U_WEIGHT_TODOS * todos_term(1)
            + QUEUE_U_WEIGHT_AGE_BONUS * 1.0
            + QUEUE_U_WEIGHT_ALIVENESS * 1.0;
        assert!((report_a.queue[0].priority - expected).abs() < 1e-9);
    }

    #[test]
    fn queue_is_sorted_descending_by_priority() {
        // See `chain_gate_beats_score_even_when_similarity_is_zero`'s guard comment.
        let _g = crate::daemon::dream_cadence::env_test_guard();
        let conn = open();
        // Two independent never-picked-up seeds in different projects with
        // different severities, so their Queue U priorities differ.
        insert_episode(
            &conn,
            "ep-low",
            "sess-low",
            "proj-a",
            "2026-08-01T00:00:00Z",
            "partial", // severity 0.6
            1,
            None,
            &["a.rs"],
            None,
            Some(true),
            Some(1),
        );
        insert_episode(
            &conn,
            "ep-high",
            "sess-high",
            "proj-b",
            "2026-08-01T00:00:00Z",
            "failed", // severity 1.0
            1,
            None,
            &["b.rs"],
            None,
            Some(true),
            Some(1),
        );
        insert_vector(&conn, "ep-low", &[1.0, 0.0, 0.0]);
        insert_vector(&conn, "ep-high", &[0.0, 1.0, 0.0]);

        let report = scan_unfinished(&conn).unwrap();
        assert_eq!(report.queue.len(), 2);
        assert_eq!(report.queue[0].episode_id, "ep-high");
        assert_eq!(report.queue[1].episode_id, "ep-low");
        assert!(report.queue[0].priority > report.queue[1].priority);
    }
}
