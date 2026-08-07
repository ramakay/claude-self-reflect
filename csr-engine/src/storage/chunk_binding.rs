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
//! Binding considers the complete suffix-compatible candidate set: every
//! symbol that is either exactly `name` or ends with a known container
//! separator (`"::"` or `"."`) immediately followed by `name`. If EXACTLY
//! ONE such symbol exists, bind to it. A bare exact spelling is not preferred
//! over a qualified spelling with the same terminal name: `shared` plus
//! `outer.shared` is two candidates and therefore ambiguous. If two or more
//! exist (e.g. `Foo::run` and
//!    `Bar::run` both present for the same file, and `code_nodes` only says
//!    `"run"`), the match is genuinely ambiguous — **abstain**: no binding
//!    is produced for that node, rather than guessing.
//!
//! # Append-only identity-lineage preference
//!
//! `codegraph stamp-spans` re-extracts current anchors with the production
//! extractor and appends them as `source_kind = 'backfill_rederived_v2'`.
//! Each run has a unique `source_id` and a `witness_generations` manifest.
//! Binding ignores incomplete runs, rejects generations not causally at or
//! behind the repository's current HEAD, and chooses the one causal maximum
//! COMPLETE generation (newest completed run breaks same-HEAD ties). The
//! manifest is mutable bookkeeping; `witness_ledger` remains append-only.
//! Old bare and `#N` rows remain auditable but cannot poison the selected
//! corrected lineage.
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
use std::collections::{BTreeMap, VecDeque};

use super::codegraph::{self, NodeRow};
use super::witness_verdicts::{self, VerdictChannel};

use super::witness_ledger::WITNESS_EXTRACTOR_VERSION as CURRENT_EXTRACTOR_VERSION;

