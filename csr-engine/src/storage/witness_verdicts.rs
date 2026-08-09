//! Append-only witness *verdict* ledger (v10 "dreaming" — see `crate::dream`).
//!
//! Where `witness_ledger` stores raw evidence ("at commit X, symbol Y had
//! content Z"), this module stores the deterministic CONCLUSIONS the
//! `dream` successor join draws from that evidence: EVENTS, keyed to a
//! specific `witness_ledger` row (`witness_id`), each anchored to the git
//! HEAD the dream cycle that minted it observed.
//!
//! # Append-only invariant
//!
//! Same discipline as `witness_ledger`: this module exposes INSERT and QUERY
//! functions ONLY. A witness whose state changes (e.g. reverts from
//! `superseded_by` back to intact) gets a NEW `anchor_reinstated` event, never
//! a mutation of the old one — the full history of conclusions survives,
//! exactly like the evidence it is drawn from.
//!
//! # Idempotency (app-side ONLY)
//!
//! [`insert_verdict_if_changed`] is the only write entry point. It compares
//! the candidate event against the LATEST recorded event for that
//! `witness_id` (see [`is_new_event`]) and skips the insert iff the two are
//! identical in `(verdict, successor_witness_id, receipt_oid,
//! observed_head_oid)` — so re-running `dream` at an unchanged HEAD with an
//! unchanged conclusion adds nothing. There is deliberately NO UNIQUE
//! identity index backstop on the table: event history legitimately
//! re-visits earlier states (B -> A -> B — superseded, reinstated, then
//! superseded again with the exact same fields as the first event), and a
//! UNIQUE index would silently swallow that third event, freezing the
//! witness at "reinstated" forever. Only the latest event per witness
//! matters, so "skip iff identical to latest" is the whole contract.
//!
//! # Symbol-level current state (order-independent, two channels)
//!
//! [`symbol_verdict_state`] answers "what is the CURRENT state of this
//! `(project, file, symbol)` anchor?" for chunk binding
//! (`storage::chunk_binding`). The rule is deliberately NOT "the globally
//! latest inserted event is negative" — insertion order across different
//! witnesses of the same symbol carries no meaning. Latest-event-per-witness
//! is `ORDER BY id DESC` scoped to `witness_id` (id order IS meaningful
//! within one witness). The state resolves to one of:
//!
//! - **Nothing** (`None`): no witness of the symbol has a negative LATEST
//!   event (`anchor_obsolete` or `superseded_by`, not cancelled by a later
//!   `anchor_reinstated` FOR THAT WITNESS). Unaudited or fully reinstated —
//!   no consumer action.
//! - **[`VerdictChannel::Demote`]**: at least one negative-latest witness
//!   AND no witness of the symbol has a stamp equal to the current HEAD
//!   stamp recorded in the ledger (i.e. no witness is intact at the most
//!   recent `observed_head_oid` — the one carried by the globally newest
//!   event for the symbol). The symbol is truly gone or fully stale at
//!   current truth. Rank-affecting.
//! - **[`VerdictChannel::Annotate`]**: at least one negative-latest witness
//!   AND at least one witness intact at the observed HEAD — the plain
//!   A -> B evolution. NO rank effect; consumers annotate the chunk with
//!   "symbol evolved since earlier evidence; current as of `receipt_oid`",
//!   where `receipt_oid` comes from the most recent negative latest-event
//!   for the symbol.
//!
//! Why Annotate never demotes (the deliberate v10 contract): symbol-level
//! binding cannot attribute staleness to individual chunk COHORTS — chunks
//! from the A era and chunks from the B era share the same
//! `(conversation -> symbol)` binding, so demoting on evolution would punish
//! current-truth chunks alongside stale ones. v10 therefore annotates with
//! the receipt and lets the reader decide.
//!
//! The A -> B -> A revert case yields NOT-demoted regardless of event
//! insertion order: either the reinstatement is the witness's latest event,
//! or a witness intact at HEAD exists. With the full event history (B's own
//! witness stays negative-latest while the A witnesses are intact at HEAD)
//! the state is Annotate — "the symbol carries history of a rejected
//! change" — never Demote.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;

/// The three deterministic conclusions the `dream` successor join can draw
/// about a witness. There is no "unknown" variant — every witness with more
/// than one committed-tier ledger row either gets no event (still intact) or
/// exactly one of these, derived from content hashes and git ancestry alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerdictKind {
    /// The witnessed anchor no longer exists at the observed HEAD (the
    /// symbol's file/span could not be re-stamped there).
    AnchorObsolete,
    /// A witness that previously carried a negative verdict now matches the
    /// content at the observed HEAD again (the A -> B -> A revert case).
    AnchorReinstated,
    /// A specific, later, causally-descendant committed witness replaced
    /// this one.
    SupersededBy,
}

impl VerdictKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            VerdictKind::AnchorObsolete => "anchor_obsolete",
            VerdictKind::AnchorReinstated => "anchor_reinstated",
            VerdictKind::SupersededBy => "superseded_by",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "anchor_obsolete" => Some(VerdictKind::AnchorObsolete),
            "anchor_reinstated" => Some(VerdictKind::AnchorReinstated),
            "superseded_by" => Some(VerdictKind::SupersededBy),
            _ => None,
        }
    }

    /// `true` for the two verdicts that mean "this witness's claim no longer
    /// holds and has not since been reinstated" — the predicate chunk
    /// binding (`storage::chunk_binding`) filters on.
    pub fn is_negative(&self) -> bool {
        matches!(
            self,
            VerdictKind::AnchorObsolete | VerdictKind::SupersededBy
        )
    }
}

/// One row of the witness verdict ledger.
#[derive(Debug, Clone, PartialEq)]
pub struct WitnessVerdictRow {
    /// `witness_ledger.id` this event is about. `None` only for a
    /// not-yet-inserted candidate row's `id` field is irrelevant — this is
    /// the FOREIGN reference, always present.
    pub witness_id: i64,
    pub verdict: VerdictKind,
    /// Set only for `SupersededBy`.
    pub successor_witness_id: Option<i64>,
    /// Commit proving the verdict: the successor's `at_oid` for
    /// `SupersededBy`, else the HEAD oid observed when the dream cycle ran.
    pub receipt_oid: Option<String>,
    /// HEAD (or, for a multi-repo dream run, that witness's own repo's HEAD)
    /// observed when the dream cycle that minted this event ran. Never
    /// wall-clock time.
    pub observed_head_oid: String,
}

