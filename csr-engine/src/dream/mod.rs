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
//! # The join, in one pass per `(file, symbol)` anchor
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
//!    itself — see the reinstatement rule), the ledger stamps pick a
//!    CANDIDATE verdict kind:
//!    - `H` exists and `W.stamp == H.stamp`: content already matches
//!      current truth. If `W`'s own latest live verdict was negative
//!      (`anchor_obsolete` / `superseded_by`), the candidate is
//!      `anchor_reinstated` — but ONLY from a HEAD that has moved forward
//!      from where that negative verdict was observed (see "Forward-only
//!      judgment"). A checkout of a different branch or an older commit
//!      whose content happens to match proves nothing and abstains.
//!      Otherwise: intact, no event. Reinstatement requires EXACT blake3
//!      stamp equality — a near-revert (`A'` close to but not equal to `A`)
//!      never reinstates.
//!    - `H` exists and stamps differ: search the anchor's rows whose stamp
//!      equals `H.stamp` for a valid successor `W2` — one that is (i) a
//!      causal DESCENDANT of `W` (`codewitness::causal::compare(W.at_oid,
//!      W2.at_oid) == AncestorOf`) AND (ii) on the HEAD path:
//!      `compare(W2.at_oid, HEAD)` is `AncestorOf` or `Equal`, so a receipt
//!      minted on a divergent, never-merged branch can never certify
//!      supersession. Both checks come from git commit-graph ancestry —
//!      never insertion order or wall-clock time. The first qualifying row
//!      becomes `W`'s successor: candidate `superseded_by`. Any
//!      `Incomparable` on either check abstains
//!      (`abstained_incomparable_ancestry`); a candidate pool that fails for
//!      other reasons abstains (`abstained_no_successor`).
//!    - `H` absent: candidate `anchor_obsolete` — but ONLY IF `W.at_oid` is
//!      a proper ancestor of the observed HEAD. If HEAD is an ancestor of
//!      `W` (e.g. a historical commit is checked out) or the two are
//!      `Incomparable`, the run is looking at a HEAD that is BEHIND the
//!      witness and can prove nothing about it — abstain
//!      (`head_behind_witness`).
//! 4. A candidate that is NEW for its witness (its kind differs from the
//!    witness's latest live event) must then be backed by a true receipt
//!    (`receipt` module). Ledger stamps only pick the candidate; they are
//!    weak evidence of change (rows minted by two span methods, raw stamps
//!    that count a reformat as a rewrite, and a `H` that can be missing
//!    while the symbol is at HEAD). So the symbol itself is re-read from the
//!    committed bytes (`receipt::SymbolState`: present with a normalized
//!    span stamp / absent / unknown) and the claim is checked against git:
//!    - `anchor_obsolete`: the symbol is present at `W.at_oid` and ABSENT in
//!      the HEAD blob (present there -> `abstained_present_at_head`); the
//!      receipt is the first commit in `W..HEAD` where it is absent. A
//!      same-leaf symbol under another qualification at HEAD or at that
//!      receipt commit means it MOVED, not died (`abstained_moved`).
//!    - `superseded_by`: the successor's normalized span content differs
//!      from `W`'s (equal -> `abstained_same_content`); the receipt is the
//!      first commit in `W..W2.at_oid` whose content differs from `W`'s.
//!    - `anchor_reinstated`: HEAD content equals `W`'s; the receipt is the
//!      first commit in `L.observed_head..HEAD` where the content equals
//!      `W`'s again (`L` = the negative event being cancelled).
//!
//!    A needed state that is unknown (unparseable, unsupported language,
//!    unreadable blob) abstains (`abstained_unparseable`); a witness symbol
//!    that is not in the file at its own commit abstains
//!    (`abstained_not_at_witness`); a conclusion with no nameable commit
//!    abstains (`abstained_no_receipt`). The receipt is NEVER the observed
//!    HEAD or the successor's `at_oid`. Receipts are computed lazily, only
//!    for new events, so re-running at a newer HEAD re-searches nothing for
//!    conclusions already on record.
//!
//! A witness whose symbol contains `#` (the positional collision suffix,
//! `remove#5`) gets no verdict at all (`abstained_positional_symbol`): the
//! number is a span-order rank, not an identity.
//!
//! # Forward-only judgment
//!
//! Before ANY candidate event (superseded / obsolete / reinstated) is
//! emitted for a witness `W` that already has a latest live event `L`, the
//! run requires `compare(L.observed_head_oid, HEAD)` to be `AncestorOf` or
//! `Equal`: the current HEAD must be where the previous verdict was drawn,
//! or a descendant of it. Otherwise the run abstains (`head_not_forward`) —
//! including when the observed oid does not parse or the compare fails.
//! This kills branch flapping (a `main` <-> `dev` checkout of the same repo
//! used to flip `anchor_obsolete` / `anchor_reinstated` on every dream
//! pass): reinstatement now requires HEAD to have moved forward from where
//! the negative verdict was observed. A witness with no live event has
//! nothing to flap and is judged on the current HEAD as before.
//!
//! # Supersession vs reinstatement (deliberate contract)
//!
//! A -> B -> A' supersession is LEGITIMATE: supersession on any differing
//! stamp with a valid HEAD-path successor is a candidate; it becomes truth
//! only when the symbol's normalized content demonstrably changed and a
//! commit can be named as the one that changed it. Near-revert abstention applies ONLY to
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

pub mod backfill;
pub mod cli;
pub mod policy;
pub(crate) mod receipt;
pub mod report;
pub(crate) mod strategy;
pub mod threads;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use codewitness::{causal, Auditor, CausalOrder, ObjectId};
use rusqlite::Connection;

use crate::engine::Engine;
use crate::extraction::repo_root::repo_root_for_file;
use crate::import::backfill::{
    self as witness_backfill, open_repo_head, relpath_in_repo, StampSpansStats,
};
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
    /// `(file, symbol)` anchors with committed-tier rows at more
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
    /// A candidate event was withheld because the witness's latest live
    /// event was observed at a HEAD that the current HEAD has NOT moved
    /// forward from (the current HEAD is behind it, on a diverged branch,
    /// or the ancestry could not be proven / the oid did not parse) — the
    /// run is judging a different line of history than the one the existing
    /// verdict was drawn on (branch flapping), so it abstains.
    pub head_not_forward: usize,
    /// The anchor's file has no resolvable git repository (or live HEAD) —
    /// the whole anchor is skipped; none of its witnesses are counted above.
    pub abstained_no_repo: usize,
    /// Witness skipped outright: its symbol carries the `#N` collision
    /// suffix the HEAD pass mints (`qualify_witness_symbols`). The suffix is
    /// positional - adding one sibling renumbers the rest - so the same
    /// string names different functions over time and no verdict about it
    /// can be trusted. HEAD-row witnesses are skipped silently, like every
    /// other non-considered HEAD row.
    pub abstained_positional_symbol: usize,
    /// A NEW conclusion was withheld because the file's content at a commit
    /// the receipt needs could not be established: not a clean parse,
    /// unsupported language, or unreadable blob - see `receipt::SymbolState`.
    pub abstained_unparseable: usize,
    /// A NEW conclusion was withheld because the file parses cleanly at the
    /// witness's own commit but the witness symbol is not in it (many legacy
    /// `source_kind='backfill'` rows claim a symbol at a commit that never
    /// contained it).
    pub abstained_not_at_witness: usize,
    /// `anchor_obsolete` withheld: the join found no stamp at HEAD but the
    /// symbol is in the HEAD blob (the HEAD pass missed it).
    pub abstained_present_at_head: usize,
    /// `superseded_by` withheld: the successor's normalized span content
    /// equals the witness's (reformat, or two span methods over the same
    /// code) — nothing was superseded.
    pub abstained_same_content: usize,
    /// `anchor_obsolete` withheld: the qualified symbol is gone but the file
    /// still has a symbol with the same leaf name (at the receipt commit or
    /// at HEAD) - the symbol moved or was re-nested, not deleted.
    pub abstained_moved: usize,
    /// A NEW conclusion was withheld because no commit could be named as
    /// the one that made the change (empty or oversized history range, git
    /// failure, or an unknown state met first). Never falls back to HEAD.
    pub abstained_no_receipt: usize,
    /// New events whose receipt is a commit found in history.
    pub receipts_found: usize,
    /// `git log` receipt-range searches actually run (memo misses). A
    /// re-run whose conclusions are already recorded runs none.
    pub receipt_searches: usize,
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
             abstained: incomparable_ancestry={}  no_successor={}  head_behind_witness={}  head_not_forward={}  no_repo={}  positional_symbol={}\n\
             receipts: found={}  searches={}\n\
             receipt abstained: unparseable={}  not_at_witness={}  present_at_head={}  same_content={}  moved={}  no_receipt={}\n\
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
            self.head_not_forward,
            self.abstained_no_repo,
            self.abstained_positional_symbol,
            self.receipts_found,
            self.receipt_searches,
            self.abstained_unparseable,
            self.abstained_not_at_witness,
            self.abstained_present_at_head,
            self.abstained_same_content,
            self.abstained_moved,
            self.abstained_no_receipt,
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