#[cfg(test)]
thread_local! {
    static ANCESTRY_COMPARISONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn compare_commits(
    auditor: &codewitness::Auditor,
    left: codewitness::ObjectId,
    right: codewitness::ObjectId,
) -> codewitness::Result<codewitness::CausalOrder> {
    #[cfg(test)]
    ANCESTRY_COMPARISONS.with(|count| count.set(count.get() + 1));
    codewitness::causal::compare(auditor.repo(), left, right)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LineageSelection {
    Legacy,
    Derived(String),
    Abstain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LineageCacheKey {
    project: String,
    file: String,
    current_head: String,
    max_manifest_id: i64,
}

const LINEAGE_CACHE_CAPACITY: usize = 128;
type LineageCache = VecDeque<(LineageCacheKey, LineageSelection)>;

struct LineageContext {
    cache: LineageCache,
    auditors: BTreeMap<String, Option<(codewitness::Auditor, codewitness::ObjectId)>>,
}

impl LineageContext {
    fn new() -> Self {
        Self {
            cache: VecDeque::with_capacity(LINEAGE_CACHE_CAPACITY),
            auditors: BTreeMap::new(),
        }
    }

    fn cached_lineage(&mut self, key: &LineageCacheKey) -> Option<LineageSelection> {
        let position = self
            .cache
            .iter()
            .position(|(candidate, _)| candidate == key)?;
        let entry = self
            .cache
            .remove(position)
            .expect("position came from the same lineage cache");
        let selection = entry.1.clone();
        self.cache.push_front(entry);
        Some(selection)
    }

    fn cache_lineage(&mut self, key: LineageCacheKey, selection: &LineageSelection) {
        if let Some(position) = self
            .cache
            .iter()
            .position(|(candidate, _)| candidate == &key)
        {
            self.cache.remove(position);
        }
        self.cache.push_front((key, selection.clone()));
        self.cache.truncate(LINEAGE_CACHE_CAPACITY);
    }

    fn repository(
        &mut self,
        repo_root: &str,
    ) -> Option<&(codewitness::Auditor, codewitness::ObjectId)> {
        self.auditors
            .entry(repo_root.to_string())
            .or_insert_with(|| {
                let auditor = codewitness::Auditor::open(repo_root).ok()?;
                let head = auditor.repo().head_id().ok()?.detach();
                Some((auditor, head))
            })
            .as_ref()
    }
}

type FileKey = (String, String);
type SymbolsByFile = BTreeMap<FileKey, Vec<String>>;
type LineagesByFile = BTreeMap<FileKey, LineageSelection>;

fn selected_lineage(
    conn: &Connection,
    project: &str,
    file: &str,
    context: &mut LineageContext,
) -> Result<LineageSelection> {
    let (max_manifest_id, manifest_count, rooted_count, min_root, max_root): (
        Option<i64>,
        i64,
        i64,
        Option<String>,
        Option<String>,
    ) = conn.query_row(
        "SELECT MAX(id), COUNT(*), COUNT(repo_root), MIN(repo_root), MAX(repo_root)
         FROM witness_generations
         WHERE project = ?1 AND file = ?2 AND status = 'complete'",
        rusqlite::params![project, file],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?;
    let Some(max_manifest_id) = max_manifest_id else {
        return Ok(LineageSelection::Legacy);
    };

    // Every COMPLETE manifest participates in the trust decision. A missing,
    // empty, or conflicting root makes the whole lineage ambiguous; never use
    // a first-non-NULL compatibility fallback. Production publication always
    // supplies the resolved root when it constructs the COMPLETE manifest.
    if rooted_count != manifest_count || min_root != max_root {
        return Ok(LineageSelection::Abstain);
    }
    let Some(repo_root) = min_root.filter(|root| !root.trim().is_empty()) else {
        return Ok(LineageSelection::Abstain);
    };
    let Some((_, current_head)) = context.repository(&repo_root) else {
        return Ok(LineageSelection::Abstain);
    };
    let current_head = *current_head;

    // This small per-binding-call LRU is deliberately scoped to one SQLite
    // snapshot, so separate/restored databases cannot share a cached lineage.
    // A new COMPLETE publication changes max_manifest_id and a checkout
    // changes current_head, providing natural invalidation within the call.
    let cache_key = LineageCacheKey {
        project: project.to_string(),
        file: file.to_string(),
        current_head: current_head.to_string(),
        max_manifest_id,
    };
    if let Some(selection) = context.cached_lineage(&cache_key) {
        return Ok(selection);
    }

    let (auditor, _) = context
        .repository(&repo_root)
        .expect("repository and HEAD were resolved above");

    // Collapse publications at the same HEAD and extractor version in SQL
    // before touching the commit graph. MAX(id) implements the established
    // newest-manifest tie within one version without merging versioned
    // ledger lineages.
    let mut statement = conn.prepare(
        "SELECT generation_id, head_oid, extractor_version
         FROM witness_generations
         WHERE id IN (
             SELECT MAX(id)
             FROM witness_generations
             WHERE project = ?1 AND file = ?2 AND status = 'complete'
             GROUP BY head_oid, extractor_version
         )
         ORDER BY id DESC",
    )?;
    let generations = statement
        .query_map(rusqlite::params![project, file], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut eligible = Vec::new();
    for (generation_id, head_oid, extractor_version) in generations {
        let Ok(head) = head_oid.parse::<codewitness::ObjectId>() else {
            continue;
        };
        if matches!(
            compare_commits(auditor, head, current_head),
            Ok(codewitness::CausalOrder::AncestorOf | codewitness::CausalOrder::Equal)
        ) {
            eligible.push((generation_id, head_oid, extractor_version, head));
        }
    }
    if eligible.is_empty() {
        let selection = LineageSelection::Abstain;
        context.cache_lineage(cache_key, &selection);
        return Ok(selection);
    }

    // Choose the causal maximum HEAD, not the row inserted last. Multiple
    // incomparable maxima are ambiguous and therefore abstain. At the same
    // HEAD, newest COMPLETE run wins; unique run ids prevent cross-run union.
    let mut maxima: Vec<&(String, String, String, codewitness::ObjectId)> = Vec::new();
    for candidate in &eligible {
        let dominated = eligible.iter().any(|other| {
            if candidate.1 == other.1 {
                return false;
            }
            matches!(
                compare_commits(auditor, candidate.3, other.3),
                Ok(codewitness::CausalOrder::AncestorOf)
            )
        });
        if !dominated {
            maxima.push(candidate);
        }
    }
    // Cross-version rule at the one causal-maximum HEAD: prefer the current
    // engine's extractor; if absent, accept exactly one other version; if
    // multiple non-current versions coexist, abstain rather than false-bind.
    // Different causal-maximum HEADs remain ambiguous regardless of version.
    let selection = match maxima.first() {
        Some(first) if maxima.iter().all(|generation| generation.1 == first.1) => maxima
            .iter()
            .find(|generation| generation.2 == CURRENT_EXTRACTOR_VERSION)
            .map_or_else(
                || match maxima.as_slice() {
                    [generation] => LineageSelection::Derived(generation.0.clone()),
                    _ => LineageSelection::Abstain,
                },
                |generation| LineageSelection::Derived(generation.0.clone()),
            ),
        _ => LineageSelection::Abstain,
    };
    context.cache_lineage(cache_key, &selection);
    Ok(selection)
}

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

/// DISTINCT non-NULL `witness_ledger.symbol` values for the selected COMPLETE
/// generation (or legacy fallback) of every `(project,file)` pair. Generation
/// selection consults git ancestry; symbol lookup hits
/// `idx_witness_ledger_lookup(project,file,symbol)`.
fn witness_symbols_for_files(
    conn: &Connection,
    files: &[(String, String)],
) -> Result<(SymbolsByFile, LineagesByFile)> {
    let mut out: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut lineages = BTreeMap::new();
    if files.is_empty() {
        return Ok((out, lineages));
    }
    let mut lineage_context = LineageContext::new();
    for (project, file) in files {
        let lineage = selected_lineage(conn, project, file, &mut lineage_context)?;
        let (predicate, source_id): (&str, Option<&str>) = match &lineage {
            LineageSelection::Legacy => (
                "wl.source_kind <> 'backfill_rederived_v2' AND ?3 IS NULL",
                None,
            ),
            LineageSelection::Derived(source_id) => (
                "wl.source_kind = 'backfill_rederived_v2' AND wl.source_id = ?3",
                Some(source_id),
            ),
            LineageSelection::Abstain => {
                lineages.insert((project.clone(), file.clone()), lineage);
                continue;
            }
        };
        let sql = format!(
            "SELECT DISTINCT wl.symbol FROM witness_ledger wl
             WHERE wl.project = ?1 AND wl.file = ?2 AND wl.symbol IS NOT NULL
               AND {predicate}"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![project, file, source_id], |row| {
            row.get::<_, String>(0)
        })?;
        for row in rows {
            out.entry((project.clone(), file.clone()))
                .or_default()
                .push(row?);
        }
        lineages.insert((project.clone(), file.clone()), lineage);
    }
    Ok((out, lineages))
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
    let mut suffix_matches: Vec<&String> = witness_symbols
        .iter()
        .filter(|s| {
            s.as_str() == bare_name
                || CONTAINER_SEPARATORS
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
    // One deferred read transaction gives candidate generation selection and
    // verdict-state lookup the same WAL snapshot. A concurrent publisher can
    // complete before or after this read, never between its two decisions.
    let tx = conn.unchecked_transaction()?;
    let candidates: std::collections::HashSet<&str> =
        conversation_ids.iter().map(|s| s.as_str()).collect();

    let nodes: Vec<NodeRow> = codegraph::nodes_for_conversations(&tx, conversation_ids)?;

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

    // Resolve one lineage and candidate set per file, then batch verdict
    // states for every bound anchor. Both phases share the transaction above:
    // (a) witness symbols for each distinct (project,file) selected lineage;
    // (b) verdict states for every symbol-level anchor in those lineages.
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
    let (symbols_by_file, lineages) = witness_symbols_for_files(&tx, &file_keys)?;

    let mut resolved = Vec::new();
    let mut state_anchors = Vec::new();
    let mut seen_anchors = std::collections::HashSet::new();

    for (idx, convs) in &node_convs {
        let node = &nodes[*idx];
        let key = (node.project.clone(), node.file.clone());
        let empty: Vec<String> = Vec::new();
        let symbols = symbols_by_file.get(&key).unwrap_or(&empty);
        let Some(bound_symbol) = resolve_bound_symbol(symbols, &node.name) else {
            continue; // never stamped, or ambiguous — abstain.
        };

        let source_id = match lineages.get(&key) {
            Some(LineageSelection::Derived(source_id)) => Some(source_id.clone()),
            Some(LineageSelection::Legacy) => None,
            _ => continue,
        };
        let anchor = (
            node.project.clone(),
            node.file.clone(),
            bound_symbol.clone(),
            source_id,
        );
        if seen_anchors.insert(anchor.clone()) {
            state_anchors.push(anchor);
        }
        resolved.push((*idx, convs, bound_symbol));
    }

    let states = witness_verdicts::symbol_verdict_states_for_lineages(&tx, &state_anchors)?;

    for (idx, convs, bound_symbol) in resolved {
        let node = &nodes[idx];

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

    tx.commit()?;
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

    fn install_generation_fixture_table(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS witness_generations (
                id INTEGER PRIMARY KEY,
                generation_id TEXT NOT NULL UNIQUE,
                project TEXT NOT NULL,
                file TEXT NOT NULL,
                repo_root TEXT,
                head_oid TEXT NOT NULL,
                extractor_version TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                completed_at TEXT
            );",
        )
        .unwrap();
    }

    fn generation(
        conn: &Connection,
        generation_id: &str,
        head_oid: &str,
        status: &str,
        repo_root: Option<&str>,
    ) {
        generation_with_extractor(
            conn,
            generation_id,
            head_oid,
            "codegraph-v3",
            status,
            repo_root,
        );
    }

    fn generation_with_extractor(
        conn: &Connection,
        generation_id: &str,
        head_oid: &str,
        extractor_version: &str,
        status: &str,
        repo_root: Option<&str>,
    ) {
        install_generation_fixture_table(conn);
        conn.execute(
            "INSERT INTO witness_generations
             (generation_id, project, file, repo_root, head_oid, extractor_version, status,
              completed_at)
             VALUES (?1, 'proj', '/repo/src/lib.rs', ?2, ?3, ?4, ?5,
                     CASE WHEN ?5 = 'complete' THEN datetime('now') END)",
            rusqlite::params![
                generation_id,
                repo_root,
                head_oid,
                extractor_version,
                status
            ],
        )
        .unwrap();
    }

    fn select_test_lineage(conn: &Connection) -> LineageSelection {
        selected_lineage(conn, "proj", "/repo/src/lib.rs", &mut LineageContext::new()).unwrap()
    }

    fn git(args: &[&str], repo: &std::path::Path) -> std::process::Output {
        std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            // Scrub repo-pinning vars git exports to hook subprocesses —
            // under `git commit` (pre-commit runs this suite) GIT_DIR points
            // at the OUTER repo and would hijack these temp-repo commands.
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_COMMON_DIR")
            .env("GIT_AUTHOR_NAME", "CSR Test")
            .env("GIT_AUTHOR_EMAIL", "csr@example.invalid")
            .env("GIT_COMMITTER_NAME", "CSR Test")
            .env("GIT_COMMITTER_EMAIL", "csr@example.invalid")
            .output()
            .unwrap()
    }

    fn initialized_repo() -> (tempfile::TempDir, String) {
        let tmp = tempfile::tempdir().unwrap();
        assert!(git(&["init", "-q"], tmp.path()).status.success());
        std::fs::write(
            tmp.path().join("history.txt"),
            format!("{}\n", tmp.path().display()),
        )
        .unwrap();
        assert!(git(&["add", "history.txt"], tmp.path()).status.success());
        assert!(git(&["commit", "-q", "-m", "one"], tmp.path())
            .status
            .success());
        let head = String::from_utf8(git(&["rev-parse", "HEAD"], tmp.path()).stdout)
            .unwrap()
            .trim()
            .to_string();
        (tmp, head)
    }

    #[test]
    fn resolve_bound_symbol_exact_match() {
        let symbols = vec!["foo".to_string(), "Bar::baz".to_string()];
        assert_eq!(resolve_bound_symbol(&symbols, "foo"), Some("foo".into()));
    }

    #[test]
    fn resolve_bound_symbol_bare_exact_plus_qualified_variant_abstains() {
        // An exact bare spelling is not stronger evidence than a qualified
        // spelling with the same terminal name. Returning `shared` here is
        // the rejected mixed bare/qualified false bind.
        let symbols = vec!["shared".to_string(), "outer.shared".to_string()];
        assert_eq!(resolve_bound_symbol(&symbols, "shared"), None);
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
    fn collision_probe_bindability_improves_on_newest_rederived_lineage() {
        let (repo, old_head) = initialized_repo();
        std::fs::write(repo.path().join("history.txt"), "two\n").unwrap();
        assert!(git(&["add", "history.txt"], repo.path()).status.success());
        assert!(git(&["commit", "-q", "-m", "two"], repo.path())
            .status
            .success());
        let new_head = String::from_utf8(git(&["rev-parse", "HEAD"], repo.path()).stdout)
            .unwrap()
            .trim()
            .to_string();
        let repo_root = repo.path().to_str();
        let conn = open();
        upsert_node(&conn, &node("shared", "conv-1", "conv-1")).unwrap();

        witness_ledger::insert_witness(&conn, &ledger_row("shared", "old", "b3:old-1")).unwrap();
        witness_ledger::insert_witness(&conn, &ledger_row("shared#2", "old", "b3:old-2")).unwrap();

        let mut prior = ledger_row("shared", &old_head, "b3:gen1-1");
        prior.source_kind = "backfill_rederived_v2".into();
        prior.source_id = Some("run-prior".into());
        witness_ledger::insert_witness(&conn, &prior).unwrap();
        let mut prior_numbered = ledger_row("shared#2", &old_head, "b3:gen1-2");
        prior_numbered.source_kind = "backfill_rederived_v2".into();
        prior_numbered.source_id = Some("run-prior".into());
        witness_ledger::insert_witness(&conn, &prior_numbered).unwrap();
        generation(&conn, "run-prior", &old_head, "complete", repo_root);

        let before = witness_verdict_for_chunks(&conn, &["conv-1".to_string()]).unwrap();
        assert!(
            before.is_empty(),
            "legacy #N collision poison is executably unbindable"
        );

        let mut fresh = ledger_row("Outer::shared", &new_head, "b3:new");
        fresh.source_kind = "backfill_rederived_v2".into();
        fresh.source_id = Some("run-fresh".into());
        witness_ledger::insert_witness(&conn, &fresh).unwrap();
        generation(&conn, "run-fresh", &new_head, "complete", repo_root);
        let fresh_id = witness_ledger::latest_witness_for_symbol(
            &conn,
            "proj",
            "/repo/src/lib.rs",
            Some("Outer::shared"),
        )
        .unwrap()
        .unwrap()
        .id;
        witness_verdicts::insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: fresh_id,
                verdict: VerdictKind::AnchorObsolete,
                successor_witness_id: None,
                receipt_oid: Some("head".into()),
                observed_head_oid: "head".into(),
            },
        )
        .unwrap();

        assert_eq!(
            select_test_lineage(&conn),
            LineageSelection::Derived("run-fresh".into())
        );

        let out = witness_verdict_for_chunks(&conn, &["conv-1".to_string()]).unwrap();
        let hits = out
            .get("conv-1")
            .expect("the fresh unique suffix lineage must be bindable");
        assert_eq!(hits[0].symbol.as_deref(), Some("Outer::shared"));
    }

    #[test]
    fn incomplete_rederived_generation_cannot_publish_a_survivor_binding() {
        let conn = open();
        upsert_node(&conn, &node("shared", "conv-1", "conv-1")).unwrap();

        let mut survivor = ledger_row("Outer::shared", "head", "b3:survivor");
        survivor.source_kind = "backfill_rederived_v2".into();
        survivor.source_id = Some("run-incomplete".into());
        witness_ledger::insert_witness(&conn, &survivor).unwrap();
        generation(&conn, "run-incomplete", "head", "incomplete", None);

        let survivor_id = witness_ledger::latest_witness_for_symbol(
            &conn,
            "proj",
            "/repo/src/lib.rs",
            Some("Outer::shared"),
        )
        .unwrap()
        .unwrap()
        .id;
        witness_verdicts::insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: survivor_id,
                verdict: VerdictKind::AnchorObsolete,
                successor_witness_id: None,
                receipt_oid: Some("head".into()),
                observed_head_oid: "head".into(),
            },
        )
        .unwrap();

        let out = witness_verdict_for_chunks(&conn, &["conv-1".to_string()]).unwrap();
        assert!(
            out.is_empty(),
            "an incomplete generation's sole survivor must never become a unique bind"
        );
    }

    #[test]
    fn legacy_negative_verdict_does_not_leak_into_clean_complete_generation() {
        let conn = open();
        upsert_node(&conn, &node("shared", "conv-1", "conv-1")).unwrap();

        witness_ledger::insert_witness(&conn, &ledger_row("shared", "old", "b3:old")).unwrap();
        let legacy_id = witness_ledger::latest_witness_for_symbol(
            &conn,
            "proj",
            "/repo/src/lib.rs",
            Some("shared"),
        )
        .unwrap()
        .unwrap()
        .id;
        witness_verdicts::insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: legacy_id,
                verdict: VerdictKind::AnchorObsolete,
                successor_witness_id: None,
                receipt_oid: Some("old".into()),
                observed_head_oid: "old".into(),
            },
        )
        .unwrap();

        let mut corrected = ledger_row("shared", "new", "b3:new");
        corrected.source_kind = "backfill_rederived_v2".into();
        corrected.source_id = Some("run-clean".into());
        witness_ledger::insert_witness(&conn, &corrected).unwrap();
        generation(&conn, "run-clean", "new", "complete", None);

        let out = witness_verdict_for_chunks(&conn, &["conv-1".to_string()]).unwrap();
        assert!(
            out.is_empty(),
            "state must come from the selected clean generation, not a legacy spelling"
        );
    }

    #[test]
    fn complete_manifest_without_repo_root_abstains() {
        let conn = open();
        generation(&conn, "run-null-root", "deadbeef", "complete", None);

        assert_eq!(select_test_lineage(&conn), LineageSelection::Abstain);
    }

    #[test]
    fn complete_manifests_with_different_repo_roots_abstain() {
        let (source, head) = initialized_repo();
        let clone_parent = tempfile::tempdir().unwrap();
        let clone = clone_parent.path().join("clone");
        let clone_output = std::process::Command::new("git")
            .args(["clone", "-q"])
            .arg(source.path())
            .arg(&clone)
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_COMMON_DIR")
            .output()
            .unwrap();
        assert!(clone_output.status.success());

        let conn = open();
        generation(
            &conn,
            "run-source",
            &head,
            "complete",
            source.path().to_str(),
        );
        generation(&conn, "run-clone", &head, "complete", clone.to_str());

        assert_eq!(select_test_lineage(&conn), LineageSelection::Abstain);
    }

    #[test]
    fn same_head_generations_collapse_before_ancestry_checks() {
        let (repo, head) = initialized_repo();
        let conn = open();
        for index in 0..6 {
            generation(
                &conn,
                &format!("run-{index}"),
                &head,
                "complete",
                repo.path().to_str(),
            );
        }

        let mut context = LineageContext::new();
        ANCESTRY_COMPARISONS.with(|count| count.set(0));
        assert_eq!(
            selected_lineage(&conn, "proj", "/repo/src/lib.rs", &mut context).unwrap(),
            LineageSelection::Derived("run-5".into())
        );
        let comparisons = ANCESTRY_COMPARISONS.with(std::cell::Cell::get);
        assert!(
            comparisons <= 1,
            "same-HEAD collapse should require at most one HEAD eligibility comparison, got {comparisons}"
        );

        ANCESTRY_COMPARISONS.with(|count| count.set(0));
        assert_eq!(
            selected_lineage(&conn, "proj", "/repo/src/lib.rs", &mut context).unwrap(),
            LineageSelection::Derived("run-5".into())
        );
        assert_eq!(
            ANCESTRY_COMPARISONS.with(std::cell::Cell::get),
            0,
            "unchanged manifest maximum and HEAD should hit the lineage LRU"
        );
    }

    #[test]
    fn same_head_prefers_current_extractor_over_later_non_current_generation() {
        let (repo, head) = initialized_repo();
        let conn = open();
        generation_with_extractor(
            &conn,
            "run-current",
            &head,
            "codegraph-v3",
            "complete",
            repo.path().to_str(),
        );
        generation_with_extractor(
            &conn,
            "run-v9-later",
            &head,
            "codegraph-v9",
            "complete",
            repo.path().to_str(),
        );

        assert_eq!(
            select_test_lineage(&conn),
            LineageSelection::Derived("run-current".into()),
            "the current extractor must win even when another version has the larger row id"
        );
    }

    #[test]
    fn same_head_with_multiple_non_current_extractors_abstains() {
        let (repo, head) = initialized_repo();
        let conn = open();
        generation_with_extractor(
            &conn,
            "run-v8",
            &head,
            "codegraph-v8",
            "complete",
            repo.path().to_str(),
        );
        generation_with_extractor(
            &conn,
            "run-v9",
            &head,
            "codegraph-v9",
            "complete",
            repo.path().to_str(),
        );

        assert_eq!(select_test_lineage(&conn), LineageSelection::Abstain);
    }

    #[test]
    fn same_head_collapse_limits_ancestry_checks_per_extractor_group() {
        let (repo, head) = initialized_repo();
        let conn = open();
        for index in 0..4 {
            generation_with_extractor(
                &conn,
                &format!("run-v9-{index}"),
                &head,
                "codegraph-v9",
                "complete",
                repo.path().to_str(),
            );
            generation_with_extractor(
                &conn,
                &format!("run-current-{index}"),
                &head,
                "codegraph-v3",
                "complete",
                repo.path().to_str(),
            );
        }

        let mut context = LineageContext::new();
        ANCESTRY_COMPARISONS.with(|count| count.set(0));
        assert_eq!(
            selected_lineage(&conn, "proj", "/repo/src/lib.rs", &mut context).unwrap(),
            LineageSelection::Derived("run-current-3".into())
        );
        assert!(
            ANCESTRY_COMPARISONS.with(std::cell::Cell::get) <= 2,
            "four runs in each of two extractor groups require at most one eligibility comparison per group"
        );
    }

    #[test]
    fn causally_newer_complete_head_wins_over_older_head_published_later() {
        use std::process::Command;

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(repo)
                .env_remove("GIT_DIR")
                .env_remove("GIT_INDEX_FILE")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_OBJECT_DIRECTORY")
                .env_remove("GIT_COMMON_DIR")
                .env("GIT_AUTHOR_NAME", "CSR Test")
                .env("GIT_AUTHOR_EMAIL", "csr@example.invalid")
                .env("GIT_COMMITTER_NAME", "CSR Test")
                .env("GIT_COMMITTER_EMAIL", "csr@example.invalid")
                .output()
                .unwrap()
        };
        assert!(git(&["init", "-q"]).status.success());
        std::fs::write(repo.join("history.txt"), "one\n").unwrap();
        assert!(git(&["add", "history.txt"]).status.success());
        assert!(git(&["commit", "-q", "-m", "one"]).status.success());
        let old_head = String::from_utf8(git(&["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();
        std::fs::write(repo.join("history.txt"), "two\n").unwrap();
        assert!(git(&["add", "history.txt"]).status.success());
        assert!(git(&["commit", "-q", "-m", "two"]).status.success());
        let new_head = String::from_utf8(git(&["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();

        let conn = open();
        upsert_node(&conn, &node("shared", "conv-1", "conv-1")).unwrap();
        let repo_root = repo.to_string_lossy();

        let mut newer_a = ledger_row("Outer::shared", &new_head, "b3:new-a");
        newer_a.source_kind = "backfill_rederived_v2".into();
        newer_a.source_id = Some("run-new".into());
        witness_ledger::insert_witness(&conn, &newer_a).unwrap();
        let mut newer_b = ledger_row("Inner::shared", &new_head, "b3:new-b");
        newer_b.source_kind = "backfill_rederived_v2".into();
        newer_b.source_id = Some("run-new".into());
        witness_ledger::insert_witness(&conn, &newer_b).unwrap();
        generation(&conn, "run-new", &new_head, "complete", Some(&repo_root));

        let mut older = ledger_row("Outer::shared", &old_head, "b3:old");
        older.source_kind = "backfill_rederived_v2".into();
        older.source_id = Some("run-old-late".into());
        witness_ledger::insert_witness(&conn, &older).unwrap();
        generation(
            &conn,
            "run-old-late",
            &old_head,
            "complete",
            Some(&repo_root),
        );
        let older_id = witness_ledger::latest_witness_for_symbol(
            &conn,
            "proj",
            "/repo/src/lib.rs",
            Some("Outer::shared"),
        )
        .unwrap()
        .unwrap()
        .id;
        witness_verdicts::insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: older_id,
                verdict: VerdictKind::AnchorObsolete,
                successor_witness_id: None,
                receipt_oid: Some(new_head.clone()),
                observed_head_oid: new_head,
            },
        )
        .unwrap();

        let out = witness_verdict_for_chunks(&conn, &["conv-1".to_string()]).unwrap();
        assert!(
            out.is_empty(),
            "newer HEAD's two candidates must abstain; late older HEAD must not win by row id"
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
