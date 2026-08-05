//! Chunk -> witness binding (v10 "dreaming" read path — see `crate::dream`).
//!
//! Search results carry conversation ids, not `(file, symbol)` anchors, so
//! serving a "this chunk's code claim is stale" hint at search time requires
//! resolving a conversation id back to whatever symbols the conversation
//! actually named — and then asking `witness_verdicts` for the CURRENT
//! state of that symbol.
//!
//! # Two-channel consumption (the deliberate v10 contract)
//!
//! Every hit this module surfaces carries a [`VerdictChannel`]:
//!
//! 1. **Demote** — the symbol has negative-latest witnesses AND no witness
//!    intact at the observed HEAD: truly gone or fully stale.
//!    Rank-affecting.
//! 2. **Annotate** — the symbol has at least one negative-latest witness
//!    AND at least one witness intact at the observed HEAD (the plain
//!    A -> B evolution). NO rank effect: consumers annotate the chunk with
//!    "symbol evolved since earlier evidence; current as of `receipt_oid`",
//!    where `receipt_oid` is the receipt of the most recent negative event
//!    for the symbol.
//!
//! Rationale: symbol-level binding cannot attribute staleness to individual
//! chunk COHORTS — chunks minted in the A era and chunks minted in the B
//! era share the same `(conversation -> symbol)` binding, so demoting on
//! evolution would punish current-truth chunks alongside stale ones. v10
//! therefore never demotes on evolution — it annotates with the receipt and
//! lets the reader decide. (Corollary: a fully-reverted A -> B -> A still
//! surfaces as Annotate, because B's witness keeps an uncancelled negative
//! event while the A witnesses are intact at HEAD — "the symbol carries
//! history of a rejected change".)
//!
//! # The actual link: `code_nodes.first_conv_id` / `last_conv_id`
//!
//! `code_nodes` already carries direct conversation attribution on every
//! row (`first_conv_id`: the conversation that introduced the symbol;
//! `last_conv_id`: the conversation that most recently touched it — see
//! `storage::codegraph`'s module doc, "every node and edge carries `conv_id`
//! / `session_id` provenance"). `code_edges.conv_id` carries the same kind
//! of attribution but for a RELATION (a call/import site), not for the
//! symbol's own definition — `code_nodes` is the direct `(file, symbol)`
//! link this module needs, so `codegraph::nodes_for_conversations` (which
//! this module calls) queries `code_nodes`, not `code_edges`.
//!
//! # The qualification mismatch, and its fallback
//!
//! `code_nodes.name` is the UNQUALIFIED symbol name emitted by extraction
//! (`extraction::codegraph` deliberately carries no impl/class container
//! rows — see `import::backfill::qualify_witness_symbols`'s doc comment).
//! `witness_ledger.symbol`, by contrast, is TYPE-QUALIFIED for exactly the
//! symbols `qualify_witness_symbols` could resolve to a containing
//! impl/class span (`"Container::name"` for Rust, `"Container.name"` for
//! dot-separated languages) — minted that way specifically so two unrelated
//! same-named methods in different containers don't collide in the ledger.
//!
//! Binding therefore tries, in order:
//! 1. **Exact match**: `witness_ledger.symbol == code_nodes.name` (the
//!    common case for free functions, types, consts — nothing to qualify).
//! 2. **Suffix fallback**: among the DISTINCT `witness_ledger.symbol` values
//!    recorded for that `(project, file)`, find every one that is either
//!    exactly `name` or ends with a known container separator (`"::"` or
//!    `"."`) immediately followed by `name`. If EXACTLY ONE such symbol
//!    exists, bind to it. If two or more exist (e.g. `Foo::run` and
//!    `Bar::run` both present for the same file, and `code_nodes` only says
//!    `"run"`), the match is genuinely ambiguous — **abstain**: no binding
//!    is produced for that node, rather than guessing.
//!
//! A disambiguated collision symbol (`"name#2"`, `"name#3"`, ... — see
//! `qualify_witness_symbols`'s "collision safety net") poisons the whole
//! base name: if the file's witness symbol set contains ANY `#N`-suffixed
//! variant sharing the same base (e.g. `Foo::run#2` alongside `Foo::run`),
//! binding for that base name ABSTAINS entirely — exact match AND suffix
//! fallback both. The `#N` suffix means the ledger knows of multiple
//! same-named spans in that file, and a bare `code_nodes.name` has no
//! signal at all for which duplicate it should bind to — binding the
//! unsuffixed form anyway would silently attribute the wrong span's
//! verdict.

