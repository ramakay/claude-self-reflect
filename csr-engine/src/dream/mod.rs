//! v10 "dreaming": deterministic supersession verdicts over the witness
//! ledger.
//!
//! `dream` is the write path that turns `witness_ledger` evidence into
//! `witness_verdicts` events (`crate::storage::witness_verdicts`), which
//! `crate::storage::chunk_binding` later reads to flag search results whose
//! underlying code claim no longer holds. Zero LLM, zero wall-clock time —
//! every conclusion is drawn from content-hash stamps and git commit-graph
//! ancestry (`codewitness::causal::compare`) alone, exactly like
//! `codewitness` itself.
//!
//! # The join, in one pass per `(project, file, symbol)` anchor
//!
//! For every anchor with committed-tier `witness_ledger` rows at more than
//! one distinct `at_oid` (a single stamp ever recorded has no history to
//! judge):
//!
//! 1. Resolve the anchor's file's repo and live HEAD (same resolution
//!    `import::backfill::stamp_spans_into` uses: prefer a `code_nodes`-
//!    stored `repo_root`, else `extraction::repo_root::repo_root_for_file`).
//!    No resolvable repo -> the whole anchor is abstained
//!    (`abstained_no_repo`).
//! 2. `H` = the row (if any) whose `at_oid` equals that live HEAD. Absent
//!    means the symbol no longer stamps at HEAD (span vanished, or the
//!    file/function is gone).
//! 3. For every witness `W` in the anchor (INCLUDING the row at HEAD
//!    itself — see the reinstatement rule):
//!    - `H` exists and `W.stamp == H.stamp`: content already matches
//!      current truth. If `W`'s own latest recorded verdict was negative
//!      (`anchor_obsolete` / `superseded_by`), emit `anchor_reinstated` —
//!      REGARDLESS of ancestry direction between `W` and HEAD (recovery on
//!      exact blake3 equality is always safe; ancestry proof is required
//!      only for NEGATIVE verdicts). This covers both the forward
//!      A -> B -> A revert and a checkout of an older commit whose content
//!      matches a negatively-marked witness. Otherwise: intact, no event.
//!      Reinstatement requires EXACT blake3 stamp equality — a near-revert
//!      (`A'` close to but not equal to `A`) never reinstates.
//!    - `H` exists and stamps differ: search the anchor's rows whose stamp
//!      equals `H.stamp` for a valid successor `W2` — one that is (i) a
//!      causal DESCENDANT of `W` (`codewitness::causal::compare(W.at_oid,
//!      W2.at_oid) == AncestorOf`) AND (ii) on the HEAD path:
//!      `compare(W2.at_oid, HEAD)` is `AncestorOf` or `Equal`, so a receipt
//!      minted on a divergent, never-merged branch can never certify
//!      supersession. Both checks come from git commit-graph ancestry —
//!      never insertion order or wall-clock time. The first qualifying row
//!      becomes `W`'s successor: emit `superseded_by`. Any `Incomparable`
//!      on either check abstains (`abstained_incomparable_ancestry`); a
//!      candidate pool that fails for other reasons abstains
//!      (`abstained_no_successor`).
//!    - `H` absent: `anchor_obsolete` — but ONLY IF `W.at_oid` is a proper
//!      ancestor of the observed HEAD. If HEAD is an ancestor of `W`
//!      (e.g. a historical commit is checked out) or the two are
//!      `Incomparable`, the run is looking at a HEAD that is BEHIND the
//!      witness and can prove nothing about it — abstain
//!      (`head_behind_witness`).
//!
//! # Supersession vs reinstatement (deliberate contract)
//!
//! A -> B -> A' supersession is LEGITIMATE: supersession on any differing
//! stamp with a valid HEAD-path successor is receipt-backed truth (the
//! content demonstrably changed). Near-revert abstention applies ONLY to
//! reinstatement — `anchor_reinstated` fires strictly on exact blake3
//! stamp equality, never on similarity.
//!
//! # Two-channel consumption (the deliberate v10 contract)
//!
//! The events this module writes are consumed at search time through TWO
//! channels resolved by `storage::witness_verdicts::symbol_verdict_state`
//! (surfaced per chunk by `storage::chunk_binding`): **Demote** — the
//! symbol has negative-latest witnesses AND no witness intact at the
//! observed HEAD (truly gone or fully stale; rank-affecting) — and
//! **Annotate** — negative-latest witnesses exist but so does a witness
//! intact at HEAD (plain A -> B evolution; no rank effect, only a "symbol
//! evolved since earlier evidence; current as of `receipt_oid`" note).
//! Symbol-level binding cannot attribute staleness to individual chunk
//! cohorts (A-era and B-era chunks share the binding), so v10 never
//! demotes on evolution — it annotates with the receipt and lets the
//! reader decide. See `chunk_binding`'s module doc for the full contract.
//! Consumption is wired by search rerank (see `chunk_binding`) —
//! deliberately sequenced as the next lane; this module only writes the
//! events and never touches retrieval.
//!
//! Every candidate event is checked against
//! `witness_verdicts::is_new_event` before counting/writing — re-running
//! `dream` at an unchanged HEAD with an unchanged conclusion writes
//! nothing (see that module's idempotency doc).

pub mod policy;
pub mod report;
pub mod threads;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use codewitness::{causal, Auditor, CausalOrder, ObjectId};
use rusqlite::Connection;

use crate::engine::Engine;
use crate::extraction::repo_root::repo_root_for_file;
use crate::import::backfill::{self, open_repo_head, StampSpansStats};
use crate::storage::codegraph::stored_repo_root_for_file;
use crate::storage::witness_ledger::{self, WitnessLedgerRow};
use crate::storage::witness_verdicts::{self, VerdictKind, WitnessVerdictRow};