/// Run a dream cycle while advertising the live "dreaming" statusline marker
/// (`crate::status::dream_state`) for the cycle's duration. `label` names the
/// trigger written into the marker (e.g. `"compact"`).
///
/// Returns `Ok(None)` WITHOUT dreaming when a fresh marker is already active —
/// the marker file is the cross-process single-flight signal, so a
/// compact-triggered dream never piles on top of a daemon pass already in
/// flight. The marker is cleared on completion and on error alike; an outright
/// crash falls back to the 15-minute staleness floor in
/// `dream_state::read_active_marker`.
pub fn run_dream_marked(
    engine: &Engine,
    repo_filter: Option<&str>,
    dry_run: bool,
    label: &str,
) -> Result<Option<DreamStats>> {
    if crate::status::dream_state::read_active_marker(chrono::Utc::now()).is_some() {
        return Ok(None);
    }
    crate::status::dream_state::write_marker(label);
    let result = run_dream(engine, repo_filter, dry_run);
    crate::status::dream_state::clear_marker();
    result.map(Some)
}

/// Whether a compaction should trigger a background dream cycle. Off when
/// dreaming is disabled wholesale (`CSR_NO_DREAMING`) or when the
/// compact-specific opt-out (`CSR_NO_DREAM_ON_COMPACT`) is set.
pub fn compact_dream_enabled() -> bool {
    compact_dream_enabled_from(
        crate::daemon::dream_cadence::dreaming_disabled(),
        std::env::var("CSR_NO_DREAM_ON_COMPACT").as_deref() == Ok("1"),
    )
}

fn compact_dream_enabled_from(dreaming_disabled: bool, compact_opt_out: bool) -> bool {
    !dreaming_disabled && !compact_opt_out
}

/// Spawn a detached `csr-engine dream --mark compact` process so a compaction
/// triggers exactly one dream cycle over the freshly-imported transcript,
/// independent of the idle-gated daemon cadence (a compaction is an explicit
/// trigger, so it deliberately bypasses the "defer under a live session"
/// rule). Fire-and-forget: it never blocks compaction, discards stdio, and
/// self-skips via the marker guard in [`run_dream_marked`] if a dream is
/// already running.
pub fn spawn_detached_compact_dream() {
    if !compact_dream_enabled() {
        return;
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            tracing::debug!(%error, "compact dream: cannot resolve own exe; skipping");
            return;
        }
    };
    match std::process::Command::new(exe)
        .args(["dream", "--mark", "compact"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => tracing::debug!("compact dream: spawned detached cycle"),
        Err(error) => tracing::debug!(%error, "compact dream: spawn failed (non-fatal)"),
    }
}

#[cfg(test)]
mod compact_gate_tests {
    use super::compact_dream_enabled_from;