use anyhow::Result;
use rusqlite::Connection;
use std::collections::BTreeMap;

use super::codegraph::{self, NodeRow};
use super::witness_verdicts::{self, VerdictChannel};

/// One verdict hit surfaced for a chunk's conversation — carries the
/// two-channel contract's `channel` (see the module doc's "Two-channel
/// consumption") alongside the raw verdict and receipt.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkWitnessVerdict {
    pub file: String,
    /// `None` only if a future caller ever binds against a whole-file
    /// witness; `nodes_for_conversations` only returns symbol-level nodes
    /// today, so this is always `Some` in practice.
    pub symbol: Option<String>,
    /// `Demote` (rank-affecting: symbol gone/fully stale at HEAD) or
    /// `Annotate` (no rank effect: "symbol evolved since earlier evidence;
    /// current as of `receipt_oid`").
    pub channel: VerdictChannel,
    /// `"anchor_obsolete"` | `"superseded_by"` — always negative (reinstated
    /// verdicts are filtered out before a row is ever constructed).
    pub verdict: &'static str,
    pub receipt_oid: Option<String>,
}

/// The two known container separators `import::backfill::container_spans`
/// mints (`"::"` for Rust, `"."` for the rest) — see this module's doc
/// comment on the suffix fallback.
const CONTAINER_SEPARATORS: [&str; 2] = ["::", "."];