/// Outcome of a `dream` run (also used for `--dry-run`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DreamStats {
    /// The prerequisite HEAD `stamp-spans` pass this run always kicks off
    /// first — the join depends on current-truth `witness_ledger` rows
    /// existing at HEAD (see the module doc). This pass runs FOR REAL even
    /// under `--dry-run` (append-only evidence, harmless — see `run_dream`).
    pub stamp_spans: StampSpansStats,
    /// `(project, file, symbol)` anchors with committed-tier rows at more
    /// than one distinct `at_oid` — the population the join actually walks.
    pub anchors_considered: usize,
    /// Non-HEAD committed witness rows examined across those anchors.
    pub witnesses_considered: usize,
    /// Witness content already matches the anchor's current HEAD stamp, and
    /// carried no prior negative verdict — no event.
    pub intact: usize,
    pub superseded: usize,
    pub obsolete: usize,
    pub reinstated: usize,
    /// Every causal-ancestry check for a candidate successor came back
    /// `Incomparable` (diverged branches, no merge) — no event.
    pub abstained_incomparable_ancestry: usize,
    /// A candidate successor existed but failed some other check
    /// (causally backwards/equal) with no `Incomparable` seen — no event.
    pub abstained_no_successor: usize,
    /// `H` was absent but the observed HEAD is NOT a proper descendant of
    /// the witness (HEAD is an ancestor of it — e.g. a historical commit is
    /// checked out — or the two are incomparable): the run cannot prove
    /// obsolescence from a HEAD behind the witness — no event.
    pub head_behind_witness: usize,
    /// The anchor's file has no resolvable git repository (or live HEAD) —
    /// the whole anchor is skipped; none of its witnesses are counted above.
    pub abstained_no_repo: usize,
    /// Verdict events actually written (or, under `--dry-run`, that WOULD
    /// have been written — the decision is identical either way, see
    /// `witness_verdicts::is_new_event`).
    pub events_written: usize,
    /// Candidate events that were identical to the latest recorded event
    /// for their witness and so were skipped (idempotent re-run).
    pub events_deduped: usize,
}

impl DreamStats {
    /// Human-readable one-block summary. Under `--dry-run` the prerequisite
    /// stamp-spans pass still ran for real (see `run_dream`), so its block
    /// is never labeled dry-run — only the verdict side is.
    pub fn format_text(&self, dry_run: bool) -> String {
        let mode = if dry_run {
            " (dry-run, no verdict writes)"
        } else {
            ""
        };
        format!(
            "CSR dream{mode}\n\
             ──────────────────\n\
             {}\
             anchors considered   : {}\n\
             witnesses considered : {}\n\
             intact                : {}\n\
             superseded            : {}\n\
             obsolete               : {}\n\
             reinstated             : {}\n\
             abstained: incomparable_ancestry={}  no_successor={}  head_behind_witness={}  no_repo={}\n\
             events written / deduped : {} / {}\n",
            self.stamp_spans.format_text(false),
            self.anchors_considered,
            self.witnesses_considered,
            self.intact,
            self.superseded,
            self.obsolete,
            self.reinstated,
            self.abstained_incomparable_ancestry,
            self.abstained_no_successor,
            self.head_behind_witness,
            self.abstained_no_repo,
            self.events_written,
            self.events_deduped,
        )
    }
}

/// Live cancellation shared by the daemon shutdown signal and the dreaming
/// kill switch. The CLI path does not construct one.
pub struct DreamCancellation {
    shutdown: Arc<AtomicBool>,
}

impl DreamCancellation {
    pub fn new(shutdown: Arc<AtomicBool>) -> Self {
        Self { shutdown }
    }

    pub fn is_cancelled(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst) || crate::daemon::dream_cadence::dreaming_disabled()
    }
}

/// A daemon dream cycle either completed or stopped at a cancellation
/// checkpoint with partial stats. Cancellation is not an operational error.
pub enum DreamRunResult {
    Complete(DreamStats),
    Cancelled(DreamStats),
}

/// Run one dream cycle: HEAD stamp-spans (prerequisite — see the module
/// doc), then the successor join over the resulting `witness_ledger`.
/// `repo_filter`, when `Some`, restricts the JOIN to anchors whose file
/// resolves to that one repo root (the prerequisite stamp-spans pass itself
/// still visits every repo the code graph knows about — it is cheap and
/// idempotent, so narrowing it independently would add complexity for no
/// benefit).
///
/// `dry_run` suppresses ONLY verdict insertion. The prerequisite HEAD
/// stamp-spans pass ALWAYS runs for real — it mints append-only evidence
/// (harmless, idempotent), and the join's verdict computations are only
/// meaningful against a ledger that actually has HEAD rows. This guarantees
/// a dry run and a subsequent real run at the same HEAD compute identical
/// verdicts: the dry run writes stamps but zero `witness_verdicts` rows.
pub fn run_dream(engine: &Engine, repo_filter: Option<&str>, dry_run: bool) -> Result<DreamStats> {
    match run_dream_inner(engine, repo_filter, dry_run, None)? {
        DreamRunResult::Complete(stats) | DreamRunResult::Cancelled(stats) => Ok(stats),
    }
}

/// Daemon entry point with cancellation checks between stamp files,
/// repositories/anchors, and periodically within large anchors.
pub fn run_dream_with_cancellation(
    engine: &Engine,
    repo_filter: Option<&str>,
    dry_run: bool,
    cancellation: &DreamCancellation,
) -> Result<DreamRunResult> {
    run_dream_inner(engine, repo_filter, dry_run, Some(cancellation))
}