/// The latest recorded event for `witness_id` (highest `id` — insertion
/// order, which for an append-only table is also chronological order) —
/// `None` if no event has ever been recorded for it.
pub fn latest_event(conn: &Connection, witness_id: i64) -> Result<Option<WitnessVerdictRow>> {
    let mut stmt = conn.prepare(
        "SELECT witness_id, verdict, successor_witness_id, receipt_oid, observed_head_oid
         FROM witness_verdicts WHERE witness_id = ?1 ORDER BY id DESC LIMIT 1",
    )?;
    let row = stmt
        .query_row(params![witness_id], |row| {
            let verdict_str: String = row.get(1)?;
            Ok(WitnessVerdictRow {
                witness_id: row.get(0)?,
                verdict: VerdictKind::parse(&verdict_str).unwrap_or(VerdictKind::AnchorObsolete),
                successor_witness_id: row.get(2)?,
                receipt_oid: row.get(3)?,
                observed_head_oid: row.get(4)?,
            })
        })
        .optional()?;
    Ok(row)
}

/// `true` when `candidate` would be a genuinely NEW conclusion relative to
/// `latest` — i.e. they differ in `verdict`, `successor_witness_id`,
/// `receipt_oid`, or `observed_head_oid`. Pure (no I/O) so both the dry-run
/// preview path and the real write path in [`insert_verdict_if_changed`]
/// share exactly one definition of "changed".
pub fn is_new_event(latest: Option<&WitnessVerdictRow>, candidate: &WitnessVerdictRow) -> bool {
    match latest {
        None => true,
        Some(latest) => {
            latest.verdict != candidate.verdict
                || latest.successor_witness_id != candidate.successor_witness_id
                || latest.receipt_oid != candidate.receipt_oid
                || latest.observed_head_oid != candidate.observed_head_oid
        }
    }
}

/// Insert `row` unless it is identical in `(verdict, successor_witness_id,
/// receipt_oid, observed_head_oid)` to the LATEST recorded event for
/// `row.witness_id` — see the module-level idempotency doc (app-side only,
/// no DB-level UNIQUE backstop). Returns whether a new row was actually
/// written (the caller uses this for dream-cycle stats).
///
/// # Concurrency
///
/// The engine's single-writer expectation is `Storage`'s
/// `Mutex<Connection>` — every in-process writer is already serialized
/// before reaching this function. The read-then-insert pair below is
/// additionally wrapped in one IMMEDIATE transaction as the cross-process
/// backstop (e.g. a second `csr-engine dream` against the same DB file):
/// `BEGIN IMMEDIATE` takes the write lock up front, so no second writer can
/// interleave between reading the latest event and appending the new one.
pub fn insert_verdict_if_changed(conn: &Connection, row: &WitnessVerdictRow) -> Result<bool> {
    // `new_unchecked` rather than `Connection::transaction` because the
    // storage locking idiom hands out `&Connection` (behind the Mutex), not
    // `&mut Connection`; the Mutex already guarantees the exclusive access
    // the checked API would statically enforce.
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let latest = latest_event(&tx, row.witness_id)?;
    if !is_new_event(latest.as_ref(), row) {
        return Ok(false); // tx drops here — rollback of a read-only txn, a no-op.
    }
    let changed = tx.execute(
        "INSERT INTO witness_verdicts
            (witness_id, verdict, successor_witness_id, receipt_oid, observed_head_oid)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            row.witness_id,
            row.verdict.as_str(),
            row.successor_witness_id,
            row.receipt_oid,
            row.observed_head_oid,
        ],
    )?;
    tx.commit()?;
    Ok(changed > 0)
}

/// Which consumption channel a symbol's current negative state feeds — the
/// two-channel v10 contract (see the module doc's "Symbol-level current
/// state" section for the full rule and rationale).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerdictChannel {
    /// Symbol truly gone or fully stale at the observed HEAD (no intact
    /// witness). Rank-affecting.
    Demote,
    /// Symbol evolved but is intact at the observed HEAD (plain A -> B
    /// evolution). No rank effect — surface "current as of `receipt_oid`"
    /// and let the reader decide.
    Annotate,
}

/// One storage-resolved verdict bound to a stable search chunk identity.
/// Consumers must key ranking and annotation decisions by `chunk_id`; the
/// conversation grouping used by batched reads is transport only.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkWitnessVerdict {
    pub chunk_id: String,
    pub file: String,
    pub symbol: Option<String>,
    pub channel: VerdictChannel,
    pub verdict: &'static str,
    pub receipt_oid: Option<String>,
}

/// Persist an exact witness-to-chunk attribution. This relation is separate
/// from the append-only verdict event stream because one witness may support
/// multiple historical chunks, while verdict state continues to evolve.
pub fn bind_witness_to_chunk(conn: &Connection, witness_id: i64, chunk_id: &str) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO witness_chunk_bindings (witness_id, chunk_id) VALUES (?1, ?2)",
        params![witness_id, chunk_id],
    )?;
    Ok(())
}

/// Publish the exact chunk attribution captured by the live code-graph hook
/// for a newly inserted (or deduplicated) witness row. Historical nodes that
/// predate chunk attribution safely leave no binding.
pub fn bind_witness_row_to_node_chunk(
    conn: &Connection,
    row: &super::witness_ledger::WitnessLedgerRow,
    node_id: &str,
) -> Result<()> {
    let witness_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM witness_ledger
             WHERE project = ?1 AND file = ?2
               AND COALESCE(symbol, '') = COALESCE(?3, '')
               AND COALESCE(span_start, -1) = COALESCE(?4, -1)
               AND COALESCE(span_end, -1) = COALESCE(?5, -1)
               AND stamp = ?6 AND tier = ?7
               AND COALESCE(at_oid, '') = COALESCE(?8, '')
               AND source_kind = ?9
               AND COALESCE(source_id, '') = COALESCE(?10, '')",
            params![
                row.project,
                row.file,
                row.symbol,
                row.span_start,
                row.span_end,
                row.stamp,
                row.tier,
                row.at_oid,
                row.source_kind,
                row.source_id,
            ],
            |result| result.get(0),
        )
        .optional()?;
    let chunk_id: Option<String> = conn
        .query_row(
            "SELECT n.last_chunk_id
             FROM code_nodes n JOIN chunks c ON c.id = n.last_chunk_id
             WHERE n.id = ?1 AND n.last_chunk_id IS NOT NULL",
            params![node_id],
            |result| result.get(0),
        )
        .optional()?;
    if let (Some(witness_id), Some(chunk_id)) = (witness_id, chunk_id) {
        bind_witness_to_chunk(conn, witness_id, &chunk_id)?;
    }
    Ok(())
}