/// DISTINCT non-NULL `witness_ledger.symbol` values recorded for every
/// given `(project, file)` pair, keyed by pair — ONE query per ≤400-pair
/// batch (this used to be one query per file, all under the storage mutex).
/// Small (one row per symbol ever stamped per file), and hits
/// `idx_witness_ledger_lookup(project, file, symbol)`.
fn witness_symbols_for_files(
    conn: &Connection,
    files: &[(String, String)],
) -> Result<BTreeMap<(String, String), Vec<String>>> {
    let mut out: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    if files.is_empty() {
        return Ok(out);
    }
    // ≤400 pairs (800 bind variables) per statement — same
    // SQLITE_MAX_VARIABLE_NUMBER reasoning as `codegraph::nodes_for_conversations`.
    const BATCH: usize = 400;
    for batch in files.chunks(BATCH) {
        let pair_filter: Vec<String> = (0..batch.len())
            .map(|i| format!("(project = ?{} AND file = ?{})", 2 * i + 1, 2 * i + 2))
            .collect();
        let sql = format!(
            "SELECT DISTINCT project, file, symbol FROM witness_ledger
             WHERE symbol IS NOT NULL AND ({})",
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
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok((
                (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (key, symbol) = row?;
            out.entry(key).or_default().push(symbol);
        }
    }
    Ok(out)
}

/// `true` iff `s` is a `#N` collision variant (`base#2`, `base#3`, ...)
/// whose base is either exactly `bare_name` or container-qualified
/// `...::bare_name` / `....bare_name` — the poison condition for the whole
/// base name (see the module doc).
fn is_numbered_variant_of(s: &str, bare_name: &str) -> bool {
    let Some(pos) = s.rfind('#') else {
        return false;
    };
    let (base, digits) = (&s[..pos], &s[pos + 1..]);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    base == bare_name
        || CONTAINER_SEPARATORS
            .iter()
            .any(|sep| base.ends_with(format!("{sep}{bare_name}").as_str()))
}

/// Resolve `bare_name` (an unqualified `code_nodes.name`) against the set of
/// witness symbols recorded for its file — see the module doc's binding
/// algorithm. `None` means "no binding" (never stamped, ambiguous, or
/// poisoned by a `#N` collision variant of the same base).
fn resolve_bound_symbol(witness_symbols: &[String], bare_name: &str) -> Option<String> {
    if witness_symbols
        .iter()
        .any(|s| is_numbered_variant_of(s, bare_name))
    {
        return None; // #N collision variant present — abstain entirely.
    }
    if witness_symbols.iter().any(|s| s == bare_name) {
        return Some(bare_name.to_string());
    }
    let mut suffix_matches: Vec<&String> = witness_symbols
        .iter()
        .filter(|s| {
            CONTAINER_SEPARATORS
                .iter()
                .any(|sep| s.ends_with(format!("{sep}{bare_name}").as_str()))
        })
        .collect();
    suffix_matches.dedup();
    match suffix_matches.as_slice() {
        [only] => Some((*only).clone()),
        _ => None, // zero (never stamped) or 2+ (ambiguous) — abstain either way.
    }
}

/// Given conversation/chunk identifiers appearing in search results, resolve
/// their bound witnesses via the code graph and return, per conversation, a
/// `Vec` of `{file, symbol, channel: Demote|Annotate, verdict, receipt_oid}`
/// hits per `witness_verdicts::symbol_verdict_state`'s order-independent
/// two-channel rule (see the module doc's "Two-channel consumption" and
/// that module's "Symbol-level current state"). Conversation ids whose
/// symbols carry no uncancelled negative event are simply absent from the
/// returned map (never an empty `Vec`).
///
/// Cheap by construction: `nodes_for_conversations` uses the
/// `idx_code_nodes_first_conv`/`idx_code_nodes_last_conv` indexes,
/// `witness_symbols_for_file` uses `idx_witness_ledger_lookup`, and
/// `symbol_verdict_state` joins on that same index plus
/// `idx_witness_verdicts_witness` — no table in this path is ever scanned
/// in full.
pub fn witness_verdict_for_chunks(
    conn: &Connection,
    conversation_ids: &[String],
) -> Result<BTreeMap<String, Vec<ChunkWitnessVerdict>>> {
    let mut out: BTreeMap<String, Vec<ChunkWitnessVerdict>> = BTreeMap::new();
    if conversation_ids.is_empty() {
        return Ok(out);
    }
    let candidates: std::collections::HashSet<&str> =
        conversation_ids.iter().map(|s| s.as_str()).collect();

    let nodes: Vec<NodeRow> = codegraph::nodes_for_conversations(conn, conversation_ids)?;

    // node -> the input conversation_ids it is attributed to (first and/or
    // last touch; almost always the same single id, but both are checked so
    // neither channel silently loses a match).
    let mut node_convs: Vec<(usize, Vec<String>)> = Vec::with_capacity(nodes.len());
    for (idx, node) in nodes.iter().enumerate() {
        let mut convs = Vec::new();
        if candidates.contains(node.first_conv_id.as_str()) {
            convs.push(node.first_conv_id.clone());
        }
        if node.last_conv_id != node.first_conv_id
            && candidates.contains(node.last_conv_id.as_str())
        {
            convs.push(node.last_conv_id.clone());
        }
        if !convs.is_empty() {
            node_convs.push((idx, convs));
        }
    }

    // Batch the whole read path: 2 batched queries typical (+1 per 400 file
    // pairs, plus the pre-existing node lookup above). This used to loop
    // one witness-symbols query per file plus one `symbol_verdict_state`
    // query per bound symbol, all while holding the storage mutex:
    // (a) witness symbols for ALL distinct (project, file) pairs at once;
    // (b) verdict states for every symbol-level anchor in those same pairs.
    // Binding itself (`resolve_bound_symbol` — exact match, suffix fallback,
    // ambiguity abstention, #N collision poisoning) is unchanged and runs
    // in-memory against (a)'s per-file symbol sets.
    let mut file_keys: Vec<(String, String)> = Vec::new();
    {
        let mut seen: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        for (idx, _) in &node_convs {
            let node = &nodes[*idx];
            let key = (node.project.clone(), node.file.clone());
            if seen.insert(key.clone()) {
                file_keys.push(key);
            }
        }
    }
    let symbols_by_file = witness_symbols_for_files(conn, &file_keys)?;
    let states = witness_verdicts::symbol_verdict_states_for_files(conn, &file_keys)?;

    for (idx, convs) in &node_convs {
        let node = &nodes[*idx];
        let key = (node.project.clone(), node.file.clone());
        let empty: Vec<String> = Vec::new();
        let symbols = symbols_by_file.get(&key).unwrap_or(&empty);
        let Some(bound_symbol) = resolve_bound_symbol(symbols, &node.name) else {
            continue; // never stamped, or ambiguous — abstain.
        };

        let Some(state) = states.get(&(
            node.project.clone(),
            node.file.clone(),
            bound_symbol.clone(),
        )) else {
            continue; // never audited, or fully reinstated — nothing to report.
        };
        debug_assert!(
            state.representative.verdict.is_negative(),
            "symbol_verdict_states_for_files only surfaces negative representatives"
        );

        let hit = ChunkWitnessVerdict {
            file: node.file.clone(),
            symbol: Some(bound_symbol.clone()),
            channel: state.channel,
            verdict: state.representative.verdict.as_str(),
            receipt_oid: state.representative.receipt_oid.clone(),
        };
        for conv in convs {
            out.entry(conv.clone()).or_default().push(hit.clone());
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::codegraph::{upsert_node, NodeRow};
    use crate::storage::witness_ledger::{self, WitnessLedgerRow};
    use crate::storage::witness_verdicts::{self, VerdictKind, WitnessVerdictRow};

    fn open() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::storage::migrations::run(&conn).unwrap();
        conn
    }

    fn node(name: &str, first_conv: &str, last_conv: &str) -> NodeRow {
        NodeRow {
            id: crate::extraction::codegraph::node_id("proj", "/repo/src/lib.rs", "function", name),
            repo: "proj".into(),
            project: "proj".into(),
            file: "/repo/src/lib.rs".into(),
            lang: "rust".into(),
            kind: "function".into(),
            name: name.into(),
            fqname: String::new(),
            body_hash: String::new(),
            span_start: 1,
            span_end: 3,
            first_conv_id: first_conv.into(),
            last_conv_id: last_conv.into(),
            last_session_id: "sess".into(),
            repo_root: None,
            name_only: false,
            attribution: String::new(),
        }
    }

    fn ledger_row(symbol: &str, at_oid: &str, stamp: &str) -> WitnessLedgerRow {
        WitnessLedgerRow {
            id: 0,
            project: "proj".into(),
            file: "/repo/src/lib.rs".into(),
            symbol: Some(symbol.into()),
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
    fn resolve_bound_symbol_exact_match() {
        let symbols = vec!["foo".to_string(), "Bar::baz".to_string()];
        assert_eq!(resolve_bound_symbol(&symbols, "foo"), Some("foo".into()));
    }

    #[test]
    fn resolve_bound_symbol_unambiguous_suffix_match() {
        let symbols = vec!["Bar::baz".to_string()];
        assert_eq!(
            resolve_bound_symbol(&symbols, "baz"),
            Some("Bar::baz".into())
        );
    }

    #[test]
    fn resolve_bound_symbol_ambiguous_suffix_abstains() {
        let symbols = vec!["Foo::run".to_string(), "Baz::run".to_string()];
        assert_eq!(resolve_bound_symbol(&symbols, "run"), None);
    }

    #[test]
    fn resolve_bound_symbol_never_stamped_abstains() {
        let symbols = vec!["Foo::other".to_string()];
        assert_eq!(resolve_bound_symbol(&symbols, "run"), None);
    }

    #[test]
    fn numbered_collision_variant_poisons_exact_match() {
        // "run#2" alongside "run": the ledger knows of multiple same-named
        // spans — a bare "run" has no signal for which one, so binding
        // must abstain even though an exact match exists (H7).
        let symbols = vec!["run".to_string(), "run#2".to_string()];
        assert_eq!(resolve_bound_symbol(&symbols, "run"), None);
    }

    #[test]
    fn numbered_collision_variant_poisons_suffix_fallback() {
        // "Foo::run#2" alongside "Foo::run": the (otherwise unambiguous)
        // suffix fallback must abstain too.
        let symbols = vec!["Foo::run".to_string(), "Foo::run#2".to_string()];
        assert_eq!(resolve_bound_symbol(&symbols, "run"), None);
    }

    #[test]
    fn numbered_variant_of_a_different_base_does_not_poison() {
        // "Foo::other#2" shares no base with "run" — binding proceeds.
        let symbols = vec!["run".to_string(), "Foo::other#2".to_string()];
        assert_eq!(resolve_bound_symbol(&symbols, "run"), Some("run".into()));
    }

    #[test]
    fn non_numeric_hash_suffix_does_not_poison() {
        // A '#' whose suffix is not all digits is not a collision variant.
        let symbols = vec!["run".to_string(), "run#x".to_string()];
        assert_eq!(resolve_bound_symbol(&symbols, "run"), Some("run".into()));
    }

    #[test]
    fn empty_conversation_ids_returns_empty_map() {
        let conn = open();
        let out = witness_verdict_for_chunks(&conn, &[]).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn conversation_with_no_negative_verdict_is_absent_from_map() {
        let conn = open();
        upsert_node(&conn, &node("foo", "conv-1", "conv-1")).unwrap();
        witness_ledger::insert_witness(&conn, &ledger_row("foo", "aaa", "b3:1")).unwrap();
        // No verdict ever recorded — symbol is unaudited, not negative.
        let out = witness_verdict_for_chunks(&conn, &["conv-1".to_string()]).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn conversation_bound_to_obsolete_symbol_is_surfaced() {
        let conn = open();
        upsert_node(&conn, &node("foo", "conv-1", "conv-1")).unwrap();
        witness_ledger::insert_witness(&conn, &ledger_row("foo", "aaa", "b3:1")).unwrap();
        let wid = witness_ledger::latest_witness_for_symbol(
            &conn,
            "proj",
            "/repo/src/lib.rs",
            Some("foo"),
        )
        .unwrap()
        .unwrap()
        .id;
        witness_verdicts::insert_verdict_if_changed(
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

        let out = witness_verdict_for_chunks(&conn, &["conv-1".to_string()]).unwrap();
        let hits = out.get("conv-1").expect("conv-1 must have a hit");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file, "/repo/src/lib.rs");
        assert_eq!(hits[0].symbol.as_deref(), Some("foo"));
        assert_eq!(
            hits[0].channel,
            VerdictChannel::Demote,
            "vanished symbol (no witness intact at HEAD) is the demote channel"
        );
        assert_eq!(hits[0].verdict, "anchor_obsolete");
        assert_eq!(hits[0].receipt_oid.as_deref(), Some("head1"));
    }

    #[test]
    fn a_b_evolution_yields_annotate_not_demote() {
        // Plain evolution: the symbol's old witness (aaa) is superseded by
        // the new one (bbb), which IS the row at the observed HEAD. The
        // binding must surface on the ANNOTATE channel — no rank effect,
        // receipt from the supersession event — never Demote.
        let conn = open();
        upsert_node(&conn, &node("foo", "conv-1", "conv-1")).unwrap();
        witness_ledger::insert_witness(&conn, &ledger_row("foo", "aaa", "b3:1")).unwrap();
        witness_ledger::insert_witness(&conn, &ledger_row("foo", "bbb", "b3:2")).unwrap();
        let rows = conn
            .prepare("SELECT id, at_oid FROM witness_ledger")
            .unwrap()
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let id_of = |oid: &str| rows.iter().find(|(_, o)| o == oid).unwrap().0;
        witness_verdicts::insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: id_of("aaa"),
                verdict: VerdictKind::SupersededBy,
                successor_witness_id: Some(id_of("bbb")),
                receipt_oid: Some("bbb".into()),
                observed_head_oid: "bbb".into(),
            },
        )
        .unwrap();

        let out = witness_verdict_for_chunks(&conn, &["conv-1".to_string()]).unwrap();
        let hits = out.get("conv-1").expect("evolution must surface a hit");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].channel, VerdictChannel::Annotate);
        assert_eq!(hits[0].verdict, "superseded_by");
        assert_eq!(
            hits[0].receipt_oid.as_deref(),
            Some("bbb"),
            "annotation carries 'current as of <receipt_oid>'"
        );
    }

    #[test]
    fn a_b_a_full_history_yields_annotate_with_rejected_change_receipt() {
        // Full A -> B -> A revert history: A witnesses (c1, c3) are intact
        // at HEAD (c3); B's witness (c2) keeps an uncancelled superseded_by
        // (receipt c3). By the two-channel rule this is ANNOTATE — "the
        // symbol carries history of a rejected change" — with the
        // B-supersession receipt, and never Demote.
        let conn = open();
        upsert_node(&conn, &node("foo", "conv-1", "conv-1")).unwrap();
        witness_ledger::insert_witness(&conn, &ledger_row("foo", "c1", "b3:A")).unwrap();
        witness_ledger::insert_witness(&conn, &ledger_row("foo", "c2", "b3:B")).unwrap();
        witness_ledger::insert_witness(&conn, &ledger_row("foo", "c3", "b3:A")).unwrap();
        let id_of = |oid: &str| -> i64 {
            conn.query_row(
                "SELECT id FROM witness_ledger WHERE at_oid = ?1",
                rusqlite::params![oid],
                |r| r.get(0),
            )
            .unwrap()
        };
        // c1: superseded at HEAD c2, then reinstated at HEAD c3.
        witness_verdicts::insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: id_of("c1"),
                verdict: VerdictKind::SupersededBy,
                successor_witness_id: Some(id_of("c2")),
                receipt_oid: Some("c2".into()),
                observed_head_oid: "c2".into(),
            },
        )
        .unwrap();
        witness_verdicts::insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: id_of("c1"),
                verdict: VerdictKind::AnchorReinstated,
                successor_witness_id: None,
                receipt_oid: Some("c3".into()),
                observed_head_oid: "c3".into(),
            },
        )
        .unwrap();
        // c2 (the rejected B): superseded by the revert commit c3 — stays
        // negative-latest.
        witness_verdicts::insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: id_of("c2"),
                verdict: VerdictKind::SupersededBy,
                successor_witness_id: Some(id_of("c3")),
                receipt_oid: Some("c3".into()),
                observed_head_oid: "c3".into(),
            },
        )
        .unwrap();

        let out = witness_verdict_for_chunks(&conn, &["conv-1".to_string()]).unwrap();
        let hits = out.get("conv-1").expect("rejected-change history surfaces");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].channel, VerdictChannel::Annotate);
        assert_eq!(
            hits[0].receipt_oid.as_deref(),
            Some("c3"),
            "the receipt is the B-supersession's (the rejected change's undoing)"
        );
    }

    #[test]
    fn reinstated_verdict_cancels_and_is_not_surfaced() {
        let conn = open();
        upsert_node(&conn, &node("foo", "conv-1", "conv-1")).unwrap();
        witness_ledger::insert_witness(&conn, &ledger_row("foo", "aaa", "b3:1")).unwrap();
        let wid = witness_ledger::latest_witness_for_symbol(
            &conn,
            "proj",
            "/repo/src/lib.rs",
            Some("foo"),
        )
        .unwrap()
        .unwrap()
        .id;
        witness_verdicts::insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: wid,
                verdict: VerdictKind::SupersededBy,
                successor_witness_id: Some(999),
                receipt_oid: Some("head1".into()),
                observed_head_oid: "head1".into(),
            },
        )
        .unwrap();
        witness_verdicts::insert_verdict_if_changed(
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

        let out = witness_verdict_for_chunks(&conn, &["conv-1".to_string()]).unwrap();
        assert!(
            out.is_empty(),
            "reinstated symbol must not be surfaced as stale"
        );
    }

    #[test]
    fn suffix_fallback_binds_qualified_ledger_symbol() {
        let conn = open();
        // code_nodes carries the bare name "run"; the ledger has it
        // container-qualified as "Worker::run" — must still bind.
        upsert_node(&conn, &node("run", "conv-1", "conv-1")).unwrap();
        witness_ledger::insert_witness(&conn, &ledger_row("Worker::run", "aaa", "b3:1")).unwrap();
        let wid = witness_ledger::latest_witness_for_symbol(
            &conn,
            "proj",
            "/repo/src/lib.rs",
            Some("Worker::run"),
        )
        .unwrap()
        .unwrap()
        .id;
        witness_verdicts::insert_verdict_if_changed(
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

        let out = witness_verdict_for_chunks(&conn, &["conv-1".to_string()]).unwrap();
        let hits = out
            .get("conv-1")
            .expect("suffix-fallback bind must surface a hit");
        assert_eq!(hits[0].symbol.as_deref(), Some("Worker::run"));
    }

    #[test]
    fn ambiguous_suffix_match_abstains_even_with_a_negative_verdict() {
        let conn = open();
        upsert_node(&conn, &node("run", "conv-1", "conv-1")).unwrap();
        witness_ledger::insert_witness(&conn, &ledger_row("Foo::run", "aaa", "b3:1")).unwrap();
        witness_ledger::insert_witness(&conn, &ledger_row("Bar::run", "bbb", "b3:2")).unwrap();
        let foo_wid = witness_ledger::latest_witness_for_symbol(
            &conn,
            "proj",
            "/repo/src/lib.rs",
            Some("Foo::run"),
        )
        .unwrap()
        .unwrap()
        .id;
        witness_verdicts::insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: foo_wid,
                verdict: VerdictKind::AnchorObsolete,
                successor_witness_id: None,
                receipt_oid: Some("head1".into()),
                observed_head_oid: "head1".into(),
            },
        )
        .unwrap();

        let out = witness_verdict_for_chunks(&conn, &["conv-1".to_string()]).unwrap();
        assert!(
            out.is_empty(),
            "ambiguous (file, bare-name) suffix match must abstain, never guess"
        );
    }

    #[test]
    fn last_conv_id_channel_also_binds() {
        let conn = open();
        upsert_node(&conn, &node("foo", "conv-intro", "conv-last")).unwrap();
        witness_ledger::insert_witness(&conn, &ledger_row("foo", "aaa", "b3:1")).unwrap();
        let wid = witness_ledger::latest_witness_for_symbol(
            &conn,
            "proj",
            "/repo/src/lib.rs",
            Some("foo"),
        )
        .unwrap()
        .unwrap()
        .id;
        witness_verdicts::insert_verdict_if_changed(
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

        let out = witness_verdict_for_chunks(&conn, &["conv-last".to_string()]).unwrap();
        assert!(
            out.contains_key("conv-last"),
            "last_conv_id must also resolve a binding"
        );
    }
}