fn run_dream_inner(
    engine: &Engine,
    repo_filter: Option<&str>,
    dry_run: bool,
    cancellation: Option<&DreamCancellation>,
) -> Result<DreamRunResult> {
    if cancellation.is_some_and(DreamCancellation::is_cancelled) {
        return Ok(DreamRunResult::Cancelled(DreamStats::default()));
    }
    let stamp_spans = match cancellation {
        Some(cancel) => {
            backfill::backfill_stamp_spans_cancellable(engine, false, &|| cancel.is_cancelled())?
        }
        None => backfill::backfill_stamp_spans(engine, false)?,
    };
    let mut stats = DreamStats {
        stamp_spans,
        ..Default::default()
    };
    if cancellation.is_some_and(DreamCancellation::is_cancelled) {
        return Ok(DreamRunResult::Cancelled(stats));
    }
    let should_cancel = || cancellation.is_some_and(DreamCancellation::is_cancelled);
    let cancelled = engine.storage().with_connection(|conn| {
        dream_join_cancellable(conn, repo_filter, dry_run, &mut stats, Some(&should_cancel))
    })?;
    if cancelled {
        Ok(DreamRunResult::Cancelled(stats))
    } else {
        Ok(DreamRunResult::Complete(stats))
    }
}

/// A `(project, file, symbol)` anchor key.
type AnchorKey = (String, String, Option<String>);

/// Group already-sorted rows (see `witness_ledger::all_committed_witnesses`'s
/// `ORDER BY project, file, COALESCE(symbol,''), id`) into contiguous
/// `(project, file, symbol)` anchors, preserving each group's insertion
/// (`id`) order.
fn group_by_anchor(rows: Vec<WitnessLedgerRow>) -> Vec<(AnchorKey, Vec<WitnessLedgerRow>)> {
    let mut groups: Vec<(AnchorKey, Vec<WitnessLedgerRow>)> = Vec::new();
    for row in rows {
        let key = (row.project.clone(), row.file.clone(), row.symbol.clone());
        match groups.last_mut() {
            Some((k, v)) if *k == key => v.push(row),
            _ => groups.push((key, vec![row])),
        }
    }
    groups
}

fn parse_oid(s: Option<&str>) -> Option<ObjectId> {
    s.and_then(|s| s.parse::<ObjectId>().ok())
}

/// Memoized `codewitness::causal::compare` results for one dream run,
/// keyed by `(repo_root, a, b)`. The repo namespace matters: `None` records
/// a compare that FAILED (e.g. an oid missing from that repo's object
/// database) so it is not retried — and a failure in one repo must never
/// leak into another repo where the same oid pair could resolve fine.
type CausalCache = HashMap<(String, String, String), Option<CausalOrder>>;

/// `causal::compare(a, b)` on `repo_root`'s repository through `cache`,
/// storing BOTH orderings on a miss (the inverse relation is free) so no
/// oid pair is ever walked twice in one dream run — this is what keeps
/// `find_successor` from repeating merge-base walks across witnesses.
fn compare_cached(
    auditor: &Auditor,
    cache: &mut CausalCache,
    repo_root: &str,
    a: ObjectId,
    b: ObjectId,
) -> Option<CausalOrder> {
    let key = (repo_root.to_string(), a.to_string(), b.to_string());
    if let Some(hit) = cache.get(&key) {
        return *hit;
    }
    let result = causal::compare(auditor.repo(), a, b).ok();
    let inverse = result.map(|order| match order {
        CausalOrder::AncestorOf => CausalOrder::DescendantOf,
        CausalOrder::DescendantOf => CausalOrder::AncestorOf,
        other => other,
    });
    cache.insert(key, result);
    cache.insert(
        (repo_root.to_string(), b.to_string(), a.to_string()),
        inverse,
    );
    result
}

/// The successor join's storage-level core — see the module doc for the
/// full algorithm. Operates on an already-locked `&Connection` so a whole
/// `dream` cycle (potentially many anchors) runs under one lock acquisition
/// rather than one per query.
#[cfg(test)]
fn dream_join(
    conn: &Connection,
    repo_filter: Option<&str>,
    dry_run: bool,
    stats: &mut DreamStats,
) -> Result<()> {
    dream_join_cancellable(conn, repo_filter, dry_run, stats, None).map(|_| ())
}