/// Exact persisted `(chunk_id, conversation_id)` bindings for the supplied
/// witnesses. Orphaned chunk ids abstain through the inner join.
pub fn chunk_bindings_for_witnesses(
    conn: &Connection,
    witness_ids: &[i64],
) -> Result<HashMap<i64, Vec<(String, String)>>> {
    let mut out: HashMap<i64, Vec<(String, String)>> = HashMap::new();
    const BATCH: usize = 400;
    for batch in witness_ids.chunks(BATCH) {
        if batch.is_empty() {
            continue;
        }
        let placeholders: Vec<String> = (1..=batch.len()).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "SELECT b.witness_id, b.chunk_id, c.conversation_id
             FROM witness_chunk_bindings b
             JOIN chunks c ON c.id = b.chunk_id
             WHERE b.witness_id IN ({})
             ORDER BY b.witness_id, b.chunk_id",
            placeholders.join(", ")
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(batch.iter()), |row| {
            Ok((row.get::<_, i64>(0)?, row.get(1)?, row.get(2)?))
        })?;
        for row in rows {
            let (witness_id, chunk_id, conversation_id) = row?;
            out.entry(witness_id)
                .or_default()
                .push((chunk_id, conversation_id));
        }
    }
    Ok(out)
}

/// Order-independent CURRENT state of a `(project, file, symbol)` anchor —
/// the resolved [`VerdictChannel`] plus a deterministic representative
/// negative event for surfacing.
#[derive(Debug, Clone, PartialEq)]
pub struct SymbolVerdictState {
    pub channel: VerdictChannel,
    /// Every witness whose latest event is negative. Exact chunk attribution
    /// may have been recorded on an older witness than the representative,
    /// so binding must consult the complete current negative set.
    pub negative_witness_ids: Vec<i64>,
    /// The newest (highest event `id`) negative latest-event among the
    /// symbol's witnesses — the source of `verdict` and `receipt_oid` for
    /// surfacing; never a claim that global insertion order is meaningful
    /// across witnesses.
    pub representative: WitnessVerdictRow,
}

