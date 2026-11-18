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
//!    - `superseded_by`: the symbol is PRESENT at the successor's commit
//!      (absent -> `abstained_successor_absent`) with normalized span
//!      content that differs from `W`'s (equal -> `abstained_same_content`);
//!      the receipt is the first commit in `W..W2.at_oid` at which the
//!      symbol is present with content that differs from `W`'s.
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
pub(crate) mod change_summary;
pub mod cli;
pub mod policy;
pub(crate) mod receipt;
pub mod report;
pub(crate) mod strategy;
pub mod symbol_cli;
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
    /// `superseded_by` withheld: the successor witness's commit does not
    /// contain the symbol, so it cannot be what replaced the witness.
    pub abstained_successor_absent: usize,
    /// `anchor_obsolete` withheld: the qualified symbol is gone but the file
    /// still has a symbol with the same leaf name (at the receipt commit or
    /// at HEAD) - the symbol moved or was re-nested, not deleted.
    pub abstained_moved: usize,
    /// A NEW conclusion was withheld because no commit could be named as
    /// the one that made the change (empty or oversized history range, git
    /// failure, or an unknown state met first). Never falls back to HEAD.
    pub abstained_no_receipt: usize,
    /// A NEW conclusion was withheld because the first commit that matched is
    /// a merge whose first-parent diff leaves the span unchanged (it did not
    /// author the change) and the authoring commit could not be established.
    /// Never falls back to the merge.
    pub abstained_merge_not_author: usize,
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
             receipt abstained: unparseable={}  not_at_witness={}  present_at_head={}  same_content={}  successor_absent={}  moved={}  no_receipt={}  merge_not_author={}\n\
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
            self.abstained_successor_absent,
            self.abstained_moved,
            self.abstained_no_receipt,
            self.abstained_merge_not_author,
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
    let exe = match crate::process::detached_child_exe() {
        Ok(Some(exe)) => exe,
        // A cargo test harness driving the hook in-process: not an engine.
        Ok(None) => return,
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
mod compact_gate_tests;

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
            // When the receipt commit was made (not when this run noticed).
            let mut receipt_committed_at: Option<String> = None;
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
                        receipt_committed_at = receipt::commit_time_local(auditor, &receipt_oid);
                        candidate.receipt_oid = Some(receipt_oid);
                        stats.receipts_found += 1;
                    }
                    Err(abstain) => {
                        match abstain {
                            receipt::Abstain::Unparseable => stats.abstained_unparseable += 1,
                            receipt::Abstain::NotAtWitness => stats.abstained_not_at_witness += 1,
                            receipt::Abstain::PresentAtHead => stats.abstained_present_at_head += 1,
                            receipt::Abstain::SameContent => stats.abstained_same_content += 1,
                            receipt::Abstain::SuccessorAbsent => {
                                stats.abstained_successor_absent += 1
                            }
                            receipt::Abstain::Moved => stats.abstained_moved += 1,
                            receipt::Abstain::NoReceipt => stats.abstained_no_receipt += 1,
                            receipt::Abstain::MergeNotAuthor => {
                                stats.abstained_merge_not_author += 1
                            }
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
                    witness_verdicts::insert_verdict_with_commit_time(
                        conn,
                        &candidate,
                        receipt_committed_at.as_deref(),
                    )?;
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
mod tests;