pub(crate) fn dream_join_cancellable(
    conn: &Connection,
    repo_filter: Option<&str>,
    dry_run: bool,
    stats: &mut DreamStats,
    should_cancel: Option<&dyn Fn() -> bool>,
) -> Result<bool> {
    let rows = witness_ledger::all_committed_witnesses(conn)?;
    let groups = group_by_anchor(rows);

    // Per-repo Auditor + live HEAD, resolved once per distinct repo root
    // across the whole run (mirrors `stamp_spans_into`'s own `repo_cache`).
    let mut repo_cache: BTreeMap<String, Option<(Auditor, ObjectId)>> = BTreeMap::new();
    // Per-file repo_root resolution, cached across anchors sharing a file
    // (distinct symbols in the same file re-resolve nothing).
    let mut file_repo_cache: BTreeMap<String, Option<String>> = BTreeMap::new();
    // Causal-compare memoization, shared across every anchor in this run —
    // see `compare_cached`. Oids are content-addressed, so a single map
    // across repos cannot alias.
    let mut causal_cache: CausalCache = HashMap::new();

    for (_key, group_rows) in groups {
        if should_cancel.is_some_and(|cancel| cancel()) {
            return Ok(true);
        }
        let distinct_oids: BTreeSet<&str> = group_rows
            .iter()
            .filter_map(|r| r.at_oid.as_deref())
            .collect();
        if distinct_oids.len() <= 1 {
            continue; // no history to judge for this anchor.
        }

        let file = group_rows[0].file.clone();
        let repo_root = file_repo_cache
            .entry(file.clone())
            .or_insert_with(|| {
                stored_repo_root_for_file(conn, &file)
                    .unwrap_or(None)
                    .or_else(|| repo_root_for_file(&file))
            })
            .clone();
        let Some(repo_root) = repo_root else {
            stats.abstained_no_repo += 1;
            continue;
        };
        if let Some(filter) = repo_filter {
            if repo_root != filter {
                continue;
            }
        }

        let cached = repo_cache
            .entry(repo_root.clone())
            .or_insert_with(|| open_repo_head(&repo_root));
        let Some((auditor, head_oid)) = cached.as_ref() else {
            stats.abstained_no_repo += 1;
            continue;
        };

        stats.anchors_considered += 1;
        let head_oid_str = head_oid.to_string();
        let h = group_rows
            .iter()
            .find(|r| r.at_oid.as_deref() == Some(head_oid_str.as_str()));
        // Successor-candidate set, computed ONCE per anchor: the rows whose
        // stamp equals the current HEAD stamp (typically 1-2 rows — H itself
        // plus the odd same-content historical row). `find_successor` scans
        // only this set, so the join's worst case is O(k*c) per anchor
        // (k = rows, c = HEAD-stamp-matching rows), not O(k^2).
        let head_stamp_rows: Vec<&WitnessLedgerRow> = match h {
            Some(h) => group_rows.iter().filter(|r| r.stamp == h.stamp).collect(),
            None => Vec::new(),
        };

        for (w_index, w) in group_rows.iter().enumerate() {
            if w_index % 64 == 0 && should_cancel.is_some_and(|cancel| cancel()) {
                return Ok(true);
            }
            let w_is_head_row = w.at_oid.as_deref() == Some(head_oid_str.as_str());
            if !w_is_head_row {
                // H itself is not "history to judge" — it never counts as a
                // considered witness (nor as `intact`) — but it is NOT
                // skipped outright: after a historical checkout the row at
                // HEAD can itself carry a negative latest event, and the
                // reinstatement arm below must still fire for it.
                stats.witnesses_considered += 1;
            }

            let latest_for_w = witness_verdicts::latest_event(conn, w.id)?;

            let candidate: Option<WitnessVerdictRow> = match h {
                // Reinstatement FIRST, and NEVER ancestry-gated: exact
                // blake3 stamp equality with the current HEAD stamp recovers
                // a negative witness regardless of what `compare(W, HEAD)`
                // would say — including H's own row after a checkout of an
                // older commit whose content matches. Recovery on exact
                // stamp equality is always safe; ancestry proof is required
                // only for NEGATIVE verdicts (the arms below). Still
                // strictly EXACT equality — a near-revert A' never
                // reinstates (see "Supersession vs reinstatement").
                Some(h) if h.stamp == w.stamp => match &latest_for_w {
                    Some(l) if l.verdict.is_negative() => Some(WitnessVerdictRow {
                        witness_id: w.id,
                        verdict: VerdictKind::AnchorReinstated,
                        successor_witness_id: None,
                        receipt_oid: Some(head_oid_str.clone()),
                        observed_head_oid: head_oid_str.clone(),
                    }),
                    _ => {
                        if !w_is_head_row {
                            stats.intact += 1;
                        }
                        None
                    }
                },
                // Defensive: a second row at the HEAD oid whose stamp
                // somehow differs from H's — nothing meaningful to judge.
                _ if w_is_head_row => None,
                // Supersession: ANY differing stamp with a valid HEAD-path
                // successor is receipt-backed truth (content changed) — the
                // A -> B -> A' case supersedes deliberately; reinstatement
                // alone requires exact stamp equality (arm above).
                Some(_) => find_successor(
                    auditor,
                    &mut causal_cache,
                    &repo_root,
                    head_oid,
                    w,
                    &head_stamp_rows,
                    stats,
                )
                .map(|w2| WitnessVerdictRow {
                    witness_id: w.id,
                    verdict: VerdictKind::SupersededBy,
                    successor_witness_id: Some(w2.id),
                    receipt_oid: w2.at_oid.clone(),
                    observed_head_oid: head_oid_str.clone(),
                }),
                // H absent: obsolete ONLY when the witness is a proper
                // ancestor of the observed HEAD. A HEAD that is an ancestor
                // of the witness (historical checkout) or incomparable to it
                // proves nothing — abstain (`head_behind_witness`).
                None => {
                    let w_ancestor_of_head = parse_oid(w.at_oid.as_deref()).and_then(|w_oid| {
                        compare_cached(auditor, &mut causal_cache, &repo_root, w_oid, *head_oid)
                    });
                    match w_ancestor_of_head {
                        Some(CausalOrder::AncestorOf) => Some(WitnessVerdictRow {
                            witness_id: w.id,
                            verdict: VerdictKind::AnchorObsolete,
                            successor_witness_id: None,
                            receipt_oid: Some(head_oid_str.clone()),
                            observed_head_oid: head_oid_str.clone(),
                        }),
                        _ => {
                            stats.head_behind_witness += 1;
                            None
                        }
                    }
                }
            };

            let Some(candidate) = candidate else {
                continue;
            };
            match candidate.verdict {
                VerdictKind::SupersededBy => stats.superseded += 1,
                VerdictKind::AnchorObsolete => stats.obsolete += 1,
                VerdictKind::AnchorReinstated => stats.reinstated += 1,
            }
            if witness_verdicts::is_new_event(latest_for_w.as_ref(), &candidate) {
                stats.events_written += 1;
                if !dry_run {
                    witness_verdicts::insert_verdict_if_changed(conn, &candidate)?;
                }
            } else {
                stats.events_deduped += 1;
            }
        }
    }
    Ok(false)
}