/// Resolve the module doc's two-channel rule for one anchor: `None` when no
/// witness has a negative latest event; otherwise `Demote` or `Annotate`
/// depending on whether any witness is intact at the most recent observed
/// HEAD. A conversation's attribution is to the SYMBOL, not to one specific
/// historical `at_oid` row, so this spans EVERY `witness_ledger` row sharing
/// the key. `symbol = None` selects the whole-file witness's history.
pub fn symbol_verdict_state(
    conn: &Connection,
    project: &str,
    file: &str,
    symbol: Option<&str>,
) -> Result<Option<SymbolVerdictState>> {
    // Latest event PER WITNESS (`ORDER BY id DESC` scoped to `witness_id` —
    // id order is chronological WITHIN one witness), returned newest-first
    // globally so index 0 carries the most recent `observed_head_oid` seen
    // for this symbol.
    let mut stmt = conn.prepare(
        "SELECT v.witness_id, v.verdict, v.successor_witness_id, v.receipt_oid, v.observed_head_oid
         FROM witness_verdicts v
         JOIN witness_ledger wl ON wl.id = v.witness_id
         WHERE wl.project = ?1 AND wl.file = ?2 AND wl.symbol IS ?3
           AND v.id = (SELECT MAX(v2.id) FROM witness_verdicts v2
                       WHERE v2.witness_id = v.witness_id)
         ORDER BY v.id DESC",
    )?;
    let latest_per_witness: Vec<WitnessVerdictRow> = stmt
        .query_map(params![project, file, symbol], |row| {
            let verdict_str: String = row.get(1)?;
            Ok(WitnessVerdictRow {
                witness_id: row.get(0)?,
                verdict: VerdictKind::parse(&verdict_str).unwrap_or(VerdictKind::AnchorObsolete),
                successor_witness_id: row.get(2)?,
                receipt_oid: row.get(3)?,
                observed_head_oid: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // (a) at least one witness whose latest event is negative. Rows are
    // newest-first, so `find` also picks the deterministic representative.
    let Some(representative) = latest_per_witness.iter().find(|r| r.verdict.is_negative()) else {
        return Ok(None);
    };

    // Channel resolution: is any witness intact at the most recent observed
    // HEAD? Resolve the current HEAD stamp (the ledger row for this symbol
    // whose `at_oid` equals the newest event's `observed_head_oid`); if ANY
    // witness of the symbol carries that stamp, the symbol is present at
    // current truth — evolution, not staleness: Annotate. Otherwise the
    // symbol is truly gone or fully stale: Demote.
    let head_oid = &latest_per_witness[0].observed_head_oid;
    let head_stamp: Option<String> = conn
        .query_row(
            "SELECT stamp FROM witness_ledger
             WHERE project = ?1 AND file = ?2 AND symbol IS ?3 AND at_oid = ?4
             LIMIT 1",
            params![project, file, symbol, head_oid],
            |r| r.get(0),
        )
        .optional()?;
    let intact_at_head = match head_stamp {
        Some(head_stamp) => conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM witness_ledger
                 WHERE project = ?1 AND file = ?2 AND symbol IS ?3 AND stamp = ?4)",
            params![project, file, symbol, head_stamp],
            |r| r.get(0),
        )?,
        None => false,
    };
    Ok(Some(SymbolVerdictState {
        channel: if intact_at_head {
            VerdictChannel::Annotate
        } else {
            VerdictChannel::Demote
        },
        negative_witness_ids: latest_per_witness
            .iter()
            .filter(|event| event.verdict.is_negative())
            .map(|event| event.witness_id)
            .collect(),
        representative: representative.clone(),
    }))
}

/// One `witness_verdicts` event joined to its witness's anchor identity —
/// the row shape `dream::report`'s timeline walks. Ordered newest-first by
/// the caller (`all_events_with_anchor`'s `ORDER BY v.id DESC`), so grouping
/// by day (`created_at`'s date prefix) needs no re-sort.
#[derive(Debug, Clone, PartialEq)]
pub struct DreamEventRow {
    pub event_id: i64,
    pub project: String,
    pub file: String,
    pub symbol: Option<String>,
    pub verdict: VerdictKind,
    pub successor_witness_id: Option<i64>,
    pub receipt_oid: Option<String>,
    pub observed_head_oid: String,
    /// `datetime('now')` string, `"YYYY-MM-DD HH:MM:SS"` (UTC) — see
    /// `storage::migrations`'s `witness_verdicts` table default.
    pub created_at: String,
}

/// Every `witness_verdicts` event ever recorded, newest-first, joined to its
/// witness's `(project, file, symbol)` identity. This is EVERY event, not
/// just the latest per witness — the dream report's timeline is a journal of
/// conclusions drawn over time, not just current state (that's
/// `all_demoted_symbols`/`symbol_verdict_state`).
pub fn all_events_with_anchor(conn: &Connection) -> Result<Vec<DreamEventRow>> {
    let mut stmt = conn.prepare(
        "SELECT v.id, wl.project, wl.file, wl.symbol, v.verdict, v.successor_witness_id,
                v.receipt_oid, v.observed_head_oid, v.created_at
         FROM witness_verdicts v
         JOIN witness_ledger wl ON wl.id = v.witness_id
         ORDER BY v.id DESC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            let verdict_str: String = row.get(4)?;
            Ok(DreamEventRow {
                event_id: row.get(0)?,
                project: row.get(1)?,
                file: row.get(2)?,
                symbol: row.get(3)?,
                verdict: VerdictKind::parse(&verdict_str).unwrap_or(VerdictKind::AnchorObsolete),
                successor_witness_id: row.get(5)?,
                receipt_oid: row.get(6)?,
                observed_head_oid: row.get(7)?,
                created_at: row.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The most recent dream cycle observed anywhere in the ledger: the globally
/// newest event's `(observed_head_oid, created_at)`. `None` if no dream
/// cycle has ever written an event (a fresh install, or `dream` never run).
pub fn last_dream_run(conn: &Connection) -> Result<Option<(String, String)>> {
    conn.query_row(
        "SELECT observed_head_oid, created_at FROM witness_verdicts ORDER BY id DESC LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(Into::into)
}

/// Totals `(obsolete, superseded, reinstated)` across EVERY event ever
/// recorded (not latest-per-witness) — the report header's "totals by
/// verdict type" and `status`'s `by_verdict` block.
pub fn event_totals_by_verdict(conn: &Connection) -> Result<(i64, i64, i64)> {
    let mut stmt =
        conn.prepare("SELECT verdict, COUNT(*) FROM witness_verdicts GROUP BY verdict")?;
    let mut obsolete = 0i64;
    let mut superseded = 0i64;
    let mut reinstated = 0i64;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for r in rows {
        let (verdict, count) = r?;
        match verdict.as_str() {
            "anchor_obsolete" => obsolete = count,
            "superseded_by" => superseded = count,
            "anchor_reinstated" => reinstated = count,
            _ => {}
        }
    }
    Ok((obsolete, superseded, reinstated))
}

/// A `(project, file, symbol)` anchor CURRENTLY on the `Demote` channel —
/// "what CSR forgot": truly gone or fully stale at current truth. Pairs the
/// anchor identity with the same [`SymbolVerdictState`] `symbol_verdict_state`
/// would return for it, so a report/status caller never has to re-derive the
/// channel itself.
#[derive(Debug, Clone, PartialEq)]
pub struct DemotedSymbol {
    pub project: String,
    pub file: String,
    pub symbol: Option<String>,
    pub state: SymbolVerdictState,
}

/// Every anchor whose current state resolves to [`VerdictChannel::Demote`].
/// Two passes: first a single query finds every DISTINCT `(project, file,
/// symbol)` anchor with an uncancelled negative latest event (a cheap
/// candidate filter — `Annotate` anchors are excluded from consideration
/// here too, since they also have a negative latest event, so this pass
/// over-selects and the second pass narrows); then the candidates are
/// resolved through the SAME state logic chunk binding uses, so this can
/// never disagree with it.
///
/// This IS a hot path: `status` (including the `--compact` statusline) calls
/// it on every poll via `gather_dream`, so the second pass batches named
/// symbols through [`symbol_verdict_states_for_files`] (one query per ≤400
/// `(project, file)` pairs, documented-identical semantics per key) instead
/// of issuing 2-3 `symbol_verdict_state` round trips per candidate — the
/// same hot-path discipline as `Storage::integrity_check_cached`. Whole-file
/// anchors (`symbol IS NULL`) stay on the single-anchor resolver, which the
/// batched fn deliberately excludes; they are the rare case (minted only for
/// files with no symbol-level nodes).
pub fn all_demoted_symbols(conn: &Connection) -> Result<Vec<DemotedSymbol>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT wl.project, wl.file, wl.symbol
         FROM witness_verdicts v
         JOIN witness_ledger wl ON wl.id = v.witness_id
         WHERE v.verdict IN ('anchor_obsolete','superseded_by')
           AND v.id = (SELECT MAX(v2.id) FROM witness_verdicts v2 WHERE v2.witness_id = v.witness_id)",
    )?;
    let anchors: Vec<(String, String, Option<String>)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut pairs: Vec<(String, String)> = anchors
        .iter()
        .filter(|(_, _, symbol)| symbol.is_some())
        .map(|(project, file, _)| (project.clone(), file.clone()))
        .collect();
    pairs.sort();
    pairs.dedup();
    let named_states = symbol_verdict_states_for_files(conn, &pairs)?;

    let mut out = Vec::new();
    for (project, file, symbol) in anchors {
        let state = match &symbol {
            Some(sym) => named_states
                .get(&(project.clone(), file.clone(), sym.clone()))
                .cloned(),
            None => symbol_verdict_state(conn, &project, &file, None)?,
        };
        if let Some(state) = state {
            if state.channel == VerdictChannel::Demote {
                out.push(DemotedSymbol {
                    project,
                    file,
                    symbol,
                    state,
                });
            }
        }
    }
    Ok(out)
}

/// Bulk variant of [`symbol_verdict_state`]: resolve the CURRENT state of
/// EVERY symbol-level anchor recorded for the given `(project, file)` pairs,
/// in ONE query per ≤400-pair batch (chunk binding previously issued one
/// `symbol_verdict_state` round-trip per bound symbol, all under the storage
/// mutex). Returns a map keyed by `(project, file, symbol)`; anchors with no
/// negative-latest witness are simply absent (exactly the single-symbol
/// fn's `None`).
///
/// Semantics are identical to [`symbol_verdict_state`] per key, restated:
/// rows are latest-event-per-witness (`MAX(id)` scoped to `witness_id`),
/// ordered newest-first; the representative is the newest negative one; the
/// channel is Annotate iff a ledger row for the key exists at the newest
/// event's `observed_head_oid` (the single-symbol fn's stamp-lookup +
/// EXISTS pair reduces to exactly that check — the row that supplies
/// `head_stamp` always satisfies its own EXISTS), else Demote. Whole-file
/// witnesses (`symbol IS NULL`) are out of scope here — chunk binding only
/// ever binds named symbols; the single-symbol fn remains the entry point
/// for those.
pub fn symbol_verdict_states_for_files(
    conn: &Connection,
    files: &[(String, String)],
) -> Result<std::collections::BTreeMap<(String, String, String), SymbolVerdictState>> {
    let mut out = std::collections::BTreeMap::new();
    if files.is_empty() {
        return Ok(out);
    }
    // ≤400 pairs (800 bind variables) per statement — same
    // SQLITE_MAX_VARIABLE_NUMBER reasoning as `codegraph::nodes_for_conversations`.
    const BATCH: usize = 400;
    for batch in files.chunks(BATCH) {
        let pair_filter: Vec<String> = (0..batch.len())
            .map(|i| format!("(wl.project = ?{} AND wl.file = ?{})", 2 * i + 1, 2 * i + 2))
            .collect();
        let sql = format!(
            "SELECT wl.project, wl.file, wl.symbol,
                    v.witness_id, v.verdict, v.successor_witness_id,
                    v.receipt_oid, v.observed_head_oid,
                    EXISTS(SELECT 1 FROM witness_ledger wl2
                           WHERE wl2.project = wl.project AND wl2.file = wl.file
                             AND wl2.symbol IS wl.symbol
                             AND wl2.at_oid = v.observed_head_oid)
             FROM witness_verdicts v
             JOIN witness_ledger wl ON wl.id = v.witness_id
             WHERE wl.symbol IS NOT NULL
               AND ({})
               AND v.id = (SELECT MAX(v2.id) FROM witness_verdicts v2
                           WHERE v2.witness_id = v.witness_id)
             ORDER BY v.id DESC",
            pair_filter.join(" OR ")
        );
        let params: Vec<&dyn rusqlite::types::ToSql> = batch
            .iter()
            .flat_map(|(p, f)| {
                [
                    p as &dyn rusqlite::types::ToSql,
                    f as &dyn rusqlite::types::ToSql,
                ]
            })
            .collect();
        let mut stmt = conn.prepare(&sql)?;
        // (key, latest-per-witness event, intact-at-this-event's-HEAD),
        // newest-first globally and therefore newest-first within each key.
        let rows: Vec<((String, String, String), WitnessVerdictRow, bool)> = stmt
            .query_map(params.as_slice(), |row| {
                let verdict_str: String = row.get(4)?;
                Ok((
                    (row.get(0)?, row.get(1)?, row.get(2)?),
                    WitnessVerdictRow {
                        witness_id: row.get(3)?,
                        verdict: VerdictKind::parse(&verdict_str)
                            .unwrap_or(VerdictKind::AnchorObsolete),
                        successor_witness_id: row.get(5)?,
                        receipt_oid: row.get(6)?,
                        observed_head_oid: row.get(7)?,
                    },
                    row.get(8)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        // Group per key, preserving the newest-first row order.
        let mut grouped: std::collections::BTreeMap<
            (String, String, String),
            Vec<(WitnessVerdictRow, bool)>,
        > = std::collections::BTreeMap::new();
        for (key, event, intact) in rows {
            grouped.entry(key).or_default().push((event, intact));
        }
        for (key, events) in grouped {
            // Newest-first, so `find` picks the same deterministic
            // representative as the single-symbol fn.
            let Some((representative, _)) = events.iter().find(|(e, _)| e.verdict.is_negative())
            else {
                continue; // no negative-latest witness — same as `None`.
            };
            // events[0] is the globally newest event for the key: its
            // `observed_head_oid` is the head the intact check must use, and
            // its per-row EXISTS was computed against exactly that oid.
            let intact_at_head = events[0].1;
            out.insert(
                key,
                SymbolVerdictState {
                    channel: if intact_at_head {
                        VerdictChannel::Annotate
                    } else {
                        VerdictChannel::Demote
                    },
                    negative_witness_ids: events
                        .iter()
                        .filter(|(event, _)| event.verdict.is_negative())
                        .map(|(event, _)| event.witness_id)
                        .collect(),
                    representative: representative.clone(),
                },
            );
        }
    }
    Ok(out)
}

/// Binding-only bulk state lookup. Each anchor carries the exact selected
/// re-derivation generation (`Some(source_id)`) or the legacy lineage
/// (`None`). Both negative-event selection and the intact-at-HEAD check are
/// constrained to that lineage, preventing an old collapsed spelling from
/// poisoning a corrected complete generation.
pub(crate) fn symbol_verdict_states_for_lineages(
    conn: &Connection,
    anchors: &[(String, String, String, Option<String>)],
) -> Result<std::collections::BTreeMap<(String, String, String), SymbolVerdictState>> {
    let mut out = std::collections::BTreeMap::new();
    const BATCH: usize = 200;
    for batch in anchors.chunks(BATCH) {
        let filters: Vec<String> = (0..batch.len())
            .map(|i| {
                let base = 4 * i + 1;
                format!(
                    "(wl.project = ?{base} AND wl.file = ?{} AND wl.symbol = ?{}
                      AND ((?{} IS NULL AND wl.source_kind <> 'backfill_rederived_v2')
                           OR (wl.source_kind = 'backfill_rederived_v2'
                               AND wl.source_id = ?{})))",
                    base + 1,
                    base + 2,
                    base + 3,
                    base + 3
                )
            })
            .collect();
        let sql = format!(
            "SELECT wl.project, wl.file, wl.symbol,
                    v.witness_id, v.verdict, v.successor_witness_id,
                    v.receipt_oid, v.observed_head_oid,
                    EXISTS(SELECT 1 FROM witness_ledger wl2
                           WHERE wl2.project = wl.project AND wl2.file = wl.file
                             AND wl2.symbol IS wl.symbol
                             AND wl2.at_oid = v.observed_head_oid
                             AND ((wl.source_kind = 'backfill_rederived_v2'
                                   AND wl2.source_kind = 'backfill_rederived_v2'
                                   AND wl2.source_id IS wl.source_id)
                                  OR (wl.source_kind <> 'backfill_rederived_v2'
                                      AND wl2.source_kind <> 'backfill_rederived_v2')))
             FROM witness_verdicts v
             JOIN witness_ledger wl ON wl.id = v.witness_id
             WHERE ({})
               AND v.id = (SELECT MAX(v2.id) FROM witness_verdicts v2
                           WHERE v2.witness_id = v.witness_id)
             ORDER BY v.id DESC",
            filters.join(" OR ")
        );
        let params: Vec<&dyn rusqlite::types::ToSql> = batch
            .iter()
            .flat_map(|(project, file, symbol, source_id)| {
                [
                    project as &dyn rusqlite::types::ToSql,
                    file as &dyn rusqlite::types::ToSql,
                    symbol as &dyn rusqlite::types::ToSql,
                    source_id as &dyn rusqlite::types::ToSql,
                ]
            })
            .collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows: Vec<((String, String, String), WitnessVerdictRow, bool)> = stmt
            .query_map(params.as_slice(), |row| {
                let verdict_str: String = row.get(4)?;
                Ok((
                    (row.get(0)?, row.get(1)?, row.get(2)?),
                    WitnessVerdictRow {
                        witness_id: row.get(3)?,
                        verdict: VerdictKind::parse(&verdict_str)
                            .unwrap_or(VerdictKind::AnchorObsolete),
                        successor_witness_id: row.get(5)?,
                        receipt_oid: row.get(6)?,
                        observed_head_oid: row.get(7)?,
                    },
                    row.get(8)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut grouped: std::collections::BTreeMap<
            (String, String, String),
            Vec<(WitnessVerdictRow, bool)>,
        > = std::collections::BTreeMap::new();
        for (key, event, intact) in rows {
            grouped.entry(key).or_default().push((event, intact));
        }
        for (key, events) in grouped {
            let Some((representative, _)) = events.iter().find(|(e, _)| e.verdict.is_negative())
            else {
                continue;
            };
            out.insert(
                key,
                SymbolVerdictState {
                    channel: if events[0].1 {
                        VerdictChannel::Annotate
                    } else {
                        VerdictChannel::Demote
                    },
                    negative_witness_ids: events
                        .iter()
                        .filter(|(event, _)| event.verdict.is_negative())
                        .map(|(event, _)| event.witness_id)
                        .collect(),
                    representative: representative.clone(),
                },
            );
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::witness_ledger::{self, WitnessLedgerRow};

    fn open() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::storage::migrations::run(&conn).unwrap();
        conn
    }

    fn ledger_row(at_oid: &str, stamp: &str) -> WitnessLedgerRow {
        WitnessLedgerRow {
            id: 0,
            project: "proj".into(),
            file: "/repo/src/lib.rs".into(),
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

    #[test]
    fn no_event_recorded_returns_none() {
        let conn = open();
        assert!(latest_event(&conn, 999).unwrap().is_none());
    }

    #[test]
    fn insert_and_query_round_trip() {
        let conn = open();
        witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
        let w1 = witness_ledger::latest_witness_for_symbol(
            &conn,
            "proj",
            "/repo/src/lib.rs",
            Some("foo"),
        )
        .unwrap()
        .unwrap();
        let wid: i64 = conn
            .query_row(
                "SELECT id FROM witness_ledger WHERE stamp = 'b3:1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(w1.stamp, "b3:1");

        let row = WitnessVerdictRow {
            witness_id: wid,
            verdict: VerdictKind::AnchorObsolete,
            successor_witness_id: None,
            receipt_oid: Some("headoid".into()),
            observed_head_oid: "headoid".into(),
        };
        let wrote = insert_verdict_if_changed(&conn, &row).unwrap();
        assert!(wrote, "first insert must write");
        let latest = latest_event(&conn, wid).unwrap().unwrap();
        assert_eq!(latest.verdict, VerdictKind::AnchorObsolete);
        assert_eq!(latest.observed_head_oid, "headoid");
    }

    #[test]
    fn identical_event_at_same_head_is_a_no_op() {
        let conn = open();
        witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
        let wid: i64 = conn
            .query_row("SELECT id FROM witness_ledger", [], |r| r.get(0))
            .unwrap();
        let row = WitnessVerdictRow {
            witness_id: wid,
            verdict: VerdictKind::AnchorObsolete,
            successor_witness_id: None,
            receipt_oid: Some("headoid".into()),
            observed_head_oid: "headoid".into(),
        };
        assert!(insert_verdict_if_changed(&conn, &row).unwrap());
        assert!(
            !insert_verdict_if_changed(&conn, &row).unwrap(),
            "identical (verdict, successor, observed_head_oid) at the same witness must not re-insert"
        );
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM witness_verdicts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn same_verdict_new_head_still_inserts_a_fresh_event() {
        // Per the module doc: identity is (verdict, successor, observed_head_oid)
        // — an unchanged conclusion re-confirmed at a NEW HEAD is not
        // "identical" to the prior event and gets its own row.
        let conn = open();
        witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
        let wid: i64 = conn
            .query_row("SELECT id FROM witness_ledger", [], |r| r.get(0))
            .unwrap();
        let row_at_head1 = WitnessVerdictRow {
            witness_id: wid,
            verdict: VerdictKind::AnchorObsolete,
            successor_witness_id: None,
            receipt_oid: Some("head1".into()),
            observed_head_oid: "head1".into(),
        };
        let row_at_head2 = WitnessVerdictRow {
            observed_head_oid: "head2".into(),
            receipt_oid: Some("head2".into()),
            ..row_at_head1.clone()
        };
        assert!(insert_verdict_if_changed(&conn, &row_at_head1).unwrap());
        assert!(insert_verdict_if_changed(&conn, &row_at_head2).unwrap());
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM witness_verdicts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2, "a new observed HEAD must mint a fresh event");
    }

    #[test]
    fn reinstatement_is_a_new_event_not_a_mutation() {
        let conn = open();
        witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
        let wid: i64 = conn
            .query_row("SELECT id FROM witness_ledger", [], |r| r.get(0))
            .unwrap();
        insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: wid,
                verdict: VerdictKind::SupersededBy,
                successor_witness_id: Some(42),
                receipt_oid: Some("head1".into()),
                observed_head_oid: "head1".into(),
            },
        )
        .unwrap();
        insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: wid,
                verdict: VerdictKind::AnchorReinstated,
                successor_witness_id: None,
                receipt_oid: Some("head2".into()),
                observed_head_oid: "head2".into(),
            },
        )
        .unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM witness_verdicts WHERE witness_id = ?1",
                params![wid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2, "both events must survive — no mutation");
        let latest = latest_event(&conn, wid).unwrap().unwrap();
        assert_eq!(latest.verdict, VerdictKind::AnchorReinstated);
    }

    #[test]
    fn a_b_evolution_is_annotate_not_demote() {
        // Two different at_oid rows for the SAME (project, file, symbol);
        // the OLDER one is superseded by the newer, which IS the row at the
        // observed HEAD — plain A -> B evolution. A negative-latest witness
        // exists AND a witness is intact at HEAD, so the state is the
        // ANNOTATE channel (no rank effect), never Demote.
        let conn = open();
        witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
        witness_ledger::insert_witness(&conn, &ledger_row("bbb", "b3:2")).unwrap();
        let old_id: i64 = conn
            .query_row(
                "SELECT id FROM witness_ledger WHERE at_oid = 'aaa'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let new_id: i64 = conn
            .query_row(
                "SELECT id FROM witness_ledger WHERE at_oid = 'bbb'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: old_id,
                verdict: VerdictKind::SupersededBy,
                successor_witness_id: Some(new_id),
                receipt_oid: Some("bbb".into()),
                observed_head_oid: "bbb".into(),
            },
        )
        .unwrap();
        let state = symbol_verdict_state(&conn, "proj", "/repo/src/lib.rs", Some("foo"))
            .unwrap()
            .expect("A -> B evolution must surface on the annotate channel");
        assert_eq!(
            state.channel,
            VerdictChannel::Annotate,
            "a witness intact at the observed HEAD (the 'bbb' row) blocks demotion"
        );
        assert_eq!(
            state.representative.receipt_oid.as_deref(),
            Some("bbb"),
            "annotation carries the most recent negative event's receipt"
        );
    }

    #[test]
    fn vanished_symbol_is_demote() {
        // Negative latest event AND no ledger row at the observed HEAD —
        // the symbol is truly gone at current truth: DEMOTE channel.
        let conn = open();
        witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
        witness_ledger::insert_witness(&conn, &ledger_row("bbb", "b3:2")).unwrap();
        let old_id: i64 = conn
            .query_row(
                "SELECT id FROM witness_ledger WHERE at_oid = 'aaa'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: old_id,
                verdict: VerdictKind::AnchorObsolete,
                successor_witness_id: None,
                receipt_oid: Some("headoid".into()),
                observed_head_oid: "headoid".into(), // no ledger row at this oid
            },
        )
        .unwrap();
        let state = symbol_verdict_state(&conn, "proj", "/repo/src/lib.rs", Some("foo"))
            .unwrap()
            .expect("obsolete with no HEAD witness must surface");
        assert_eq!(state.channel, VerdictChannel::Demote);
        assert_eq!(state.representative.verdict, VerdictKind::AnchorObsolete);
    }

    #[test]
    fn no_verdict_recorded_returns_none_for_symbol_query() {
        let conn = open();
        witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
        assert!(
            symbol_verdict_state(&conn, "proj", "/repo/src/lib.rs", Some("foo"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_b_a_revert_is_never_demoted_regardless_of_event_insertion_order() {
        // A -> B -> A with the FULL event history a real dream run leaves
        // behind: ledger rows at c1 (stamp A), c2 (stamp B), c3 (stamp A
        // again, c3 == the most recent observed HEAD). The c1 witness
        // carries superseded_by (minted at HEAD c2) then anchor_reinstated
        // (minted at HEAD c3); the c2 (B) witness carries its own
        // superseded_by (receipt c3) and stays NEGATIVE-latest while the A
        // witnesses are intact at HEAD. By the two-channel rule that is
        // ANNOTATE — "the symbol carries history of a rejected change",
        // surfaced with the B-supersession receipt (c3) — and NEVER Demote,
        // whichever ORDER c1's two events were inserted in (C1:
        // order-independence).
        for reversed in [false, true] {
            let conn = open();
            witness_ledger::insert_witness(&conn, &ledger_row("c1", "b3:A")).unwrap();
            witness_ledger::insert_witness(&conn, &ledger_row("c2", "b3:B")).unwrap();
            witness_ledger::insert_witness(&conn, &ledger_row("c3", "b3:A")).unwrap();
            let oid_id = |oid: &str| -> i64 {
                conn.query_row(
                    "SELECT id FROM witness_ledger WHERE at_oid = ?1",
                    params![oid],
                    |r| r.get(0),
                )
                .unwrap()
            };
            let (c1_id, c2_id, c3_id) = (oid_id("c1"), oid_id("c2"), oid_id("c3"));
            let c1_superseded = WitnessVerdictRow {
                witness_id: c1_id,
                verdict: VerdictKind::SupersededBy,
                successor_witness_id: Some(c2_id),
                receipt_oid: Some("c2".into()),
                observed_head_oid: "c2".into(),
            };
            let c1_reinstated = WitnessVerdictRow {
                witness_id: c1_id,
                verdict: VerdictKind::AnchorReinstated,
                successor_witness_id: None,
                receipt_oid: Some("c3".into()),
                observed_head_oid: "c3".into(),
            };
            // B's own supersession by the revert commit — its latest (and
            // uncancelled) event.
            let c2_superseded = WitnessVerdictRow {
                witness_id: c2_id,
                verdict: VerdictKind::SupersededBy,
                successor_witness_id: Some(c3_id),
                receipt_oid: Some("c3".into()),
                observed_head_oid: "c3".into(),
            };
            let events: [&WitnessVerdictRow; 3] = if reversed {
                [&c1_reinstated, &c1_superseded, &c2_superseded]
            } else {
                [&c1_superseded, &c1_reinstated, &c2_superseded]
            };
            for e in events {
                insert_verdict_if_changed(&conn, e).unwrap();
            }
            let state = symbol_verdict_state(&conn, "proj", "/repo/src/lib.rs", Some("foo"))
                .unwrap()
                .expect("B's uncancelled negative event must surface");
            assert_eq!(
                state.channel,
                VerdictChannel::Annotate,
                "A->B->A must never demote (reversed insertion: {reversed})"
            );
            assert_eq!(
                state.representative.receipt_oid.as_deref(),
                Some("c3"),
                "annotation carries the B-supersession receipt"
            );
        }
    }

    #[test]
    fn b_a_b_state_revisit_inserts_three_events_and_reruns_add_nothing() {
        // B -> A -> B: superseded, reinstated, then superseded again with
        // the exact same fields as the first event. The third event DIFFERS
        // from the latest (reinstated), so it must insert — the old UNIQUE
        // identity index would have silently swallowed it (C2). A rerun of
        // the same final event at the same HEAD then adds nothing.
        let conn = open();
        witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
        let wid: i64 = conn
            .query_row("SELECT id FROM witness_ledger", [], |r| r.get(0))
            .unwrap();
        let superseded = WitnessVerdictRow {
            witness_id: wid,
            verdict: VerdictKind::SupersededBy,
            successor_witness_id: Some(42),
            receipt_oid: Some("succ".into()),
            observed_head_oid: "head1".into(),
        };
        let reinstated = WitnessVerdictRow {
            witness_id: wid,
            verdict: VerdictKind::AnchorReinstated,
            successor_witness_id: None,
            receipt_oid: Some("head2".into()),
            observed_head_oid: "head2".into(),
        };
        assert!(insert_verdict_if_changed(&conn, &superseded).unwrap());
        assert!(insert_verdict_if_changed(&conn, &reinstated).unwrap());
        assert!(
            insert_verdict_if_changed(&conn, &superseded).unwrap(),
            "re-visiting an earlier state must insert a THIRD event"
        );
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM witness_verdicts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3, "B -> A -> B is three events");
        let latest = latest_event(&conn, wid).unwrap().unwrap();
        assert!(
            latest.verdict.is_negative(),
            "final state must be negative (superseded)"
        );
        assert!(
            !insert_verdict_if_changed(&conn, &superseded).unwrap(),
            "rerun at the same HEAD with the same conclusion adds nothing"
        );
        let count_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM witness_verdicts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count_after, 3);
    }

    #[test]
    fn all_events_with_anchor_joins_and_orders_newest_first() {
        let conn = open();
        witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
        witness_ledger::insert_witness(&conn, &ledger_row("bbb", "b3:2")).unwrap();
        let old_id: i64 = conn
            .query_row(
                "SELECT id FROM witness_ledger WHERE at_oid = 'aaa'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let new_id: i64 = conn
            .query_row(
                "SELECT id FROM witness_ledger WHERE at_oid = 'bbb'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: old_id,
                verdict: VerdictKind::SupersededBy,
                successor_witness_id: Some(new_id),
                receipt_oid: Some("bbb".into()),
                observed_head_oid: "bbb".into(),
            },
        )
        .unwrap();
        insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: old_id,
                verdict: VerdictKind::AnchorReinstated,
                successor_witness_id: None,
                receipt_oid: Some("ccc".into()),
                observed_head_oid: "ccc".into(),
            },
        )
        .unwrap();
        let events = all_events_with_anchor(&conn).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].verdict,
            VerdictKind::AnchorReinstated,
            "newest event first"
        );
        assert_eq!(events[0].project, "proj");
        assert_eq!(events[0].symbol.as_deref(), Some("foo"));
        assert_eq!(events[1].verdict, VerdictKind::SupersededBy);
    }

    #[test]
    fn all_events_with_anchor_empty_db_returns_empty() {
        let conn = open();
        assert!(all_events_with_anchor(&conn).unwrap().is_empty());
    }

    #[test]
    fn last_dream_run_reports_the_globally_newest_event() {
        let conn = open();
        assert!(
            last_dream_run(&conn).unwrap().is_none(),
            "no events ever recorded"
        );
        witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
        let wid: i64 = conn
            .query_row("SELECT id FROM witness_ledger", [], |r| r.get(0))
            .unwrap();
        insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: wid,
                verdict: VerdictKind::AnchorObsolete,
                successor_witness_id: None,
                receipt_oid: Some("head1".into()),
                observed_head_oid: "head1".into(),
            },
        )
        .unwrap();
        let (head_oid, created_at) = last_dream_run(&conn).unwrap().unwrap();
        assert_eq!(head_oid, "head1");
        assert!(!created_at.is_empty());
    }

    #[test]
    fn event_totals_by_verdict_counts_every_event_not_just_latest() {
        let conn = open();
        witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
        let wid: i64 = conn
            .query_row("SELECT id FROM witness_ledger", [], |r| r.get(0))
            .unwrap();
        insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: wid,
                verdict: VerdictKind::SupersededBy,
                successor_witness_id: Some(42),
                receipt_oid: Some("head1".into()),
                observed_head_oid: "head1".into(),
            },
        )
        .unwrap();
        insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: wid,
                verdict: VerdictKind::AnchorReinstated,
                successor_witness_id: None,
                receipt_oid: Some("head2".into()),
                observed_head_oid: "head2".into(),
            },
        )
        .unwrap();
        let (obsolete, superseded, reinstated) = event_totals_by_verdict(&conn).unwrap();
        assert_eq!(obsolete, 0);
        assert_eq!(
            superseded, 1,
            "counts the event even though it's no longer latest"
        );
        assert_eq!(reinstated, 1);
    }

    #[test]
    fn all_demoted_symbols_finds_only_the_demote_channel_anchor() {
        let conn = open();
        // Anchor 1 ("foo"): vanished symbol -> Demote.
        witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
        let foo_id: i64 = conn
            .query_row(
                "SELECT id FROM witness_ledger WHERE stamp = 'b3:1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: foo_id,
                verdict: VerdictKind::AnchorObsolete,
                successor_witness_id: None,
                receipt_oid: Some("headoid".into()),
                observed_head_oid: "headoid".into(),
            },
        )
        .unwrap();

        // Anchor 2 ("bar"): plain A -> B evolution -> Annotate, never Demote.
        let mut bar_a = ledger_row("bbb", "b3:bar-a");
        bar_a.symbol = Some("bar".into());
        let mut bar_b = ledger_row("ccc", "b3:bar-b");
        bar_b.symbol = Some("bar".into());
        witness_ledger::insert_witness(&conn, &bar_a).unwrap();
        witness_ledger::insert_witness(&conn, &bar_b).unwrap();
        let bar_a_id: i64 = conn
            .query_row(
                "SELECT id FROM witness_ledger WHERE stamp = 'b3:bar-a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let bar_b_id: i64 = conn
            .query_row(
                "SELECT id FROM witness_ledger WHERE stamp = 'b3:bar-b'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: bar_a_id,
                verdict: VerdictKind::SupersededBy,
                successor_witness_id: Some(bar_b_id),
                receipt_oid: Some("ccc".into()),
                observed_head_oid: "ccc".into(),
            },
        )
        .unwrap();

        let demoted = all_demoted_symbols(&conn).unwrap();
        assert_eq!(demoted.len(), 1, "only the vanished 'foo' anchor demotes");
        assert_eq!(demoted[0].symbol.as_deref(), Some("foo"));
        assert_eq!(demoted[0].state.channel, VerdictChannel::Demote);
    }

    #[test]
    fn all_demoted_symbols_empty_when_no_negative_events() {
        let conn = open();
        witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
        assert!(all_demoted_symbols(&conn).unwrap().is_empty());
    }
}