    #[test]
    fn compact_dream_gate_truth_table() {
        assert!(compact_dream_enabled_from(false, false), "on by default");
        assert!(
            !compact_dream_enabled_from(true, false),
            "CSR_NO_DREAMING disables it"
        );
        assert!(
            !compact_dream_enabled_from(false, true),
            "CSR_NO_DREAM_ON_COMPACT disables it"
        );
        assert!(!compact_dream_enabled_from(true, true));
    }
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
        Some(cancel) => witness_backfill::backfill_stamp_spans_cancellable(engine, false, &|| {
            cancel.is_cancelled()
        })?,
        None => witness_backfill::backfill_stamp_spans(engine, false)?,
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

/// A `(file, symbol)` anchor key. The session-derived `project` is
/// deliberately NOT part of the identity: the HEAD stamp pass assigns one
/// project per file, so the same absolute file+symbol is stamped under
/// different projects over time, and a project-keyed group would never see
/// the HEAD row minted under the newer project (every old group would go
/// `anchor_obsolete` on every pass).
type AnchorKey = (String, Option<String>);

/// Group already-sorted rows (see `witness_ledger::all_committed_witnesses`'s
/// `ORDER BY file, COALESCE(symbol,''), id`) into contiguous `(file,
/// symbol)` anchors, preserving each group's insertion (`id`) order.
fn group_by_anchor(rows: Vec<WitnessLedgerRow>) -> Vec<(AnchorKey, Vec<WitnessLedgerRow>)> {
    let mut groups: Vec<(AnchorKey, Vec<WitnessLedgerRow>)> = Vec::new();
    for row in rows {
        let key = (row.file.clone(), row.symbol.clone());
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
    // Receipt memo (parsed symbol maps, commit lists) for the whole run.
    let mut resolver = receipt::ReceiptResolver::new();

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

            // Positional symbols (`remove#5`): the suffix is the Nth
            // collision by span order, so it names a different function as
            // soon as a sibling is added or removed. Comparing "remove#10"
            // across commits compares two different functions - no verdict.
            if w.symbol.as_deref().is_some_and(|s| s.contains('#')) {
                if !w_is_head_row {
                    stats.abstained_positional_symbol += 1;
                }
                continue;
            }

            let latest_for_w = witness_verdicts::latest_event(conn, w.id)?;
            // A witness whose latest live event is a reinstatement had every
            // earlier negative event cancelled: nothing at or before the
            // commit that reinstatement observed may be reused as a
            // successor or a receipt (see `reinstatement_boundary`).
            let boundary = reinstatement_boundary(latest_for_w.as_ref());
            let boundary_oid = parse_oid(boundary);

            let candidate: Option<(WitnessVerdictRow, Option<&WitnessLedgerRow>)> = match h {
                // Reinstatement FIRST: exact blake3 stamp equality with the
                // current HEAD stamp recovers a negative witness. It is not
                // gated on `compare(W, HEAD)`, but it IS gated (below, once
                // a candidate exists) on the forward-only rule: HEAD must
                // have moved forward from where the negative verdict was
                // observed, so a checkout of another branch cannot flip it.
                // Still strictly EXACT equality — a near-revert A' never
                // reinstates (see "Supersession vs reinstatement").
                Some(h) if h.stamp == w.stamp => match &latest_for_w {
                    Some(l) if l.verdict.is_negative() => Some((
                        WitnessVerdictRow {
                            witness_id: w.id,
                            verdict: VerdictKind::AnchorReinstated,
                            successor_witness_id: None,
                            // Filled in by `receipt::resolve_receipt` once
                            // the event is known to be new.
                            receipt_oid: None,
                            observed_head_oid: head_oid_str.clone(),
                        },
                        None,
                    )),
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
                    boundary_oid,
                    &head_stamp_rows,
                    stats,
                )
                .map(|w2| {
                    (
                        WitnessVerdictRow {
                            witness_id: w.id,
                            verdict: VerdictKind::SupersededBy,
                            successor_witness_id: Some(w2.id),
                            receipt_oid: None,
                            observed_head_oid: head_oid_str.clone(),
                        },
                        Some(w2),
                    )
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
                        Some(CausalOrder::AncestorOf) => Some((
                            WitnessVerdictRow {
                                witness_id: w.id,
                                verdict: VerdictKind::AnchorObsolete,
                                successor_witness_id: None,
                                receipt_oid: None,
                                observed_head_oid: head_oid_str.clone(),
                            },
                            None,
                        )),
                        _ => {
                            stats.head_behind_witness += 1;
                            None
                        }
                    }
                }
            };

            let Some((mut candidate, successor)) = candidate else {
                continue;
            };
            // Forward-only judgment: a witness that already has a live
            // event may only be re-judged from a HEAD that has moved
            // FORWARD (descendant-or-equal) from the HEAD where that event
            // was observed. Otherwise the run is on another branch (or a
            // checkout of history) and would flap the verdict — abstain.
            if let Some(l) = &latest_for_w {
                let forward = parse_oid(Some(l.observed_head_oid.as_str()))
                    .and_then(|observed| {
                        compare_cached(auditor, &mut causal_cache, &repo_root, observed, *head_oid)
                    })
                    .is_some_and(|order| {
                        matches!(order, CausalOrder::AncestorOf | CausalOrder::Equal)
                    });
                if !forward {
                    stats.head_not_forward += 1;
                    continue;
                }
            }
            // Receipts are computed ONLY for a conclusion that is new for
            // this witness (kind differs from its latest live event): a
            // re-run at a newer HEAD must not re-search git for conclusions
            // already on record.
            let is_new = witness_verdicts::is_new_event(latest_for_w.as_ref(), &candidate);
            if is_new {
                let searches_before = resolver.searches;
                let resolved = resolve_candidate_receipt(
                    &mut resolver,
                    &w.project,
                    &repo_root,
                    auditor,
                    w,
                    &candidate,
                    successor,
                    latest_for_w.as_ref(),
                    boundary,
                    &head_oid_str,
                );
                stats.receipt_searches += resolver.searches - searches_before;
                match resolved {
                    Ok(receipt_oid) => {
                        candidate.receipt_oid = Some(receipt_oid);
                        stats.receipts_found += 1;
                    }
                    Err(abstain) => {
                        match abstain {
                            receipt::Abstain::Unparseable => stats.abstained_unparseable += 1,
                            receipt::Abstain::NotAtWitness => stats.abstained_not_at_witness += 1,
                            receipt::Abstain::PresentAtHead => stats.abstained_present_at_head += 1,
                            receipt::Abstain::SameContent => stats.abstained_same_content += 1,
                            receipt::Abstain::Moved => stats.abstained_moved += 1,
                            receipt::Abstain::NoReceipt => stats.abstained_no_receipt += 1,
                        }
                        continue;
                    }
                }
            }
            match candidate.verdict {
                VerdictKind::SupersededBy => stats.superseded += 1,
                VerdictKind::AnchorObsolete => stats.obsolete += 1,
                VerdictKind::AnchorReinstated => stats.reinstated += 1,
            }
            if is_new {
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

/// Resolve the true receipt for a NEW candidate event (see `receipt`).
#[allow(clippy::too_many_arguments)]
fn resolve_candidate_receipt(
    resolver: &mut receipt::ReceiptResolver,
    project: &str,
    repo_root: &str,
    auditor: &Auditor,
    w: &WitnessLedgerRow,
    candidate: &WitnessVerdictRow,
    successor: Option<&WitnessLedgerRow>,
    latest: Option<&WitnessVerdictRow>,
    boundary: Option<&str>,
    head_oid: &str,
) -> std::result::Result<String, receipt::Abstain> {
    use receipt::Abstain;
    let relpath = relpath_in_repo(repo_root, &w.file)
        .or_else(|| {
            // Symlinked temp/home roots: the stored file and the discovered
            // repo root may spell the same directory differently.
            let file = std::fs::canonicalize(&w.file).ok()?;
            let root = std::fs::canonicalize(repo_root).ok()?;
            relpath_in_repo(&root.to_string_lossy(), &file.to_string_lossy())
        })
        .ok_or(Abstain::Unparseable)?;
    let w_oid = w.at_oid.as_deref().ok_or(Abstain::Unparseable)?;
    let subject = receipt::Subject {
        auditor,
        repo_root,
        file: &w.file,
        relpath: &relpath,
        project,
        symbol: w.symbol.as_deref(),
    };
    let claim = match candidate.verdict {
        VerdictKind::AnchorObsolete => receipt::Claim::Obsolete { head: head_oid },
        VerdictKind::SupersededBy => receipt::Claim::Superseded {
            successor_oid: successor
                .and_then(|s| s.at_oid.as_deref())
                .ok_or(Abstain::Unparseable)?,
        },
        VerdictKind::AnchorReinstated => receipt::Claim::Reinstated {
            head: head_oid,
            negative_observed_head: latest
                .map(|l| l.observed_head_oid.as_str())
                .ok_or(Abstain::Unparseable)?,
        },
    };
    receipt::resolve_receipt(resolver, &subject, w_oid, boundary, &claim)
}

/// The HEAD a witness's latest live event was a reinstatement at, if it was:
/// the earliest point a NEW negative conclusion about that witness may draw
/// on. Everything before it was cancelled by the reinstatement, so a
/// successor row or a receipt commit from that older era (A at C1, B at C2,
/// A restored at C3, B again at C4: the second supersession is C4's, not
/// C2's) must not be reused.
fn reinstatement_boundary(latest: Option<&WitnessVerdictRow>) -> Option<&str> {
    latest
        .filter(|l| l.verdict == VerdictKind::AnchorReinstated)
        .map(|l| l.observed_head_oid.as_str())
}

/// Search `head_stamp_rows` — the anchor's successor-candidate set,
/// precomputed ONCE per `(file, symbol)` group as the rows whose
/// stamp equals the current HEAD stamp (typically 1-2 rows; `h` itself is
/// always one of them) — for a valid successor to `w`. A candidate `W2`
/// qualifies iff ALL of:
///
/// 1. `W2.stamp == h.stamp` (guaranteed by the precomputed set);
/// 2. `w.at_oid` is a PROPER ancestor of `W2.at_oid`
///    (`compare(w, W2) == AncestorOf`);
/// 3. `W2.at_oid` is ancestor-or-equal of the observed HEAD
///    (`compare(W2, HEAD)` is `AncestorOf` or `Equal`) — the HEAD-path
///    requirement that kills divergent-branch receipts;
/// 4. when `boundary` is set (the witness was last reinstated there),
///    `W2.at_oid` is a descendant-or-equal of it: a successor minted before
///    the reinstatement belongs to the cancelled era.
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
    boundary: Option<ObjectId>,
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
        if let Some(boundary) = boundary {
            match compare_cached(auditor, causal_cache, repo_root, boundary, candidate_oid) {
                Some(CausalOrder::AncestorOf) | Some(CausalOrder::Equal) => {}
                Some(CausalOrder::Incomparable) => {
                    any_incomparable = true;
                    continue;
                }
                _ => continue,
            }
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

    /// Parseable Rust source defining `foo` whose body is `body`.
    fn foo_src(body: &str) -> String {
        format!("fn foo() {{\n    {body}\n}}\n")
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
        repo.write("lib.rs", &foo_src("one"));
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
        repo.write("lib.rs", &foo_src("1"));
        let c1 = repo.commit("c1");
        repo.write("lib.rs", &foo_src("2"));
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
        repo.write("lib.rs", &foo_src("1"));
        let c1 = repo.commit("c1");
        repo.write("lib.rs", &foo_src("2"));
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
    fn same_conclusion_at_a_newer_head_is_deduped() {
        // c1 -> c2 supersedes the c1 witness. HEAD then moves forward to c3
        // (content changed again): c1 is still superseded, now with a
        // different successor/receipt/observed HEAD. Same verdict kind, so
        // no new event for c1; c2's witness is genuinely newly superseded.
        let repo = TestRepo::new();
        repo.write("lib.rs", &foo_src("1"));
        let c1 = repo.commit("c1");
        repo.write("lib.rs", &foo_src("2"));
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

        repo.write("lib.rs", &foo_src("3"));
        let c3 = repo.commit("c3");
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c3, "b3:3"))
            })
            .unwrap();
        let second = run_join(&storage, false);
        assert_eq!(second.superseded, 2, "{second:?}");
        assert_eq!(
            second.events_written, 1,
            "only c2's witness is new: {second:?}"
        );
        assert_eq!(second.events_deduped, 1, "{second:?}");
        let n: i64 = storage
            .with_connection(|conn| {
                Ok(conn.query_row("SELECT COUNT(*) FROM witness_verdicts", [], |r| r.get(0))?)
            })
            .unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn witness_with_only_sub_floor_events_is_judged_fresh_by_the_join() {
        let repo = TestRepo::new();
        repo.write("lib.rs", &foo_src("1"));
        let c1 = repo.commit("c1");
        repo.write("lib.rs", &foo_src("2"));
        let c2 = repo.commit("c2");

        let storage = open_storage();
        let file = repo.file_path();
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:1"))?;
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c2, "b3:2"))
            })
            .unwrap();
        assert_eq!(run_join(&storage, false).events_written, 1);
        let deduped = run_join(&storage, false);
        assert_eq!(deduped.events_deduped, 1, "live event dedupes: {deduped:?}");

        // Retire every event so far behind the v2 floor.
        storage
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE meta SET value = (SELECT CAST(MAX(id) AS TEXT) FROM witness_verdicts)
                     WHERE key = 'witness_verdicts_v2_floor'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let fresh = run_join(&storage, false);
        assert_eq!(
            fresh.events_written, 1,
            "no live event: the witness is judged fresh and re-minted: {fresh:?}"
        );
        assert_eq!(fresh.events_deduped, 0);
        assert_eq!(fresh.head_not_forward, 0);
    }

    #[test]
    fn dry_run_computes_but_writes_no_verdicts_and_matches_the_real_run() {
        // H5: `--dry-run` suppresses ONLY verdict insertion. A dry run and
        // the real run that follows it must produce IDENTICAL verdict
        // computations, and the dry run must leave zero verdict rows.
        let repo = TestRepo::new();
        repo.write("lib.rs", &foo_src("1"));
        let c1 = repo.commit("c1");
        repo.write("lib.rs", &foo_src("2"));
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
        repo.write("lib.rs", &foo_src("1"));
        let c1 = repo.commit("c1");

        repo.checkout_new("feature");
        repo.write("lib.rs", &foo_src("feature-branch"));
        let c_feature = repo.commit("c_feature");

        repo.checkout("main");
        repo.write("lib.rs", &foo_src("main-branch"));
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
        repo.write("lib.rs", &foo_src("A"));
        let c1 = repo.commit("c1");
        repo.write("lib.rs", &foo_src("B"));
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
        repo.write("lib.rs", &foo_src("A"));
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
        repo.write("lib.rs", &foo_src("1"));
        let c1 = repo.commit("c1");
        repo.write("lib.rs", "// foo removed\n");
        let _c2 = repo.commit("c2 (foo gone)"); // live HEAD — deliberately NOT in the ledger.

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
        repo.write("lib.rs", &foo_src("1"));
        let c1 = repo.commit("c1");
        repo.write("lib.rs", &foo_src("2"));
        let c2 = repo.commit("c2");
        repo.write("lib.rs", &foo_src("3"));
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
        repo.write("lib.rs", &foo_src("1"));
        let c1 = repo.commit("c1");

        repo.checkout_new("feature");
        repo.write("lib.rs", &foo_src("2"));
        let c_feat = repo.commit("c_feat");

        repo.checkout("main");
        repo.write("lib.rs", &foo_src("2")); // same content as the feature branch.
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
    fn checkout_of_matching_older_head_does_not_reinstate() {
        // Forward-only judgment: c1 (content A) -> c2 (content B). Dream at
        // HEAD=c2 marks the c1 witness superseded. Then c1 itself is checked
        // out (HEAD moves BACKWARDS). The c1 witness's stamp equals the HEAD
        // stamp, but HEAD is not a descendant of where the superseded
        // verdict was observed (c2), so the run abstains: no reinstatement,
        // the verdict stays as drawn.
        let repo = TestRepo::new();
        repo.write("lib.rs", &foo_src("A"));
        let c1 = repo.commit("c1");
        repo.write("lib.rs", &foo_src("B"));
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
        assert_eq!(second.reinstated, 0, "{second:?}");
        assert_eq!(
            second.head_not_forward, 1,
            "the c1 reinstatement is withheld: HEAD is behind the observed HEAD: {second:?}"
        );
        assert_eq!(second.events_written, 0);

        let c1_id = storage
            .witnesses_for_file("proj", &file)
            .unwrap()
            .into_iter()
            .find(|r| r.at_oid.as_deref() == Some(c1.as_str()))
            .unwrap()
            .id;
        let verdict = storage.latest_witness_verdict(c1_id).unwrap().unwrap();
        assert_eq!(verdict.verdict, VerdictKind::SupersededBy);
        assert_eq!(verdict.receipt_oid, Some(c2));
    }

    #[test]
    fn branch_checkout_does_not_flap_obsolete_into_reinstated() {
        // c0 -> c1 on main (symbol present, content A). `dev` branches off
        // c1 and deletes the symbol (c2). Dreaming on `dev` marks the
        // witnesses obsolete. Checking out `main` (symbol present with the
        // identical stamp) must NOT reinstate: main's HEAD (c1) is an
        // ancestor of the observed HEAD (c2), not a descendant.
        let repo = TestRepo::new();
        repo.write("lib.rs", &foo_src("0"));
        let c0 = repo.commit("c0");
        repo.write("lib.rs", &foo_src("A"));
        let c1 = repo.commit("c1");
        repo.checkout_new("dev");
        repo.write("lib.rs", "// foo removed\n");
        let _c2 = repo.commit("c2 (symbol deleted)");

        let storage = open_storage();
        let file = repo.file_path();
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c0, "b3:0"))?;
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:A"))
            })
            .unwrap();

        let on_dev = run_join(&storage, false);
        assert_eq!(on_dev.obsolete, 2, "{on_dev:?}");
        assert_eq!(on_dev.events_written, 2);

        repo.checkout("main");
        let on_main = run_join(&storage, false);
        assert_eq!(on_main.reinstated, 0, "{on_main:?}");
        assert!(on_main.head_not_forward >= 1, "{on_main:?}");
        assert_eq!(on_main.events_written, 0, "{on_main:?}");

        let c1_id = storage
            .witnesses_for_file("proj", &file)
            .unwrap()
            .into_iter()
            .find(|r| r.at_oid.as_deref() == Some(c1.as_str()))
            .unwrap()
            .id;
        assert_eq!(
            storage
                .latest_witness_verdict(c1_id)
                .unwrap()
                .unwrap()
                .verdict,
            VerdictKind::AnchorObsolete
        );
    }

    #[test]
    fn restore_in_a_descendant_commit_reinstates() {
        // c0 -> c1 (content A) -> c2 (symbol deleted) -> c3 (A restored,
        // identical stamp). Dream at c2: obsolete. Dream at c3, a
        // descendant of the observed HEAD c2: anchor_reinstated fires.
        let repo = TestRepo::new();
        repo.write("lib.rs", &foo_src("0"));
        let c0 = repo.commit("c0");
        repo.write("lib.rs", &foo_src("A"));
        let c1 = repo.commit("c1");
        repo.write("lib.rs", "// foo removed\n");
        let _c2 = repo.commit("c2 (symbol deleted)");

        let storage = open_storage();
        let file = repo.file_path();
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c0, "b3:0"))?;
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:A"))
            })
            .unwrap();
        let first = run_join(&storage, false);
        assert_eq!(first.obsolete, 2, "{first:?}");

        repo.write("lib.rs", &foo_src("A"));
        let c3 = repo.commit("c3 (restored)");
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c3, "b3:A"))
            })
            .unwrap();
        let second = run_join(&storage, false);
        assert_eq!(second.reinstated, 1, "{second:?}");
        assert_eq!(second.head_not_forward, 0, "{second:?}");

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
    fn unparsable_observed_head_abstains_as_head_not_forward() {
        let repo = TestRepo::new();
        repo.write("lib.rs", &foo_src("1"));
        let c1 = repo.commit("c1");
        repo.write("lib.rs", &foo_src("2"));
        let c2 = repo.commit("c2");

        let storage = open_storage();
        let file = repo.file_path();
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:1"))?;
                witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c2, "b3:2"))
            })
            .unwrap();
        let w1_id = storage
            .witnesses_for_file("proj", &file)
            .unwrap()
            .into_iter()
            .find(|r| r.at_oid.as_deref() == Some(c1.as_str()))
            .unwrap()
            .id;
        // A prior (obsolete) event whose observed HEAD is not an oid.
        storage
            .with_connection(|conn| {
                witness_verdicts::insert_verdict_if_changed(
                    conn,
                    &WitnessVerdictRow {
                        witness_id: w1_id,
                        verdict: VerdictKind::AnchorObsolete,
                        successor_witness_id: None,
                        receipt_oid: None,
                        observed_head_oid: "not-an-oid".into(),
                    },
                )
            })
            .unwrap();
        let stats = run_join(&storage, false);
        assert_eq!(stats.head_not_forward, 1, "{stats:?}");
        assert_eq!(stats.events_written, 0);
    }

    #[test]
    fn head_row_under_another_project_keeps_same_content_witness_intact() {
        // The stamp pass assigns one project per file, so the same absolute
        // file+symbol is stamped under P1 at c1 and later under P2 at c2
        // (HEAD). Anchor identity is (file, symbol): with identical content
        // the P1 witness is intact, never `anchor_obsolete`.
        let repo = TestRepo::new();
        repo.write("lib.rs", &foo_src("A"));
        let c1 = repo.commit("c1");
        repo.write("lib.rs", &format!("{}// trailer\n", foo_src("A")));
        let c2 = repo.commit("c2");

        let storage = open_storage();
        let file = repo.file_path();
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(conn, &ledger_row("p1", &file, &c1, "b3:A"))?;
                witness_ledger::insert_witness(conn, &ledger_row("p2", &file, &c2, "b3:A"))
            })
            .unwrap();

        let stats = run_join(&storage, false);
        assert_eq!(stats.anchors_considered, 1, "one (file, symbol) anchor");
        assert_eq!(stats.obsolete, 0, "{stats:?}");
        assert_eq!(stats.superseded, 0, "{stats:?}");
        assert_eq!(stats.intact, 1, "{stats:?}");
        assert_eq!(stats.events_written, 0);
    }

    #[test]
    fn head_row_under_another_project_supersedes_changed_content() {
        let repo = TestRepo::new();
        repo.write("lib.rs", &foo_src("A"));
        let c1 = repo.commit("c1");
        repo.write("lib.rs", &foo_src("B"));
        let c2 = repo.commit("c2");

        let storage = open_storage();
        let file = repo.file_path();
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(conn, &ledger_row("p1", &file, &c1, "b3:A"))?;
                witness_ledger::insert_witness(conn, &ledger_row("p2", &file, &c2, "b3:B"))
            })
            .unwrap();

        let stats = run_join(&storage, false);
        assert_eq!(stats.obsolete, 0, "{stats:?}");
        assert_eq!(stats.superseded, 1, "{stats:?}");

        let p1_id = storage
            .witnesses_for_file("p1", &file)
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
            .id;
        let p2_id = storage
            .witnesses_for_file("p2", &file)
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
            .id;
        let verdict = storage.latest_witness_verdict(p1_id).unwrap().unwrap();
        assert_eq!(verdict.verdict, VerdictKind::SupersededBy);
        assert_eq!(verdict.successor_witness_id, Some(p2_id));
        assert_eq!(verdict.receipt_oid, Some(c2));
    }

    #[test]
    fn near_revert_supersedes_rather_than_abstaining() {
        // C3 contract: A -> B -> A' (A' close to but NOT stamp-equal to A)
        // is legitimate supersession — content demonstrably changed and a
        // valid HEAD-path successor exists. Near-revert abstention applies
        // ONLY to reinstatement (exact stamp equality), never here.
        let repo = TestRepo::new();
        repo.write("lib.rs", &foo_src("A"));
        let c1 = repo.commit("c1");
        repo.write("lib.rs", &foo_src("B"));
        let c2 = repo.commit("c2");
        repo.write("lib.rs", &foo_src("A2")); // A-prime: almost, but not exactly, A.
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
    // ---- true receipts (item 2) -------------------------------------------

    /// `foo` plus an unrelated sibling, so "unrelated edit" commits can
    /// touch the same file without touching `foo`.
    fn foo_with(body: &str, tail: &str) -> String {
        format!("{}{tail}", foo_src(body))
    }

    fn insert_rows(storage: &Storage, file: &str, rows: &[(&str, &str)]) {
        storage
            .with_connection(|conn| {
                for (oid, stamp) in rows {
                    witness_ledger::insert_witness(conn, &ledger_row("proj", file, oid, stamp))?;
                }
                Ok(())
            })
            .unwrap();
    }

    fn witness_at(storage: &Storage, file: &str, oid: &str) -> i64 {
        storage
            .witnesses_for_file("proj", file)
            .unwrap()
            .into_iter()
            .find(|r| r.at_oid.as_deref() == Some(oid))
            .unwrap()
            .id
    }

    fn latest(storage: &Storage, file: &str, oid: &str) -> Option<WitnessVerdictRow> {
        storage
            .latest_witness_verdict(witness_at(storage, file, oid))
            .unwrap()
    }

    #[test]
    fn obsolete_receipt_is_the_commit_that_deleted_the_symbol() {
        let repo = TestRepo::new();
        repo.write("lib.rs", &foo_src("1"));
        let c1 = repo.commit("c1 adds foo");
        repo.write("lib.rs", &foo_with("1", "fn other() {}\n"));
        let c2 = repo.commit("c2 unrelated edit, same file");
        repo.write("lib.rs", "fn other() {}\n");
        let c3 = repo.commit("c3 deletes foo");
        repo.write("lib.rs", "fn other() {}\nfn another() {}\n");
        let c4 = repo.commit("c4 unrelated");

        let storage = open_storage();
        let file = repo.file_path();
        insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c2, "b3:1")]);

        let stats = run_join(&storage, false);
        assert_eq!(stats.obsolete, 2, "{stats:?}");
        assert_eq!(stats.receipts_found, 2, "{stats:?}");
        assert_eq!(stats.events_written, 2, "{stats:?}");
        for oid in [&c1, &c2] {
            let v = latest(&storage, &file, oid).unwrap();
            assert_eq!(v.verdict, VerdictKind::AnchorObsolete);
            assert_eq!(v.receipt_oid.as_deref(), Some(c3.as_str()), "not HEAD {c4}");
            assert_eq!(v.observed_head_oid, c4);
        }
    }

    #[test]
    fn superseded_receipt_is_the_commit_that_changed_the_symbol() {
        let repo = TestRepo::new();
        repo.write("lib.rs", &foo_src("1"));
        let c1 = repo.commit("c1");
        repo.write("lib.rs", &foo_src("2"));
        let c2 = repo.commit("c2 changes foo");
        repo.write("lib.rs", &foo_with("2", "fn other() {}\n"));
        let c3 = repo.commit("c3 unrelated");

        let storage = open_storage();
        let file = repo.file_path();
        insert_rows(
            &storage,
            &file,
            &[(&c1, "b3:1"), (&c2, "b3:2"), (&c3, "b3:2")],
        );

        let stats = run_join(&storage, false);
        assert_eq!(stats.superseded, 1, "{stats:?}");
        assert_eq!(stats.intact, 1, "{stats:?}");
        let v = latest(&storage, &file, &c1).unwrap();
        assert_eq!(v.verdict, VerdictKind::SupersededBy);
        assert_eq!(v.receipt_oid.as_deref(), Some(c2.as_str()));
        assert_eq!(
            v.successor_witness_id,
            Some(witness_at(&storage, &file, &c2))
        );
    }

    #[test]
    fn whole_file_witness_receipt_is_the_commit_that_changed_the_file() {
        let repo = TestRepo::new();
        repo.write("lib.rs", "one\n");
        let c1 = repo.commit("c1");
        repo.write("lib.rs", "two\n");
        let c2 = repo.commit("c2");
        repo.write("lib.rs", "two\n\n");
        let c3 = repo.commit("c3 blank line only");

        let storage = open_storage();
        let file = repo.file_path();
        storage
            .with_connection(|conn| {
                for (oid, stamp) in [(&c1, "b3:1"), (&c2, "b3:2"), (&c3, "b3:3")] {
                    let mut row = ledger_row("proj", &file, oid, stamp);
                    row.symbol = None;
                    row.span_start = None;
                    row.span_end = None;
                    witness_ledger::insert_witness(conn, &row)?;
                }
                Ok(())
            })
            .unwrap();

        let stats = run_join(&storage, false);
        // c1 -> c3: content changed, receipt c2. c2 -> c3: whitespace only.
        assert_eq!(stats.superseded, 1, "{stats:?}");
        assert_eq!(stats.abstained_same_content, 1, "{stats:?}");
        let v = latest(&storage, &file, &c1).unwrap();
        assert_eq!(v.receipt_oid.as_deref(), Some(c2.as_str()));
        assert!(latest(&storage, &file, &c2).is_none());
    }

    #[test]
    fn whitespace_only_reformat_is_not_a_supersession() {
        let repo = TestRepo::new();
        repo.write("lib.rs", &foo_src("1"));
        let c1 = repo.commit("c1");
        repo.write("lib.rs", "fn foo() {\n\t1   \n}\n\n");
        let c2 = repo.commit("c2 reformat only");

        let storage = open_storage();
        let file = repo.file_path();
        // Raw stamps differ (they are raw), the normalized content does not.
        insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c2, "b3:2")]);

        let stats = run_join(&storage, false);
        assert_eq!(stats.superseded, 0, "{stats:?}");
        assert_eq!(stats.abstained_same_content, 1, "{stats:?}");
        assert_eq!(stats.events_written, 0, "{stats:?}");
        assert!(latest(&storage, &file, &c1).is_none());
    }

    #[test]
    fn reinstated_receipt_is_the_commit_that_restored_the_symbol() {
        let repo = TestRepo::new();
        repo.write("lib.rs", &foo_src("1"));
        let c1 = repo.commit("c1 foo");
        repo.write("lib.rs", &foo_with("1", "// pad\n"));
        let c1b = repo.commit("c1b unrelated");
        repo.write("lib.rs", "// gone\n");
        let c2 = repo.commit("c2 deletes foo");

        let storage = open_storage();
        let file = repo.file_path();
        insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c1b, "b3:1")]);
        let first = run_join(&storage, false);
        assert_eq!(first.obsolete, 2, "{first:?}");
        assert_eq!(
            latest(&storage, &file, &c1).unwrap().receipt_oid.as_deref(),
            Some(c2.as_str())
        );

        repo.write("lib.rs", "// gone\n// still gone\n");
        let _c3 = repo.commit("c3 unrelated");
        repo.write("lib.rs", &foo_with("1", "// back\n"));
        let c4 = repo.commit("c4 restores identical foo");
        repo.write("lib.rs", &foo_with("1", "// back\n// later\n"));
        let c5 = repo.commit("c5 unrelated");
        insert_rows(&storage, &file, &[(&c5, "b3:1")]);

        let second = run_join(&storage, false);
        assert_eq!(second.reinstated, 2, "{second:?}");
        assert_eq!(second.receipts_found, 2, "{second:?}");
        for oid in [&c1, &c1b] {
            let v = latest(&storage, &file, oid).unwrap();
            assert_eq!(v.verdict, VerdictKind::AnchorReinstated);
            assert_eq!(v.receipt_oid.as_deref(), Some(c4.as_str()), "not HEAD {c5}");
            assert_eq!(v.observed_head_oid, c5);
        }
    }

    #[test]
    fn symbol_present_at_head_without_a_head_row_is_not_obsolete() {
        let repo = TestRepo::new();
        repo.write("lib.rs", &foo_src("1"));
        let c1 = repo.commit("c1");
        repo.write("lib.rs", &foo_with("1", "// pad\n"));
        let c2 = repo.commit("c2");
        repo.write("lib.rs", &foo_with("1", "// pad\n// more\n"));
        let _c3 = repo.commit("c3 (HEAD, no ledger row)");

        let storage = open_storage();
        let file = repo.file_path();
        insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c2, "b3:1")]);

        let stats = run_join(&storage, false);
        assert_eq!(stats.obsolete, 0, "{stats:?}");
        assert_eq!(stats.abstained_present_at_head, 2, "{stats:?}");
        assert_eq!(stats.events_written, 0, "{stats:?}");
        assert!(latest(&storage, &file, &c1).is_none());
    }

    #[test]
    fn unparseable_content_abstains_instead_of_guessing() {
        // "1" / "2" are not a parseable Rust symbol anywhere: state at the
        // witness's own commit is Unknown, so no event, no HEAD fallback.
        let repo = TestRepo::new();
        repo.write("lib.rs", "1");
        let c1 = repo.commit("c1");
        repo.write("lib.rs", "2");
        let c2 = repo.commit("c2");
        let storage = open_storage();
        let file = repo.file_path();
        insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c2, "b3:2")]);

        let stats = run_join(&storage, false);
        assert_eq!(stats.superseded, 0, "{stats:?}");
        assert_eq!(stats.abstained_unparseable, 1, "{stats:?}");
        assert_eq!(stats.abstained_not_at_witness, 0, "{stats:?}");
        assert_eq!(stats.events_written, 0, "{stats:?}");
    }

    #[test]
    fn unknown_state_inside_the_range_abstains_with_no_receipt() {
        let repo = TestRepo::new();
        repo.write("lib.rs", &foo_src("1"));
        let c1 = repo.commit("c1");
        repo.write("lib.rs", &foo_with("1", "// pad\n"));
        let c1b = repo.commit("c1b");
        repo.write("lib.rs", "fn foo( {{{ ]\nlet = ;\n");
        let _c2 = repo.commit("c2 syntax error");
        repo.write("lib.rs", "// gone\n");
        let _c3 = repo.commit("c3 foo gone");

        let storage = open_storage();
        let file = repo.file_path();
        insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c1b, "b3:1")]);

        let stats = run_join(&storage, false);
        assert_eq!(stats.obsolete, 0, "{stats:?}");
        assert_eq!(stats.abstained_no_receipt, 2, "{stats:?}");
        assert_eq!(stats.events_written, 0, "{stats:?}");
        assert_eq!(stats.receipts_found, 0, "{stats:?}");
    }

    #[test]
    fn rerun_at_a_newer_head_runs_no_receipt_search_and_writes_nothing() {
        let repo = TestRepo::new();
        repo.write("lib.rs", &foo_src("1"));
        let c1 = repo.commit("c1");
        repo.write("lib.rs", &foo_src("2"));
        let c2 = repo.commit("c2");

        let storage = open_storage();
        let file = repo.file_path();
        insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c2, "b3:2")]);
        let first = run_join(&storage, false);
        assert_eq!(first.events_written, 1, "{first:?}");
        assert!(first.receipt_searches >= 1, "{first:?}");

        repo.write("lib.rs", &foo_with("2", "fn other() {}\n"));
        let c3 = repo.commit("c3 unrelated");
        insert_rows(&storage, &file, &[(&c3, "b3:2")]);

        let second = run_join(&storage, false);
        assert_eq!(second.superseded, 1, "same conclusion: {second:?}");
        assert_eq!(second.events_deduped, 1, "{second:?}");
        assert_eq!(second.events_written, 0, "{second:?}");
        assert_eq!(second.receipt_searches, 0, "no git search: {second:?}");
        assert_eq!(second.receipts_found, 0, "{second:?}");
        let n: i64 = storage
            .with_connection(|conn| {
                Ok(conn.query_row("SELECT COUNT(*) FROM witness_verdicts", [], |r| r.get(0))?)
            })
            .unwrap();
        assert_eq!(n, 1);
        // The recorded receipt is still the changing commit, untouched.
        assert_eq!(
            latest(&storage, &file, &c1).unwrap().receipt_oid.as_deref(),
            Some(c2.as_str())
        );
    }

    fn insert_symbol_rows(storage: &Storage, file: &str, symbol: &str, rows: &[(&str, &str)]) {
        storage
            .with_connection(|conn| {
                for (oid, stamp) in rows {
                    let mut row = ledger_row("proj", file, oid, stamp);
                    row.symbol = Some(symbol.into());
                    witness_ledger::insert_witness(conn, &row)?;
                }
                Ok(())
            })
            .unwrap();
    }

    fn witness_of(storage: &Storage, file: &str, symbol: &str, oid: &str) -> i64 {
        storage
            .witnesses_for_file("proj", file)
            .unwrap()
            .into_iter()
            .find(|r| r.at_oid.as_deref() == Some(oid) && r.symbol.as_deref() == Some(symbol))
            .unwrap()
            .id
    }

    #[test]
    fn positional_symbol_gets_no_verdict_but_a_plain_sibling_still_does() {
        // Two `foo` definitions: the second is minted as `foo#2`. Both change
        // between c1 and c2, but only the plain-named one may be judged.
        let repo = TestRepo::new();
        repo.write("lib.rs", "fn foo() {\n    1\n}\nfn foo() {\n    a\n}\n");
        let c1 = repo.commit("c1");
        repo.write("lib.rs", "fn foo() {\n    2\n}\nfn foo() {\n    b\n}\n");
        let c2 = repo.commit("c2");

        let storage = open_storage();
        let file = repo.file_path();
        insert_symbol_rows(&storage, &file, "foo", &[(&c1, "b3:1"), (&c2, "b3:2")]);
        insert_symbol_rows(&storage, &file, "foo#2", &[(&c1, "b3:a"), (&c2, "b3:b")]);

        let stats = run_join(&storage, false);
        assert_eq!(stats.abstained_positional_symbol, 1, "{stats:?}");
        assert_eq!(stats.superseded, 1, "{stats:?}");
        assert_eq!(stats.events_written, 1, "{stats:?}");

        let plain = storage
            .latest_witness_verdict(witness_of(&storage, &file, "foo", &c1))
            .unwrap()
            .expect("plain sibling is still judged");
        assert_eq!(plain.verdict, VerdictKind::SupersededBy);
        assert_eq!(plain.receipt_oid.as_deref(), Some(c2.as_str()));
        assert!(
            storage
                .latest_witness_verdict(witness_of(&storage, &file, "foo#2", &c1))
                .unwrap()
                .is_none(),
            "positional symbol must carry no verdict"
        );
        assert!(storage
            .latest_witness_verdict(witness_of(&storage, &file, "foo#2", &c2))
            .unwrap()
            .is_none());
    }

    /// A `Foo::render` witness whose method moved to a top-level `render`.
    const NESTED_RENDER: &str = "struct Foo;\nimpl Foo {\n    fn render() {\n        1\n    }\n}\n";

    fn render_rows(storage: &Storage, file: &str, oids: &[&str]) {
        let rows: Vec<(&str, &str)> = oids.iter().map(|o| (*o, "b3:1")).collect();
        insert_symbol_rows(storage, file, "Foo::render", &rows);
    }

    #[test]
    fn moved_to_top_level_is_not_obsolete_when_the_leaf_survives_at_head() {
        let repo = TestRepo::new();
        repo.write("lib.rs", NESTED_RENDER);
        let c1 = repo.commit("c1 Foo::render");
        repo.write("lib.rs", &format!("{NESTED_RENDER}// pad\n"));
        let c1b = repo.commit("c1b unrelated");
        repo.write("lib.rs", "fn render() {\n    1\n}\n");
        let _c2 = repo.commit("c2 render moved out of impl Foo");

        let storage = open_storage();
        let file = repo.file_path();
        render_rows(&storage, &file, &[&c1, &c1b]);

        let stats = run_join(&storage, false);
        assert_eq!(stats.obsolete, 0, "{stats:?}");
        assert_eq!(stats.abstained_moved, 2, "{stats:?}");
        assert_eq!(stats.events_written, 0, "{stats:?}");
        assert!(storage
            .latest_witness_verdict(witness_of(&storage, &file, "Foo::render", &c1))
            .unwrap()
            .is_none());
    }

    #[test]
    fn moved_then_deleted_is_not_obsolete_when_the_leaf_survives_at_the_receipt() {
        let repo = TestRepo::new();
        repo.write("lib.rs", NESTED_RENDER);
        let c1 = repo.commit("c1 Foo::render");
        repo.write("lib.rs", &format!("{NESTED_RENDER}// pad\n"));
        let c1b = repo.commit("c1b unrelated");
        repo.write("lib.rs", "fn render() {\n    1\n}\n");
        let _c2 = repo.commit("c2 moved: Foo::render absent, render present");
        repo.write("lib.rs", "fn other() {}\n");
        let _c3 = repo.commit("c3 render deleted (HEAD)");

        let storage = open_storage();
        let file = repo.file_path();
        render_rows(&storage, &file, &[&c1, &c1b]);

        let stats = run_join(&storage, false);
        assert_eq!(stats.obsolete, 0, "{stats:?}");
        assert_eq!(stats.abstained_moved, 2, "{stats:?}");
        assert_eq!(stats.events_written, 0, "{stats:?}");
    }

    #[test]
    fn real_deletion_of_a_qualified_symbol_is_still_obsolete_with_the_deleting_commit() {
        let repo = TestRepo::new();
        repo.write("lib.rs", NESTED_RENDER);
        let c1 = repo.commit("c1 Foo::render");
        repo.write("lib.rs", &format!("{NESTED_RENDER}// pad\n"));
        let c1b = repo.commit("c1b unrelated");
        repo.write("lib.rs", "struct Foo;\nfn other() {}\n");
        let c2 = repo.commit("c2 deletes render for good");
        repo.write("lib.rs", "struct Foo;\nfn other() {}\nfn another() {}\n");
        let _c3 = repo.commit("c3 unrelated (HEAD)");

        let storage = open_storage();
        let file = repo.file_path();
        render_rows(&storage, &file, &[&c1, &c1b]);

        let stats = run_join(&storage, false);
        assert_eq!(stats.abstained_moved, 0, "{stats:?}");
        assert_eq!(stats.obsolete, 2, "{stats:?}");
        let v = storage
            .latest_witness_verdict(witness_of(&storage, &file, "Foo::render", &c1))
            .unwrap()
            .unwrap();
        assert_eq!(v.verdict, VerdictKind::AnchorObsolete);
        assert_eq!(v.receipt_oid.as_deref(), Some(c2.as_str()));
    }

    #[test]
    fn symbol_leaf_strips_collision_suffix_and_container() {
        use super::receipt::symbol_leaf;
        assert_eq!(symbol_leaf("make_renderer.render"), "render");
        assert_eq!(symbol_leaf("Foo::bar#2"), "bar");
        assert_eq!(symbol_leaf("remove#10"), "remove");
        assert_eq!(symbol_leaf("out"), "out");
        assert_eq!(symbol_leaf("A.B::c"), "c");
        assert_eq!(symbol_leaf("weird#x"), "weird#x");
    }

    #[test]
    fn symbol_missing_at_its_own_commit_is_not_at_witness_not_unparseable() {
        // Clean parse at both commits, but the file never contained `foo`:
        // a legacy backfill row claiming a symbol at a commit without it.
        let repo = TestRepo::new();
        repo.write("lib.rs", "fn bar() {\n    1\n}\n");
        let c1 = repo.commit("c1 only bar");
        repo.write("lib.rs", "fn bar() {\n    2\n}\n");
        let c2 = repo.commit("c2 only bar");
        let storage = open_storage();
        let file = repo.file_path();
        insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c2, "b3:2")]);

        let stats = run_join(&storage, false);
        assert_eq!(stats.abstained_not_at_witness, 1, "{stats:?}");
        assert_eq!(stats.abstained_unparseable, 0, "{stats:?}");
        assert_eq!(stats.events_written, 0, "{stats:?}");
        assert_eq!(stats.superseded, 0, "{stats:?}");
        let text = stats.format_text(false);
        assert!(text.contains("unparseable=0"), "{text}");
        assert!(text.contains("not_at_witness=1"), "{text}");
        assert!(text.contains("moved=0"), "{text}");
        assert!(text.contains("positional_symbol=0"), "{text}");
    }

    #[test]
    fn rechange_after_a_reinstatement_does_not_reuse_the_cancelled_receipt() {
        // A at C1, B at C2 (superseded, receipt C2), A restored at C3
        // (reinstated), B again at C4. The second supersession of the C1
        // witness is C4's work; the cancelled C2 row and commit must not be
        // picked up again.
        let repo = TestRepo::new();
        repo.write("lib.rs", &foo_src("1"));
        let c1 = repo.commit("c1 A");
        let storage = open_storage();
        let file = repo.file_path();
        insert_rows(&storage, &file, &[(&c1, "b3:1")]);
        run_join(&storage, false);

        repo.write("lib.rs", &foo_src("2"));
        let c2 = repo.commit("c2 B");
        insert_rows(&storage, &file, &[(&c2, "b3:2")]);
        run_join(&storage, false);
        let first = latest(&storage, &file, &c1).unwrap();
        assert_eq!(first.verdict, VerdictKind::SupersededBy);
        assert_eq!(first.receipt_oid.as_deref(), Some(c2.as_str()));

        repo.write("lib.rs", &foo_src("1"));
        let c3 = repo.commit("c3 A restored");
        insert_rows(&storage, &file, &[(&c3, "b3:1")]);
        run_join(&storage, false);
        let restored = latest(&storage, &file, &c1).unwrap();
        assert_eq!(restored.verdict, VerdictKind::AnchorReinstated);
        assert_eq!(restored.receipt_oid.as_deref(), Some(c3.as_str()));

        repo.write("lib.rs", &foo_src("2"));
        let c4 = repo.commit("c4 B again");
        insert_rows(&storage, &file, &[(&c4, "b3:2")]);
        let stats = run_join(&storage, false);
        assert_eq!(stats.abstained_no_receipt, 0, "{stats:?}");

        let second = latest(&storage, &file, &c1).unwrap();
        assert_eq!(second.verdict, VerdictKind::SupersededBy);
        assert_eq!(
            second.receipt_oid.as_deref(),
            Some(c4.as_str()),
            "second supersession must be C4, not the cancelled C2 {c2}"
        );
        assert_eq!(
            second.successor_witness_id,
            Some(witness_at(&storage, &file, &c4)),
            "successor must be the post-reinstatement row"
        );
    }

    #[test]
    fn definitions_sharing_a_line_abstain_instead_of_sharing_a_stamp() {
        // `foo` and `bar` live on one line, so a change to `bar` changes
        // `foo`'s line-span stamp. `foo` itself never changed.
        let repo = TestRepo::new();
        repo.write(
            "lib.rs",
            "fn foo() -> i32 { 1 } fn bar() -> i32 { 2 }\nfn other() {}\n",
        );
        let c1 = repo.commit("c1");
        repo.write(
            "lib.rs",
            "fn foo() -> i32 { 1 } fn bar() -> i32 { 3 }\nfn other() {}\n",
        );
        let c2 = repo.commit("c2 changes bar only");

        let storage = open_storage();
        let file = repo.file_path();
        insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c2, "b3:2")]);

        let stats = run_join(&storage, false);
        assert_eq!(stats.superseded, 0, "{stats:?}");
        assert_eq!(stats.events_written, 0, "{stats:?}");
        assert_eq!(stats.abstained_unparseable, 1, "{stats:?}");
        assert!(latest(&storage, &file, &c1).is_none());
    }

    #[test]
    fn a_class_with_a_method_changed_on_its_own_line_still_supersedes_the_class() {
        // Strict nesting is not an overlap: the class legitimately contains
        // the method's lines, so the method edit is a class edit.
        let repo = TestRepo::new();
        repo.write(
            "mod.py",
            "class Foo:\n    def bar(self):\n        return 1\n",
        );
        let c1 = repo.commit("c1");
        repo.write(
            "mod.py",
            "class Foo:\n    def bar(self):\n        return 2\n",
        );
        let c2 = repo.commit("c2 changes the method body");

        let storage = open_storage();
        let file = repo.path.join("mod.py").to_string_lossy().to_string();
        storage
            .with_connection(|conn| {
                for (oid, stamp) in [(&c1, "b3:1"), (&c2, "b3:2")] {
                    let mut row = ledger_row("proj", &file, oid, stamp);
                    row.symbol = Some("Foo".into());
                    witness_ledger::insert_witness(conn, &row)?;
                }
                Ok(())
            })
            .unwrap();

        let stats = run_join(&storage, false);
        assert_eq!(stats.superseded, 1, "{stats:?}");
        assert_eq!(stats.abstained_unparseable, 0, "{stats:?}");
        let v = latest(&storage, &file, &c1).unwrap();
        assert_eq!(v.verdict, VerdictKind::SupersededBy);
        assert_eq!(v.receipt_oid.as_deref(), Some(c2.as_str()));
    }
}