/// Search `head_stamp_rows` — the anchor's successor-candidate set,
/// precomputed ONCE per `(project, file, symbol)` group as the rows whose
/// stamp equals the current HEAD stamp (typically 1-2 rows; `h` itself is
/// always one of them) — for a valid successor to `w`. A candidate `W2`
/// qualifies iff ALL of:
///
/// 1. `W2.stamp == h.stamp` (guaranteed by the precomputed set);
/// 2. `w.at_oid` is a PROPER ancestor of `W2.at_oid`
///    (`compare(w, W2) == AncestorOf`);
/// 3. `W2.at_oid` is ancestor-or-equal of the observed HEAD
///    (`compare(W2, HEAD)` is `AncestorOf` or `Equal`) — the HEAD-path
///    requirement that kills divergent-branch receipts.
///
/// Early-exits on the first qualifying candidate. Worst case per anchor is
/// therefore O(k*c) candidate checks (k = witnesses, c = HEAD-stamp-matching
/// rows), never O(k^2) over the group — and every ancestry answer goes
/// through `compare_cached` (memoized per `(repo, oid, oid)` per dream run),
/// so no merge-base walk repeats either. Any `Incomparable` seen on either
/// check abstains the witness (`abstained_incomparable_ancestry`); a pool
/// that fails without one abstains as `abstained_no_successor` (a single
/// bucket increment per call, not per candidate tried).
#[allow(clippy::too_many_arguments)]
fn find_successor<'a>(
    auditor: &Auditor,
    causal_cache: &mut CausalCache,
    repo_root: &str,
    head_oid: &ObjectId,
    w: &WitnessLedgerRow,
    head_stamp_rows: &[&'a WitnessLedgerRow],
    stats: &mut DreamStats,
) -> Option<&'a WitnessLedgerRow> {
    let Some(w_oid) = parse_oid(w.at_oid.as_deref()) else {
        stats.abstained_no_successor += 1;
        return None;
    };

    let mut any_incomparable = false;
    for candidate in head_stamp_rows.iter().copied() {
        let Some(candidate_oid) = parse_oid(candidate.at_oid.as_deref()) else {
            continue;
        };
        if candidate_oid == w_oid {
            continue; // defensive — can't happen since w.stamp != h.stamp here.
        }
        match compare_cached(auditor, causal_cache, repo_root, w_oid, candidate_oid) {
            Some(CausalOrder::AncestorOf) => {}
            Some(CausalOrder::Incomparable) => {
                any_incomparable = true;
                continue;
            }
            _ => continue,
        }
        // HEAD-path check: the successor's commit must itself be on the
        // observed HEAD's history (ancestor-or-equal), or its receipt could
        // come from a divergent, never-merged branch.
        match compare_cached(auditor, causal_cache, repo_root, candidate_oid, *head_oid) {
            Some(CausalOrder::AncestorOf) | Some(CausalOrder::Equal) => return Some(candidate),
            Some(CausalOrder::Incomparable) => any_incomparable = true,
            _ => {}
        }
    }

    if any_incomparable {
        stats.abstained_incomparable_ancestry += 1;
    } else {
        stats.abstained_no_successor += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Minimal temp-git-repo helper, self-contained (no dependency on
    /// `codewitness`'s own test-only `TempRepo`, which lives in a different
    /// crate and isn't exported) — same spirit as
    /// `import::backfill`'s existing `init_git_repo`/`git_in` test helpers.
    struct TestRepo {
        path: PathBuf,
    }

    impl TestRepo {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "csr-dream-test-{}-{n}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).expect("create temp repo dir");
            let repo = Self { path };
            repo.git(&["init", "-q", "-b", "main"]);
            repo.git(&["config", "user.email", "dream-test@csr.invalid"]);
            repo.git(&["config", "user.name", "CSR Dream Test"]);
            repo.git(&["config", "commit.gpgsign", "false"]);
            repo
        }

        fn git(&self, args: &[&str]) -> std::process::Output {
            let mut cmd = Command::new("git");
            for (k, _) in std::env::vars_os() {
                if k.to_string_lossy().starts_with("GIT_") {
                    cmd.env_remove(&k);
                }
            }
            let out = cmd
                .current_dir(&self.path)
                .args(args)
                .output()
                .expect("spawn git");
            assert!(
                out.status.success(),
                "git {args:?} failed:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            out
        }

        fn write(&self, rel: &str, content: &str) {
            std::fs::write(self.path.join(rel), content).expect("write fixture file");
        }

        fn commit(&self, msg: &str) -> String {
            self.git(&["add", "-A"]);
            self.git(&["commit", "-q", "-m", msg]);
            self.head()
        }

        fn checkout_new(&self, name: &str) {
            self.git(&["checkout", "-q", "-b", name]);
        }

        fn checkout(&self, name: &str) {
            self.git(&["checkout", "-q", name]);
        }

        fn head(&self) -> String {
            String::from_utf8(self.git(&["rev-parse", "HEAD"]).stdout)
                .unwrap()
                .trim()
                .to_string()
        }

        /// Absolute path to the fixture file every test in this module
        /// writes to (`lib.rs`) — the correct `WitnessLedgerRow.file` value.
        /// The repo ROOT itself is deliberately never used there:
        /// `repo_root_for_file`'s fallback resolution takes the FILE's
        /// containing directory, so passing the repo root itself would
        /// resolve one directory too high and never find the repo.
        fn file_path(&self) -> String {
            self.path.join("lib.rs").to_string_lossy().to_string()
        }
    }

    impl Drop for TestRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn open_storage() -> Storage {
        Storage::open_memory().unwrap()
    }

    fn ledger_row(project: &str, file: &str, at_oid: &str, stamp: &str) -> WitnessLedgerRow {
        WitnessLedgerRow {
            id: 0,
            project: project.into(),
            file: file.into(),
            symbol: Some("foo".into()),
            span_start: Some(1),
            span_end: Some(3),
            stamp: stamp.into(),
            tier: "committed".into(),
            at_oid: Some(at_oid.into()),
            source_kind: "backfill".into(),
            source_id: Some(at_oid.into()),
        }
    }

    fn run_join(storage: &Storage, dry_run: bool) -> DreamStats {
        let mut stats = DreamStats::default();
        storage
            .with_connection(|conn| dream_join(conn, None, dry_run, &mut stats))
            .unwrap();
        stats
    }

    #[test]
    fn single_at_oid_anchor_has_no_history_to_judge() {
        let repo = TestRepo::new();
        repo.write("lib.rs", "one");
        repo.commit("c1");
        let storage = open_storage();
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(
                    conn,
                    &ledger_row("proj", &repo.file_path(), &repo.head(), "b3:1"),
                )
            })
            .unwrap();
        let stats = run_join(&storage, false);
        assert_eq!(
            stats.anchors_considered, 0,
            "one at_oid == nothing to compare"
        );
        assert_eq!(stats.witnesses_considered, 0);
    }

    #[test]
    fn linear_history_supersedes_the_older_witness() {
        let repo = TestRepo::new();
        repo.write("lib.rs", "1");
        let c1 = repo.commit("c1");
        repo.write("lib.rs", "2");
        let c2 = repo.commit("c2"); // live HEAD.

        let storage = open_storage();
        let file = repo.file_path();
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:1"))?;
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c2, "b3:2"))
            })
            .unwrap();

        let stats = run_join(&storage, false);
        assert_eq!(stats.anchors_considered, 1);
        assert_eq!(
            stats.witnesses_considered, 1,
            "only the non-HEAD c1 witness"
        );
        assert_eq!(stats.superseded, 1);
        assert_eq!(stats.obsolete, 0);
        assert_eq!(stats.abstained_incomparable_ancestry, 0);
        assert_eq!(stats.events_written, 1);

        let w1_id = storage
            .witnesses_for_file("proj", &file)
            .unwrap()
            .into_iter()
            .find(|r| r.at_oid.as_deref() == Some(c1.as_str()))
            .unwrap()
            .id;
        let verdict = storage.latest_witness_verdict(w1_id).unwrap().unwrap();
        assert_eq!(verdict.verdict, VerdictKind::SupersededBy);
        assert_eq!(verdict.receipt_oid, Some(c2));
    }

    #[test]
    fn rerun_at_unchanged_head_writes_nothing_new() {
        let repo = TestRepo::new();
        repo.write("lib.rs", "1");
        let c1 = repo.commit("c1");
        repo.write("lib.rs", "2");
        let c2 = repo.commit("c2");

        let storage = open_storage();
        let file = repo.file_path();
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:1"))?;
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c2, "b3:2"))
            })
            .unwrap();

        let first = run_join(&storage, false);
        assert_eq!(first.events_written, 1);
        let second = run_join(&storage, false);
        assert_eq!(
            second.events_written, 0,
            "identical HEAD + conclusion must be a no-op"
        );
        assert_eq!(second.events_deduped, 1);
    }

    #[test]
    fn dry_run_computes_but_writes_no_verdicts_and_matches_the_real_run() {
        // H5: `--dry-run` suppresses ONLY verdict insertion. A dry run and
        // the real run that follows it must produce IDENTICAL verdict
        // computations, and the dry run must leave zero verdict rows.
        let repo = TestRepo::new();
        repo.write("lib.rs", "1");
        let c1 = repo.commit("c1");
        repo.write("lib.rs", "2");
        let c2 = repo.commit("c2");

        let storage = open_storage();
        let file = repo.file_path();
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:1"))?;
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c2, "b3:2"))
            })
            .unwrap();

        let dry = run_join(&storage, true);
        assert_eq!(dry.superseded, 1);
        assert_eq!(
            dry.events_written, 1,
            "dry-run still reports what WOULD be written"
        );

        let w1_id = storage
            .witnesses_for_file("proj", &file)
            .unwrap()
            .into_iter()
            .find(|r| r.at_oid.as_deref() == Some(c1.as_str()))
            .unwrap()
            .id;
        assert!(
            storage.latest_witness_verdict(w1_id).unwrap().is_none(),
            "dry-run must not actually write a verdict event"
        );

        let real = run_join(&storage, false);
        assert_eq!(
            real, dry,
            "the real run must compute exactly what the dry run reported"
        );
        assert!(
            storage.latest_witness_verdict(w1_id).unwrap().is_some(),
            "the real run actually persists the event"
        );
    }

    #[test]
    fn diverged_branches_supersede_the_ancestor_and_abstain_the_sibling() {
        // Two branches off a common ancestor c1, no merge: c1 -> c_main
        // (live HEAD, on `main`) and c1 -> c_feature (a sibling branch,
        // never merged). The join must supersede the c1 witness (real
        // ancestor of HEAD) and ABSTAIN on the c_feature witness
        // (Incomparable ancestry against HEAD) — never guess either way.
        let repo = TestRepo::new();
        repo.write("lib.rs", "1");
        let c1 = repo.commit("c1");

        repo.checkout_new("feature");
        repo.write("lib.rs", "feature-branch");
        let c_feature = repo.commit("c_feature");

        repo.checkout("main");
        repo.write("lib.rs", "main-branch");
        let c_main = repo.commit("c_main"); // live HEAD stays on main.

        let storage = open_storage();
        let file = repo.file_path();
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:1"))?;
                witness_ledger::insert_witness(
                    conn,
                    &ledger_row("proj", &file, &c_feature, "b3:feature"),
                )?;
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c_main, "b3:main"))
            })
            .unwrap();

        let stats = run_join(&storage, false);
        assert_eq!(stats.anchors_considered, 1);
        assert_eq!(
            stats.witnesses_considered, 2,
            "c1 and c_feature, excluding HEAD (c_main)"
        );
        assert_eq!(stats.superseded, 1, "c1 is a real ancestor of HEAD");
        assert_eq!(
            stats.abstained_incomparable_ancestry, 1,
            "c_feature has no ancestry relationship with HEAD"
        );
        assert_eq!(stats.obsolete, 0);

        let rows = storage.witnesses_for_file("proj", &file).unwrap();
        let c1_id = rows
            .iter()
            .find(|r| r.at_oid.as_deref() == Some(c1.as_str()))
            .unwrap()
            .id;
        let feature_id = rows
            .iter()
            .find(|r| r.at_oid.as_deref() == Some(c_feature.as_str()))
            .unwrap()
            .id;

        let c1_verdict = storage.latest_witness_verdict(c1_id).unwrap().unwrap();
        assert_eq!(c1_verdict.verdict, VerdictKind::SupersededBy);
        assert_eq!(c1_verdict.receipt_oid, Some(c_main));

        assert!(
            storage
                .latest_witness_verdict(feature_id)
                .unwrap()
                .is_none(),
            "incomparable ancestry must abstain — no event, never a guess"
        );
    }

    #[test]
    fn revert_to_a_prior_stamp_reinstates_it() {
        // A -> B -> A: commit c1 (content "A"), c2 (content "B", superseded
        // marker), then c3 reverts back to content "A" — the SAME stamp as
        // c1. A fresh dream pass (simulating a re-run of `stamp_spans_into`
        // after HEAD moved to c3) must reinstate c1's witness rather than
        // leaving it superseded forever.
        let repo = TestRepo::new();
        repo.write("lib.rs", "A");
        let c1 = repo.commit("c1");
        repo.write("lib.rs", "B");
        let c2 = repo.commit("c2");

        let storage = open_storage();
        let file = repo.file_path();
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:A"))?;
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c2, "b3:B"))
            })
            .unwrap();
        let first = run_join(&storage, false);
        assert_eq!(first.superseded, 1, "c1 must be superseded by c2 first");

        // HEAD moves on: c3 reverts the content back to "A" (same stamp as
        // c1) — a fresh witness_ledger row for c3, as `stamp_spans_into`
        // would mint after re-stamping at the new HEAD.
        repo.write("lib.rs", "A");
        let c3 = repo.commit("c3 (revert)");
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c3, "b3:A"))
            })
            .unwrap();

        let second = run_join(&storage, false);
        assert_eq!(second.reinstated, 1, "c1's stamp matches HEAD (c3) again");

        let c1_id = storage
            .witnesses_for_file("proj", &file)
            .unwrap()
            .into_iter()
            .find(|r| r.at_oid.as_deref() == Some(c1.as_str()))
            .unwrap()
            .id;
        let verdict = storage.latest_witness_verdict(c1_id).unwrap().unwrap();
        assert_eq!(verdict.verdict, VerdictKind::AnchorReinstated);
        assert_eq!(verdict.receipt_oid, Some(c3));
    }

    #[test]
    fn symbol_absent_at_head_becomes_obsolete() {
        // The witness's file exists (so the repo/HEAD resolve), but no row
        // in the ledger claims the current HEAD oid for this symbol — the
        // scenario `stamp_spans_into` produces when a span vanishes
        // (`skipped_stamp_error`/`skipped_span_out_of_range`): no fresh row
        // is minted at HEAD, so `H` is absent. Only witnesses that are
        // PROPER ANCESTORS of the observed HEAD become obsolete (H6): the
        // unknown-oid row (not in the object database at all) cannot prove
        // ancestry and must abstain, never guess.
        let repo = TestRepo::new();
        repo.write("lib.rs", "1");
        let c1 = repo.commit("c1");
        repo.write("lib.rs", "2");
        let _c2 = repo.commit("c2"); // live HEAD — deliberately NOT in the ledger.

        let storage = open_storage();
        let file = repo.file_path();
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:1"))?;
                // A second, unknown at_oid so the anchor has >1 distinct
                // at_oid (the join's population gate) without ever matching
                // live HEAD.
                witness_ledger::insert_witness(
                    conn,
                    &ledger_row(
                        "proj",
                        &file,
                        "0000000000000000000000000000000000000000",
                        "b3:0",
                    ),
                )
            })
            .unwrap();

        let stats = run_join(&storage, false);
        assert_eq!(
            stats.obsolete, 1,
            "only c1 (a proven ancestor of HEAD) becomes obsolete"
        );
        assert_eq!(
            stats.head_behind_witness, 1,
            "the unknown-oid row's ancestry is unprovable — abstain"
        );
        assert_eq!(stats.superseded, 0);
    }

    #[test]
    fn checkout_of_an_ancestor_head_abstains_instead_of_obsoleting() {
        // H6: commits c1 -> c2 -> c3; the ledger has rows at c2 and c3;
        // then a HISTORICAL commit (c1) is checked out. H is absent (no
        // ledger row at c1), but HEAD is an ANCESTOR of both witnesses —
        // the run is looking backwards in time and can prove nothing about
        // them. Both must abstain (`head_behind_witness`), never be
        // declared obsolete.
        let repo = TestRepo::new();
        repo.write("lib.rs", "1");
        let c1 = repo.commit("c1");
        repo.write("lib.rs", "2");
        let c2 = repo.commit("c2");
        repo.write("lib.rs", "3");
        let c3 = repo.commit("c3");
        repo.git(&["checkout", "-q", &c1]); // detached HEAD at the ancestor.

        let storage = open_storage();
        let file = repo.file_path();
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c2, "b3:2"))?;
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c3, "b3:3"))
            })
            .unwrap();

        let stats = run_join(&storage, false);
        assert_eq!(
            stats.obsolete, 0,
            "a HEAD behind the witnesses proves nothing"
        );
        assert_eq!(stats.head_behind_witness, 2);
        assert_eq!(stats.events_written, 0);
    }

    #[test]
    fn divergent_branch_receipt_never_certifies_supersession() {
        // H4: main is c1 -> c_main (live HEAD); a feature branch off c1
        // carries c_feat with the SAME content (same stamp) as c_main.
        // c_feat is a causal descendant of c1, so without the HEAD-path
        // check it would be picked as c1's successor (it was inserted
        // first) — a receipt minted on a divergent, never-merged branch.
        // The successor must be c_main (ancestor-or-equal of HEAD).
        let repo = TestRepo::new();
        repo.write("lib.rs", "1");
        let c1 = repo.commit("c1");

        repo.checkout_new("feature");
        repo.write("lib.rs", "2");
        let c_feat = repo.commit("c_feat");

        repo.checkout("main");
        repo.write("lib.rs", "2"); // same content as the feature branch.
        let c_main = repo.commit("c_main"); // live HEAD.

        let storage = open_storage();
        let file = repo.file_path();
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:1"))?;
                // c_feat inserted BEFORE c_main so a first-match-wins scan
                // without the HEAD-path check would pick it.
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c_feat, "b3:2"))?;
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c_main, "b3:2"))
            })
            .unwrap();

        let stats = run_join(&storage, false);
        assert_eq!(stats.superseded, 1, "c1 must be superseded: {stats:?}");

        let c1_id = storage
            .witnesses_for_file("proj", &file)
            .unwrap()
            .into_iter()
            .find(|r| r.at_oid.as_deref() == Some(c1.as_str()))
            .unwrap()
            .id;
        let verdict = storage.latest_witness_verdict(c1_id).unwrap().unwrap();
        assert_eq!(verdict.verdict, VerdictKind::SupersededBy);
        assert_eq!(
            verdict.receipt_oid,
            Some(c_main),
            "the receipt must come from the HEAD path, never the divergent branch"
        );
    }

    #[test]
    fn checkout_of_matching_older_head_reinstates_negative_witness() {
        // Reinstatement is NEVER ancestry-gated: c1 (content A) -> c2
        // (content B). Dream at HEAD=c2 marks the c1 witness superseded.
        // Then c1 itself is checked out (HEAD moves BACKWARDS): the c1
        // witness's stamp equals the current HEAD stamp (it IS the row at
        // HEAD), so its negative verdict must be recovered with
        // anchor_reinstated even though compare(W, HEAD) is Equal, not
        // AncestorOf — and the symbol's Demote state must clear.
        let repo = TestRepo::new();
        repo.write("lib.rs", "A");
        let c1 = repo.commit("c1");
        repo.write("lib.rs", "B");
        let c2 = repo.commit("c2");

        let storage = open_storage();
        let file = repo.file_path();
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:A"))?;
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c2, "b3:B"))
            })
            .unwrap();

        let first = run_join(&storage, false);
        assert_eq!(first.superseded, 1, "c1 superseded at HEAD=c2 first");

        repo.git(&["checkout", "-q", &c1]); // HEAD moves backwards to c1.
        let second = run_join(&storage, false);
        assert_eq!(
            second.reinstated, 1,
            "the negatively-marked c1 witness matches the checked-out HEAD stamp: {second:?}"
        );

        let c1_id = storage
            .witnesses_for_file("proj", &file)
            .unwrap()
            .into_iter()
            .find(|r| r.at_oid.as_deref() == Some(c1.as_str()))
            .unwrap()
            .id;
        let verdict = storage.latest_witness_verdict(c1_id).unwrap().unwrap();
        assert_eq!(verdict.verdict, VerdictKind::AnchorReinstated);
        assert_eq!(verdict.receipt_oid, Some(c1.clone()));

        // Channel rules: no witness of the symbol carries an uncancelled
        // negative event any more (c2's witness was abstained, never
        // negatively marked) — the Demote state is fully cleared.
        assert!(
            storage
                .symbol_verdict_state("proj", &file, Some("foo"))
                .unwrap()
                .is_none(),
            "Demote state must clear after reinstatement"
        );
    }

    #[test]
    fn near_revert_supersedes_rather_than_abstaining() {
        // C3 contract: A -> B -> A' (A' close to but NOT stamp-equal to A)
        // is legitimate supersession — content demonstrably changed and a
        // valid HEAD-path successor exists. Near-revert abstention applies
        // ONLY to reinstatement (exact stamp equality), never here.
        let repo = TestRepo::new();
        repo.write("lib.rs", "A");
        let c1 = repo.commit("c1");
        repo.write("lib.rs", "B");
        let c2 = repo.commit("c2");
        repo.write("lib.rs", "A "); // A-prime: almost, but not exactly, A.
        let c3 = repo.commit("c3");

        let storage = open_storage();
        let file = repo.file_path();
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:A"))?;
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c2, "b3:B"))?;
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c3, "b3:Aprime"))
            })
            .unwrap();

        let stats = run_join(&storage, false);
        assert_eq!(
            stats.superseded, 2,
            "both c1 (A) and c2 (B) are superseded by c3 (A'): {stats:?}"
        );
        assert_eq!(
            stats.reinstated, 0,
            "A' is not stamp-equal — no reinstatement"
        );
        assert_eq!(stats.abstained_no_successor, 0);
        assert_eq!(stats.abstained_incomparable_ancestry, 0);
    }
}
