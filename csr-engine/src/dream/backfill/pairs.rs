//! Dream backfill — pair generators (`.plans/dream-backfill-design.md` §3
//! "Stage 2 — pair generators", with the D2/D7/D8 round-2 deltas from §8
//! folded in, since that section overrides §3-6 wherever they conflict;
//! G-todo is CUT per D8, not implemented at all).
//!
//! Three deterministic, zero-LLM generators over `episode_index`
//! (materialized by [`crate::storage::dream_backfill`]), each emitting
//! ordered [`PairCandidate`]s that [`super::rank`] scores and gates:
//!
//! 1. [`generate_relapse_pairs`] (D2 — build first, highest priority):
//!    an episode that unknowingly re-touches a symbol already ruled
//!    retired (`superseded_by` / `anchor_obsolete`) — an unconscious
//!    regression.
//! 2. [`generate_ledger_pairs`]: an episode whose anchors touched a symbol
//!    later ruled retired, paired with a later episode that touched the
//!    successor witness (or the same file after the retirement).
//! 3. [`generate_era_pairs`] (D8: G-todo cut, theta fit against prev-chain
//!    co-clustering): per-project agglomerative clustering over episode
//!    vectors; topically-cohesive clusters spanning >=14 days emit ordered
//!    pairs, gated by a shared-rare-token/shared-anchor-symbol requirement
//!    so mere file-path overlap ("everything mentions cargo test") never
//!    qualifies.
//!
//! # The body_hash / stamp finding (deviation from the design's §2 claim)
//!
//! Design §2 claims episode `anchors[].body_hash` is "the same content-hash
//! universe as `witness_ledger.stamp`". **This is false at the
//! implementation level** and this module does not rely on it:
//! `extraction::anchors::hash_normalized` is a 16-hex-char truncated SHA-256
//! over text with ALL whitespace stripped, while `witness_ledger.stamp`
//! (`codewitness::stamp`) is a full BLAKE3 hash prefixed `b3:`/`b3n:` over
//! (at most) whitespace-*normalized* — never whitespace-*stripped* — bytes.
//! The two are different hash functions over different byte streams; a
//! direct string-equality join between the two would always be empty. Both
//! generators below therefore join at **symbol identity**
//! (`(project, file, symbol_name)`, exactly the key
//! `storage::witness_verdicts::symbol_verdict_state` already takes) plus a
//! **temporal** constraint (episode ts on the correct side of the verdict's
//! resolved event time), never at content-hash equality. This is a real
//! precision loss versus the design's aspiration — an episode is linked to
//! a symbol's CURRENT governing verdict, not proven to have captured the
//! exact witness row that verdict is about — but it is the only sound
//! choice given the hash-space mismatch, and "symbol precision" in the
//! task's own wording is satisfied literally (join granularity is the
//! symbol, not the file).
//!
//! Zero LLM calls anywhere in this module.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::process::Command;

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::death_time;
use super::family::{canon_file, Family};
use crate::hooks::intent::cosine_sim;
use crate::storage::dream_backfill::prev_chain_pairs;
use crate::storage::witness_ledger::{witness_by_id, WitnessLedgerRow};
use crate::storage::witness_verdicts::{symbol_verdict_state, SymbolVerdictState, VerdictKind};
use crate::temporal::parse_timestamp;

/// D3's obsolescence threshold reused nowhere here; this module reads
/// `episode_index` directly rather than via `storage::dream_backfill`'s
/// (test-only) row loader — same convention `dream::backfill::unfinished`
/// already established: each consumer defines its own narrow row shape
/// rather than sharing one wide loader.
#[derive(Debug, Clone)]
pub(super) struct EpisodeRow {
    pub(super) episode_id: String,
    /// P2: the episode's own `episode_index.session_id` — the join key
    /// [`session_index`]/[`episode_for_source`] use to prefer the
    /// ledger-encoded advocate/successor episode over geometric
    /// reconstruction (see [`generate_ledger_pairs`]'s doc comment).
    pub(super) session_id: String,
    pub(super) ts: DateTime<Utc>,
    pub(super) outcome: String,
    pub(super) todo_count: i64,
    pub(super) blockers: Option<String>,
    pub(super) request: String,
    pub(super) completed: String,
    pub(super) next_steps: Option<String>,
    pub(super) anchors: Vec<AnchorRow>,
    pub(super) present_at_head: Option<bool>,
}

/// Mirrors `extraction::anchors::FunctionAnchor`'s JSON shape exactly
/// (`{file, node_kind, name, body_hash}`) — a local, module-private
/// deserialize target rather than importing that type directly, same
/// convention `storage::dream_backfill::EpisodeAnchor` already established.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(super) struct AnchorRow {
    pub(super) file: String,
    #[allow(dead_code)] // kept for shape-fidelity with the source JSON; not read
    pub(super) node_kind: String,
    pub(super) name: String,
    pub(super) body_hash: String,
}

/// Load every `episode_index` row for `project`, oldest-first
/// (`ORDER BY ts` is not usable directly — `ts` is stored as free-form
/// text — so this loads then sorts by the *parsed* timestamp; rows whose
/// `ts` fails to parse are excluded, same policy
/// `dream::backfill::unfinished::later_candidates` applies: no ordering can
/// be asserted for them, so they cannot contribute evidence either way).
pub(super) fn load_project_episodes(conn: &Connection, project: &str) -> Result<Vec<EpisodeRow>> {
    let mut stmt = conn.prepare(
        "SELECT episode_id, ts, outcome, todo_count, blockers,
                request, completed, next_steps, anchors_json, present_at_head,
                session_id
         FROM episode_index WHERE project = ?1",
    )?;
    let mut rows = stmt
        .query_map(params![project], |row| {
            let anchors_json: String = row.get(8)?;
            let present_at_head: Option<i64> = row.get(9)?;
            let ts_raw: String = row.get(1)?;
            Ok((
                EpisodeRow {
                    episode_id: row.get(0)?,
                    session_id: row.get(10)?,
                    ts: Utc::now(), // placeholder, replaced below once parsed
                    outcome: row.get(2)?,
                    todo_count: row.get(3)?,
                    blockers: row.get(4)?,
                    request: row.get(5)?,
                    completed: row.get(6)?,
                    next_steps: row.get(7)?,
                    anchors: serde_json::from_str::<Vec<AnchorRow>>(&anchors_json)
                        .unwrap_or_default(),
                    present_at_head: present_at_head.map(|v| v != 0),
                },
                ts_raw,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter_map(|(mut ep, ts_raw)| {
            let ts = parse_timestamp(&ts_raw)?;
            ep.ts = ts;
            Some(ep)
        })
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| {
        a.ts.cmp(&b.ts)
            .then_with(|| a.episode_id.cmp(&b.episode_id))
    });
    Ok(rows)
}

/// Family-corpus episode loader (2026-08-26 design ruling: generators run
/// over the cross-project corpus). Concatenates [`load_project_episodes`]
/// over every member key, canonicalizes each anchor's file path
/// ([`canon_file`], so a worktree-checkout touch and a main-checkout touch
/// of the same symbol share one `(file, symbol)` identity in
/// [`index_by_symbol`]), and re-sorts the merged set oldest-first — the
/// same ordering contract per-project loading already guaranteed.
pub(super) fn load_family_episodes(conn: &Connection, family: &Family) -> Result<Vec<EpisodeRow>> {
    let mut rows = Vec::new();
    for member in &family.members {
        rows.extend(load_project_episodes(conn, member)?);
    }
    for ep in &mut rows {
        for a in &mut ep.anchors {
            let canon = canon_file(&a.file);
            if canon != a.file {
                a.file = canon;
            }
        }
    }
    rows.sort_by(|a, b| {
        a.ts.cmp(&b.ts)
            .then_with(|| a.episode_id.cmp(&b.episode_id))
    });
    Ok(rows)
}

/// The family's witness-ledger identity map: canonical `(file, symbol)` ->
/// every raw `(project, file)` variant the ledger actually recorded it
/// under (a symbol stamped from both the `repo` and `repo-subdir` cwd keys,
/// or from a worktree path, appears once here with all its variants).
/// Loaded once per family per gate run — one SQL pass instead of a
/// per-symbol probe.
pub(super) struct FamilyLedgerIndex {
    variants: HashMap<(String, String), Vec<(String, String)>>,
}

impl FamilyLedgerIndex {
    pub(super) fn load(conn: &Connection, family: &Family) -> Result<FamilyLedgerIndex> {
        let mut variants: HashMap<(String, String), Vec<(String, String)>> = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT project, file, symbol FROM witness_ledger
             WHERE project = ?1 AND symbol IS NOT NULL",
        )?;
        for member in &family.members {
            let rows = stmt
                .query_map(params![member], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (project, file, symbol) in rows {
                variants
                    .entry((canon_file(&file), symbol))
                    .or_default()
                    .push((project, file));
            }
        }
        // Deterministic variant order regardless of member iteration /
        // SQL row order.
        for v in variants.values_mut() {
            v.sort();
            v.dedup();
        }
        Ok(FamilyLedgerIndex { variants })
    }
}

/// The evidence [`family_symbol_verdict_state`] hands a generator: the
/// merged verdict state plus which raw ledger variant supplied the
/// representative (its REAL file path — [`resolve_event_time`] resolves the
/// receipt OID through the repo that path actually lives in).
pub(super) struct FamilyVerdict {
    pub(super) state: SymbolVerdictState,
    pub(super) variant_file: String,
}

/// Family-wide [`symbol_verdict_state`]: consult every raw `(project,
/// file)` variant the ledger recorded for this canonical `(file, symbol)`
/// identity, and merge. Merge policy (a judgment call, documented rather
/// than designed): the variant whose representative has the greatest
/// `witness_id` wins representative/channel (witness ids are
/// insertion-ordered, so this picks the newest ledger era's conclusion);
/// `negative_witness_ids` union across all variants (every funeral is
/// receipt material regardless of which key recorded it). `None` when no
/// variant has a negative latest verdict — exactly the per-project
/// function's own contract.
pub(super) fn family_symbol_verdict_state(
    conn: &Connection,
    ledger: &FamilyLedgerIndex,
    file: &str,
    symbol: &str,
) -> Result<Option<FamilyVerdict>> {
    let Some(variants) = ledger.variants.get(&(file.to_string(), symbol.to_string())) else {
        return Ok(None);
    };

    let mut best: Option<FamilyVerdict> = None;
    let mut all_negative_ids: Vec<i64> = Vec::new();
    for (project, real_file) in variants {
        let Some(state) = symbol_verdict_state(conn, project, real_file, Some(symbol))? else {
            continue;
        };
        all_negative_ids.extend(&state.negative_witness_ids);
        let replace = best
            .as_ref()
            .is_none_or(|b| state.representative.witness_id > b.state.representative.witness_id);
        if replace {
            best = Some(FamilyVerdict {
                state,
                variant_file: real_file.clone(),
            });
        }
    }
    Ok(best.map(|mut fv| {
        all_negative_ids.sort_unstable();
        all_negative_ids.dedup();
        fv.state.negative_witness_ids = all_negative_ids;
        fv
    }))
}

/// Which round-2-numbered generator produced a candidate. Matches
/// `dream_relations.generator`'s CHECK vocabulary
/// (`storage::migrations::run`) exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Generator {
    Ledger,
    Relapse,
    Era,
}

impl Generator {
    pub fn as_str(self) -> &'static str {
        match self {
            Generator::Ledger => "ledger",
            Generator::Relapse => "relapse",
            Generator::Era => "era",
        }
    }
}

/// Direction of a candidate pair. Matches `dream_relations.relation`'s CHECK
/// vocabulary exactly — there is no third value for relapse (D2 added a
/// GENERATOR, not a new relation), so relapse pairs pick the closer of the
/// two existing values; see [`generate_relapse_pairs`]'s doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    ReplacedBy,
    ExtendedBy,
}

impl Relation {
    pub fn as_str(self) -> &'static str {
        match self {
            Relation::ReplacedBy => "replaced_by",
            Relation::ExtendedBy => "extended_by",
        }
    }
}

/// D6: whether a candidate's event time was resolved via git (a real commit
/// lookup succeeded) or fell back to an observation timestamp. Matches
/// `dream_relations.oid_provenance`'s CHECK vocabulary exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OidProvenance {
    GitDerived,
    CreatedAtFallback,
}

impl OidProvenance {
    pub fn as_str(self) -> &'static str {
        match self {
            OidProvenance::GitDerived => "git_derived",
            OidProvenance::CreatedAtFallback => "created_at_fallback",
        }
    }
}

/// A generator's raw, pre-adjudication evidence for one candidate pair —
/// never a claim of "verified" (that is Stage 4/5's job), only "here is
/// what deterministic evidence says". `hashes` entries are self-labeled
/// (`"witness:<b3-stamp>"`, `"anchor_old:<sha256-16>"`, ...) specifically so
/// nothing downstream mistakes two differently-labeled hashes for
/// comparable values — see the module doc's body_hash/stamp finding.
#[derive(Debug, Clone, Serialize)]
pub struct PairReceipt {
    pub receipt_oid: Option<String>,
    pub symbol: String,
    pub hashes: Vec<String>,
}

/// One generator's output: an ordered, receipted candidate pair, not yet
/// scored or gated (that is [`super::rank`]'s job).
#[derive(Debug, Clone)]
pub struct PairCandidate {
    pub project: String,
    pub ep_a: String,
    pub ep_b: String,
    pub ts_a: DateTime<Utc>,
    pub ts_b: DateTime<Utc>,
    pub relation: Relation,
    pub generator: Generator,
    /// D7 stable topic key: `"symbol:<name>"` for ledger/relapse
    /// (`(project, symbol)` — `project` is a separate `dream_relations`
    /// column, so the key itself carries only `symbol`), or
    /// `"era:<sha256-hex>"` for era (hash of the sorted shared-anchor-
    /// symbol/rare-token set). The `symbol:`/`era:` prefixes are a judgment
    /// call (undocumented by the design) purely to keep the two generators'
    /// key spaces from ever colliding by coincidence.
    pub topic_key: String,
    pub receipt: PairReceipt,
    /// The verdict's `observed_head_oid` (auxiliary per D6) — `None` for
    /// era pairs, which carry no witness event at all.
    pub aux_oid: Option<String>,
    pub oid_provenance: OidProvenance,
    /// The time of the underlying event this pair is evidence about: the
    /// resolved verdict/receipt time for ledger/relapse, or the later
    /// episode's own `ts` for era (a documented proxy — era pairs have no
    /// discrete "event", so the rank stage's D1 "-0.40 if event < 30d old"
    /// recency penalty needs *some* time to compare against; the later
    /// episode's ts is the closest analog: a very recent topic thread is
    /// not yet "forgotten" either).
    pub event_time: DateTime<Utc>,
}

// ---------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------

/// D7 topic key for ledger/relapse: `(project, symbol)`, `project` carried
/// by `dream_relations`'s own column so the key itself is just `symbol`,
/// namespaced against era's hash-shaped keys.
fn symbol_topic_key(symbol: &str) -> String {
    format!("symbol:{symbol}")
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `git -C <repo_root> <args>` with ambient `GIT_*` env stripped — same
/// rationale and pattern as `storage::dream_backfill::git_at` /
/// `extraction::repo_root::git_toplevel`, duplicated locally per this
/// codebase's convention of keeping each module's small git helpers
/// dependency-free of its siblings.
fn git_at(repo_root: &str) -> Command {
    let mut cmd = Command::new("git");
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&k);
        }
    }
    cmd.arg("-C").arg(repo_root);
    cmd
}

/// D6: committer date (never author date) of `oid`, via
/// `git show -s --format=%ct <oid>`. `None` when git cannot resolve the OID
/// locally (shallow clone, OID from a fork the local repo never fetched,
/// no `git` binary, `repo_root` not actually a repo) — never a guess.
fn git_commit_committer_ts(repo_root: &str, oid: &str) -> Option<i64> {
    let output = git_at(repo_root)
        .arg("show")
        .arg("-s")
        .arg("--format=%ct")
        .arg(oid)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .parse::<i64>()
        .ok()
}

/// The LATEST recorded `witness_verdicts.created_at` for `witness_id` — the
/// same row [`symbol_verdict_state`]'s `representative` field already
/// selected (that field is, by construction, each witness's own latest
/// event; see `storage::witness_verdicts::symbol_verdict_state`'s doc), just
/// re-fetching the one column that struct doesn't carry. A tiny local query
/// rather than widening `witness_verdicts`'s public row type for one caller.
fn latest_verdict_created_at(conn: &Connection, witness_id: i64) -> Result<Option<String>> {
    conn.query_row(
        "SELECT created_at FROM witness_verdicts WHERE witness_id = ?1 ORDER BY id DESC LIMIT 1",
        params![witness_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(Into::into)
}

/// D6: resolve a verdict's event time. Tries git first (committer date of
/// `verdict.receipt_oid`, via the repo the anchor's `file` resolves to);
/// falls back to the verdict's own `created_at` (observation time) when the
/// OID is unresolvable — "ledger-attested, git-unresolvable", never a
/// discard. Returns the resolved time plus which path was taken.
fn resolve_event_time(
    conn: &Connection,
    verdict: &crate::storage::witness_verdicts::WitnessVerdictRow,
    file: &str,
) -> Result<(DateTime<Utc>, OidProvenance)> {
    if let Some(oid) = verdict.receipt_oid.as_deref() {
        if let Some(repo_root) = crate::extraction::repo_root::repo_root_for_file(file) {
            if let Some(unix_ts) = git_commit_committer_ts(&repo_root, oid) {
                if let Some(dt) = DateTime::<Utc>::from_timestamp(unix_ts, 0) {
                    return Ok((dt, OidProvenance::GitDerived));
                }
            }
        }
    }
    let fallback = latest_verdict_created_at(conn, verdict.witness_id)?
        .and_then(|s| parse_timestamp(&s))
        // `created_at` is `NOT NULL DEFAULT (datetime('now'))` at the schema
        // level, so this arm should be unreachable in practice; `Utc::now()`
        // is a maximally conservative last resort (never blocks generation,
        // never back-dates evidence) rather than a `panic!`/`unwrap()`.
        .unwrap_or_else(Utc::now);
    Ok((fallback, OidProvenance::CreatedAtFallback))
}

/// Index episodes by every symbol they anchor: `(file, symbol_name) ->
/// episodes touching it`, oldest-first (input order preserved — callers
/// pass an already ts-sorted slice).
fn index_by_symbol(episodes: &[EpisodeRow]) -> BTreeMap<(String, String), Vec<&EpisodeRow>> {
    let mut by_symbol: BTreeMap<(String, String), Vec<&EpisodeRow>> = BTreeMap::new();
    for ep in episodes {
        for a in &ep.anchors {
            if a.file.is_empty() || a.name.is_empty() {
                continue;
            }
            by_symbol
                .entry((a.file.clone(), a.name.clone()))
                .or_default()
                .push(ep);
        }
    }
    by_symbol
}

fn anchor_body_hash<'a>(ep: &'a EpisodeRow, file: &str, symbol: &str) -> Option<&'a str> {
    ep.anchors
        .iter()
        .find(|a| a.file == file && a.name == symbol)
        .map(|a| a.body_hash.as_str())
}

/// P2: `session_id -> episode row`, built once per generator call so
/// ledger-edge-first endpoint resolution (below) doesn't need a fresh
/// lookup per symbol. `.or_insert` keeps the first episode seen for a given
/// session_id — `hooks::stop::store_episode` guarantees at most one live
/// episode per session anyway, so a collision here would only ever be
/// synthetic test data.
fn session_index(episodes: &[EpisodeRow]) -> HashMap<&str, &EpisodeRow> {
    let mut m = HashMap::new();
    for e in episodes {
        m.entry(e.session_id.as_str()).or_insert(e);
    }
    m
}

/// P2 (F2 fix): resolve a witness's own recorded `source_id` (a
/// conversation/session id) to the episode it names, when non-blank and
/// actually present in this project's episode set. `None` for a blank/absent
/// source_id or one that names no episode this project materialized —
/// callers fall back to geometric reconstruction in either case.
fn episode_for_source<'a>(
    idx: &HashMap<&str, &'a EpisodeRow>,
    source_id: Option<&str>,
) -> Option<&'a EpisodeRow> {
    let s = source_id?.trim();
    if s.is_empty() {
        return None;
    }
    idx.get(s).copied()
}

/// Stamps of every witness whose latest event is negative
/// (`state.negative_witness_ids`), labeled `"witness:<stamp>"`.
fn negative_witness_hashes(conn: &Connection, state: &SymbolVerdictState) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for wid in &state.negative_witness_ids {
        if let Some(w) = witness_by_id(conn, *wid)? {
            out.push(format!("witness:{}", w.stamp));
        }
    }
    Ok(out)
}

fn finalize_hashes(mut hashes: Vec<String>) -> Vec<String> {
    hashes.sort();
    hashes.dedup();
    hashes
}

// ---------------------------------------------------------------------
// G-relapse (D2 — build first, highest priority)
// ---------------------------------------------------------------------

/// Episode E3, dated after a `superseded_by`/`anchor_obsolete` verdict on
/// symbol S, whose anchors re-touch S — an unconscious regression to a
/// pattern already ruled retired. Ordered pair `(E_before, E3)`: `E_before`
/// is the latest episode that touched S strictly before the verdict's
/// resolved event time (the work that S's retirement was actually about);
/// `E3` is the EARLIEST episode touching S strictly after that time (the
/// first relapse, not every later re-touch — the first is the one carrying
/// the "unconscious" claim; later re-touches of the same still-retired S
/// are the *same* relapse story, not new ones, and D7's topic-key dedup
/// keys them identically anyway).
///
/// Relation: the CHECK vocabulary on `dream_relations.relation` has no
/// third value for "relapsed to" (D2 added a GENERATOR, not a new relation)
/// — `Relation::ExtendedBy` is the closer fit of the two (E3's work is a
/// continuation in time of E_before's, not E_before being resolved or
/// replaced by it), a judgment call the design does not resolve explicitly.
///
/// Per D2: "few candidates expected; that is correct" — this generator is
/// deliberately narrow (exact symbol re-touch after an exact verdict), not
/// a broad similarity match.
///
/// The design's "todo variant" (same todo text reappearing partial after a
/// pickup) is NOT implemented here: it needs fuzzy todo-text matching with
/// an undefined similarity threshold, out of this stage's well-specified
/// scope (same class of deferral as Stage 2's un-implemented FTS-todo-term
/// tau-positive signal) — documented as a deferred judgment call, not
/// silently dropped.
pub(super) fn generate_relapse_pairs(
    conn: &Connection,
    family: &Family,
    ledger: &FamilyLedgerIndex,
    episodes: &[EpisodeRow],
) -> Result<Vec<PairCandidate>> {
    let by_symbol = index_by_symbol(episodes);
    let sessions = session_index(episodes);
    let mut out = Vec::new();
    // A-b: family-level bulk-stamp/degenerate-receipt-oid flags, computed
    // once and reused for every symbol below (see `death_time::detect_bulk`).
    let bulk = death_time::detect_bulk(conn, family)?;

    for ((file, symbol), touching) in &by_symbol {
        let Some(fv) = family_symbol_verdict_state(conn, ledger, file, symbol)? else {
            continue;
        };
        let state = fv.state;

        // A-b: event time is now the witness's backfill-robust death time,
        // not a bare receipt_oid/created_at lookup. A CreatedAtFallback
        // death is UNORDERABLE and must never satisfy this generator's
        // before/after gate — see `death_time::DeathTime::orderable`.
        let rep_witness = witness_by_id(conn, state.representative.witness_id)?;
        let death = death_time::resolve_relapse_death_time(
            conn,
            rep_witness.as_ref(),
            &state.representative,
            &fv.variant_file,
            &bulk,
        )?;
        if !death.orderable() {
            continue;
        }
        let event_time = death.time;
        let provenance = death.oid_provenance();

        // P2: prefer the ledger-encoded advocate — the representative
        // witness's own recorded source session — when it actually touches
        // this symbol; fall back to the latest pre-event-time touch
        // geometrically otherwise (same source-preference-then-geometric-
        // fallback policy as `generate_ledger_pairs`'s `e_old`).
        let e_before = rep_witness
            .as_ref()
            .and_then(|w| episode_for_source(&sessions, w.source_id.as_deref()))
            .filter(|e| touching.iter().any(|t| t.episode_id == e.episode_id))
            .filter(|e| e.ts < event_time)
            .or_else(|| {
                let mut befores: Vec<&EpisodeRow> = touching
                    .iter()
                    .copied()
                    .filter(|e| e.ts < event_time)
                    .collect();
                befores.sort_by_key(|e| e.ts);
                befores.last().copied()
            });
        let Some(e_before) = e_before else {
            continue;
        };

        let mut relapses: Vec<&EpisodeRow> = touching
            .iter()
            .copied()
            .filter(|e| e.ts > event_time)
            .collect();
        relapses.sort_by_key(|e| e.ts);
        let Some(e3) = relapses.first().copied() else {
            continue;
        };

        let mut hashes = negative_witness_hashes(conn, &state)?;
        if let Some(h) = anchor_body_hash(e_before, file, symbol) {
            hashes.push(format!("anchor_before:{h}"));
        }
        if let Some(h) = anchor_body_hash(e3, file, symbol) {
            hashes.push(format!("anchor_relapse:{h}"));
        }

        out.push(PairCandidate {
            project: family.name.clone(),
            ep_a: e_before.episode_id.clone(),
            ep_b: e3.episode_id.clone(),
            ts_a: e_before.ts,
            ts_b: e3.ts,
            relation: Relation::ExtendedBy,
            generator: Generator::Relapse,
            topic_key: symbol_topic_key(symbol),
            receipt: PairReceipt {
                receipt_oid: state.representative.receipt_oid.clone(),
                symbol: symbol.clone(),
                hashes: finalize_hashes(hashes),
            },
            aux_oid: Some(state.representative.observed_head_oid.clone()),
            oid_provenance: provenance,
            event_time,
        });
    }

    Ok(out)
}

// ---------------------------------------------------------------------
// G-ledger
// ---------------------------------------------------------------------

/// Episode advocated symbol S; a later verdict marks S `superseded_by` /
/// `anchor_obsolete`; a later episode touches the successor witness (a
/// `superseded_by` verdict's `successor_witness_id`, resolved to its own
/// `(file, symbol)`) or the same file after that verdict's resolved event
/// time. Ordered pair `(E_old, E_new)`: `E_old` is the latest episode
/// touching S strictly before the event time; `E_new` is the EARLIEST
/// qualifying successor episode strictly after it (closest, strongest
/// causal link).
pub(super) fn generate_ledger_pairs(
    conn: &Connection,
    family: &Family,
    ledger: &FamilyLedgerIndex,
    episodes: &[EpisodeRow],
) -> Result<Vec<PairCandidate>> {
    let by_symbol = index_by_symbol(episodes);
    let sessions = session_index(episodes);
    let mut out = Vec::new();

    for ((file, symbol), touching) in &by_symbol {
        let Some(fv) = family_symbol_verdict_state(conn, ledger, file, symbol)? else {
            continue;
        };
        let state = fv.state;
        let (event_time, provenance) =
            resolve_event_time(conn, &state.representative, &fv.variant_file)?;

        // P2 (F2 fix): prefer the episode the representative witness's own
        // recorded source session names, when it actually touches this
        // symbol, over reconstructing the endpoint from temporal geometry
        // alone. Geometric reconstruction breaks for the common case of a
        // same-session supersession: "latest touch before the verdict
        // timestamp" resolves to the SUPERSEDING episode itself (it touched
        // the symbol and then, moments later in the same session, recorded
        // the verdict against it) — the ledger's own `source_id` names the
        // true advocate session directly and needs no timestamp guessing.
        let rep_witness = witness_by_id(conn, state.representative.witness_id)?;
        let e_old = rep_witness
            .as_ref()
            .and_then(|w| episode_for_source(&sessions, w.source_id.as_deref()))
            .filter(|e| touching.iter().any(|t| t.episode_id == e.episode_id))
            .filter(|e| e.ts < event_time)
            .or_else(|| {
                let mut olds: Vec<&EpisodeRow> = touching
                    .iter()
                    .copied()
                    .filter(|e| e.ts < event_time)
                    .collect();
                olds.sort_by_key(|e| e.ts);
                olds.last().copied()
            });
        let Some(e_old) = e_old else {
            continue;
        };

        let successor: Option<WitnessLedgerRow> =
            if state.representative.verdict == VerdictKind::SupersededBy {
                match state.representative.successor_witness_id {
                    Some(wid) => witness_by_id(conn, wid)?,
                    None => None,
                }
            } else {
                None
            };

        // Same source-preference for the successor side: the successor
        // witness's own recorded source session, when it postdates `e_old`;
        // fall back to the earliest later episode touching the successor
        // file/symbol (or the same file after the event) otherwise.
        let e_new = successor
            .as_ref()
            .and_then(|s| episode_for_source(&sessions, s.source_id.as_deref()))
            .filter(|e| e.ts > e_old.ts)
            .or_else(|| {
                let mut news: Vec<&EpisodeRow> = episodes
                    .iter()
                    .filter(|e| e.ts > event_time)
                    .filter(|e| {
                        // Episode anchors are already canonical
                        // (`load_family_episodes`); the successor witness's
                        // file is a raw ledger path — canonicalize before
                        // comparing so a worktree-stamped successor still
                        // matches.
                        e.anchors.iter().any(|a| a.file == *file)
                            || successor.as_ref().is_some_and(|s| {
                                let s_canon = canon_file(&s.file);
                                e.anchors.iter().any(|a| {
                                    a.file == s_canon
                                        && Some(a.name.as_str()) == s.symbol.as_deref()
                                })
                            })
                    })
                    .collect();
                news.sort_by_key(|e| e.ts);
                news.first().copied()
            });
        let Some(e_new) = e_new else {
            continue;
        };

        let mut hashes = negative_witness_hashes(conn, &state)?;
        if let Some(h) = anchor_body_hash(e_old, file, symbol) {
            hashes.push(format!("anchor_old:{h}"));
        }
        if let Some(s) = &successor {
            hashes.push(format!("successor_witness:{}", s.stamp));
        }

        out.push(PairCandidate {
            project: family.name.clone(),
            ep_a: e_old.episode_id.clone(),
            ep_b: e_new.episode_id.clone(),
            ts_a: e_old.ts,
            ts_b: e_new.ts,
            relation: Relation::ReplacedBy,
            generator: Generator::Ledger,
            topic_key: symbol_topic_key(symbol),
            receipt: PairReceipt {
                receipt_oid: state.representative.receipt_oid.clone(),
                symbol: symbol.clone(),
                hashes: finalize_hashes(hashes),
            },
            aux_oid: Some(state.representative.observed_head_oid.clone()),
            oid_provenance: provenance,
            event_time,
        });
    }

    Ok(out)
}

// ---------------------------------------------------------------------
// G-era (D8: theta fit against prev-chain co-clustering; G-todo cut)
// ---------------------------------------------------------------------

const ERA_MIN_SPAN_DAYS: i64 = 14;
const ERA_MAX_PAIRS_PER_CLUSTER: usize = 3;
/// Design's own starting value (§3 Stage 2), kept as the fallback for when
/// there is no prev-chain data to fit against at all.
const DEFAULT_ERA_THETA: f64 = 0.65;
/// Descending grid searched for the LARGEST (tightest, most conservative)
/// theta at which every prev-chain pair with resolvable vectors still
/// co-clusters (D8). A grid rather than a continuous search: clustering
/// output only changes at finitely many breakpoints anyway, and a fixed
/// grid keeps the fit trivially deterministic and cheap to test.
const ERA_THETA_GRID: &[f64] = &[
    0.95, 0.90, 0.85, 0.80, 0.75, 0.70, 0.65, 0.60, 0.55, 0.50, 0.45, 0.40,
];
/// A token's project-corpus document frequency must be at or below this
/// fraction of the project's episode count (floored at 2 — two episodes
/// sharing a token is the minimum for "shared" to mean anything at all) to
/// count as "rare" for the IDF gate. Undocumented by the design; a judgment
/// call in the same spirit as Stage 2's `QUEUE_U_TODO_CAP`.
const RARE_TOKEN_MAX_DF_FRACTION: f64 = 0.10;
/// With no shared anchor symbol, this many shared rare tokens are required
/// before a cluster's endpoints count as topically tied (see the gate's
/// comment in [`generate_era_pairs`]).
const ERA_MIN_TOKENS_WITHOUT_SYMBOL: usize = 2;

fn episode_text(e: &EpisodeRow) -> String {
    format!(
        "{} {} {}",
        e.request,
        e.completed,
        e.next_steps.as_deref().unwrap_or("")
    )
}

/// English function words that must never count as "rare" topical evidence.
/// The original design leaned on the IDF gate alone ("kills everything
/// mentions cargo test"), but the first live dry-run falsified that: in a
/// terse-episode corpus (one-line requests, short completed summaries) words
/// like "and"/"from"/"your" genuinely clear the df<=10% bar and surfaced as
/// the sole "shared rare tokens" behind the top mush pairs. df measures
/// corpus rarity, not topical content — function words carry none no matter
/// how rare they are, so they are excluded categorically. Content words
/// stay IDF-governed.
fn is_function_word(t: &str) -> bool {
    matches!(
        t,
        "and"
            | "the"
            | "for"
            | "from"
            | "your"
            | "our"
            | "their"
            | "his"
            | "her"
            | "its"
            | "with"
            | "that"
            | "this"
            | "these"
            | "those"
            | "then"
            | "than"
            | "when"
            | "what"
            | "where"
            | "which"
            | "while"
            | "was"
            | "were"
            | "are"
            | "been"
            | "being"
            | "will"
            | "would"
            | "should"
            | "could"
            | "into"
            | "over"
            | "under"
            | "onto"
            | "out"
            | "not"
            | "but"
            | "all"
            | "any"
            | "some"
            | "has"
            | "had"
            | "have"
            | "does"
            | "did"
            | "done"
            | "you"
            | "they"
            | "them"
            | "there"
            | "here"
            | "also"
            | "just"
            | "only"
            | "very"
            | "still"
            | "now"
            | "new"
            | "old"
            | "via"
            | "per"
            | "full"
    )
}

/// Lowercase, alphanumeric(+underscore)-run tokens of length >= 3, minus
/// function words (see [`is_function_word`] for why the IDF gate alone was
/// not enough).
fn tokenize(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.len() >= 3 && !is_function_word(t))
        .map(|t| t.to_string())
        .collect()
}

fn document_frequency(episodes: &[EpisodeRow]) -> HashMap<String, usize> {
    let mut df = HashMap::new();
    for e in episodes {
        for tok in tokenize(&episode_text(e)) {
            *df.entry(tok).or_insert(0) += 1;
        }
    }
    df
}

/// A token must appear in at most this many episodes to count as "rare".
/// Capped strictly below `n_episodes` (never `n_episodes` itself, however
/// small the corpus): a token every episode contains is universal
/// boilerplate, not rare, no matter how few episodes exist to share it —
/// without this cap, a 2-episode project would count boilerplate text
/// shared by both of its only two episodes as "rare" (df=2 <= a naively
/// computed threshold of 2), defeating the entire gate.
fn rare_threshold(n_episodes: usize) -> usize {
    if n_episodes < 2 {
        return 0;
    }
    let computed = ((n_episodes as f64 * RARE_TOKEN_MAX_DF_FRACTION).round() as usize).max(2);
    computed.min(n_episodes - 1)
}

/// Anchor names shorter than this are closure/loop variables (`r`, `s`,
/// `i`) the AST extractor sometimes records, not topical symbols — the
/// first live dry-run surfaced `shared_symbol:r` as a top pair's entire
/// evidence. Same empirical correction as [`is_function_word`].
const ERA_MIN_SYMBOL_LEN: usize = 3;

/// Per-episode document frequency of anchor SYMBOL names — the symbol leg's
/// mirror of [`document_frequency`], so ubiquitous method names (`new`,
/// `fmt`, `run`, `default`) anchored by most episodes in a project can
/// never serve as a cluster's sole topical evidence any more than a
/// high-df prose token can (round-4 review: the length bar alone left the
/// symbol leg without the rarity discipline the token leg already had).
fn anchor_symbol_frequency(episodes: &[EpisodeRow]) -> HashMap<String, usize> {
    let mut df = HashMap::new();
    for e in episodes {
        let names: BTreeSet<&str> = e.anchors.iter().map(|x| x.name.as_str()).collect();
        for n in names {
            *df.entry(n.to_string()).or_insert(0) += 1;
        }
    }
    df
}

fn shared_anchor_symbols(
    a: &EpisodeRow,
    b: &EpisodeRow,
    symbol_df: &HashMap<String, usize>,
    threshold: usize,
) -> BTreeSet<String> {
    let names_a: BTreeSet<&str> = a.anchors.iter().map(|x| x.name.as_str()).collect();
    b.anchors
        .iter()
        .map(|x| x.name.as_str())
        .filter(|n| {
            names_a.contains(n)
                && n.len() >= ERA_MIN_SYMBOL_LEN
                && symbol_df.get(*n).copied().unwrap_or(0) <= threshold
        })
        .map(|n| n.to_string())
        .collect()
}

fn shared_rare_tokens(
    a: &EpisodeRow,
    b: &EpisodeRow,
    df: &HashMap<String, usize>,
    threshold: usize,
) -> BTreeSet<String> {
    let ta = tokenize(&episode_text(a));
    let tb = tokenize(&episode_text(b));
    ta.intersection(&tb)
        .filter(|t| df.get(*t).copied().unwrap_or(0) <= threshold)
        .cloned()
        .collect()
}

/// D7 era topic key: a stable hash of the sorted union of shared anchor
/// symbols and shared rare tokens — computed from the CLUSTER's two
/// temporal endpoints (earliest/latest), not from whichever specific pair
/// of episodes a caller is currently emitting. This is what makes the key
/// stable across a recluster that picks a different "best middle" episode:
/// the endpoints (and therefore the key) do not move just because the
/// middle does — exactly the property the task's own "same story different
/// middle episode => same key" test requires.
///
/// **Documented deviation (conformance review, LOW):** D7's literal text
/// names only "hash of the sorted shared-anchor-symbol set" — it does not
/// mention rare tokens. This function unions symbols AND tokens on purpose:
/// [`generate_era_pairs`]'s own emission gate already requires
/// `!(shared_symbols.is_empty() && shared_tokens.is_empty())`, so a
/// token-only cluster (no shared anchor symbol at all, just shared rare
/// vocabulary) is a real, emittable case — hashing symbols alone would
/// collide every token-only era pair onto the SAME key (the hash of two
/// empty sets), defeating D7's own "diversity" purpose for exactly the
/// pairs it needs it most.
fn era_topic_key(shared_symbols: &BTreeSet<String>, shared_tokens: &BTreeSet<String>) -> String {
    let mut material: Vec<&str> = shared_symbols
        .iter()
        .map(|s| s.as_str())
        .chain(shared_tokens.iter().map(|s| s.as_str()))
        .collect();
    material.sort_unstable();
    material.dedup();
    let joined = material.join("\u{1}");
    let digest = Sha256::digest(joined.as_bytes());
    format!("era:{}", hex_encode(&digest))
}

fn average_linkage_similarity(
    a: &[String],
    b: &[String],
    vectors: &HashMap<String, Vec<f32>>,
) -> f64 {
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for x in a {
        for y in b {
            if let (Some(vx), Some(vy)) = (vectors.get(x), vectors.get(y)) {
                sum += cosine_sim(vx, vy) as f64;
                count += 1;
            }
        }
    }
    if count == 0 {
        0.0
    } else {
        sum / count as f64
    }
}

/// Single-pass average-linkage agglomerative clustering: repeatedly merge
/// the pair of clusters with the highest average pairwise cosine similarity
/// while that similarity is `>= theta`, stopping when no remaining pair
/// clears the threshold. Deterministic: episode ids are sorted before
/// clustering starts, so tie-breaking in the "which pair merges first"
/// search never depends on `HashMap` iteration order.
fn agglomerative_cluster(vectors: &HashMap<String, Vec<f32>>, theta: f64) -> Vec<Vec<String>> {
    let mut ids: Vec<String> = vectors.keys().cloned().collect();
    ids.sort();
    let mut clusters: Vec<Vec<String>> = ids.into_iter().map(|id| vec![id]).collect();

    loop {
        if clusters.len() < 2 {
            break;
        }
        let mut best: Option<(usize, usize, f64)> = None;
        for i in 0..clusters.len() {
            for j in (i + 1)..clusters.len() {
                let sim = average_linkage_similarity(&clusters[i], &clusters[j], vectors);
                if best.is_none_or(|(_, _, b)| sim > b) {
                    best = Some((i, j, sim));
                }
            }
        }
        match best {
            Some((i, j, sim)) if sim >= theta => {
                let merged_from_j = clusters[j].clone();
                clusters[i].extend(merged_from_j);
                clusters.remove(j);
            }
            _ => break,
        }
    }
    clusters
}

fn cluster_membership(clusters: &[Vec<String>]) -> HashMap<&str, usize> {
    let mut membership = HashMap::new();
    for (idx, cluster) in clusters.iter().enumerate() {
        for id in cluster {
            membership.insert(id.as_str(), idx);
        }
    }
    membership
}

/// D8: fit theta to the largest grid value at which every prev-chain pair
/// with resolvable vectors on both sides co-clusters. Falls back to
/// [`DEFAULT_ERA_THETA`] when there is no such pair to fit against (a
/// brand-new project, or one with no session resumes at all — nothing to
/// constrain the fit, so the design's own un-amended starting value stands)
/// or when no grid value satisfies the constraint (genuinely dissimilar
/// resumed sessions — clustering cannot be forced against real dissimilarity,
/// so the sane default is the only option).
pub(super) fn fit_era_theta(
    vectors: &HashMap<String, Vec<f32>>,
    chain_pairs: &[(String, String)],
) -> f64 {
    let relevant: Vec<(&str, &str)> = chain_pairs
        .iter()
        .filter(|(r, d)| vectors.contains_key(r) && vectors.contains_key(d))
        .map(|(r, d)| (r.as_str(), d.as_str()))
        .collect();
    if relevant.is_empty() {
        return DEFAULT_ERA_THETA;
    }
    for &theta in ERA_THETA_GRID {
        let clusters = agglomerative_cluster(vectors, theta);
        let membership = cluster_membership(&clusters);
        let all_co_cluster = relevant
            .iter()
            .all(|(r, d)| membership.contains_key(r) && membership.get(r) == membership.get(d));
        if all_co_cluster {
            return theta;
        }
    }
    DEFAULT_ERA_THETA
}

fn best_middle<'a>(members: &[&'a EpisodeRow]) -> Option<&'a EpisodeRow> {
    if members.len() < 3 {
        return None;
    }
    let first = members[0].ts;
    let last = members[members.len() - 1].ts;
    let mid = first + (last - first) / 2;
    members[1..members.len() - 1]
        .iter()
        .min_by_key(|e| (e.ts - mid).num_seconds().abs())
        .copied()
}

/// Per-family topical-continuity clustering (formerly per-project; the
/// 2026-08-26 design ruling widened the corpus). See the module doc for
/// the overall generator list and D8's theta-fit override. Prev-chain
/// pairs are gathered per member key — `prev_episode_id` chains are
/// recorded under the raw key each session carried — and concatenated for
/// one family-wide theta fit.
pub(super) fn generate_era_pairs(
    conn: &Connection,
    family: &Family,
    episodes: &[EpisodeRow],
    vectors: &HashMap<String, Vec<f32>>,
) -> Result<Vec<PairCandidate>> {
    if episodes.len() < 2 {
        return Ok(Vec::new());
    }
    let mut chain_pairs = Vec::new();
    for member in &family.members {
        chain_pairs.extend(prev_chain_pairs(conn, member)?);
    }
    let theta = fit_era_theta(vectors, &chain_pairs);
    let clusters = agglomerative_cluster(vectors, theta);

    let by_id: HashMap<&str, &EpisodeRow> = episodes
        .iter()
        .map(|e| (e.episode_id.as_str(), e))
        .collect();
    let df = document_frequency(episodes);
    let symbol_df = anchor_symbol_frequency(episodes);
    let threshold = rare_threshold(episodes.len());

    let mut out = Vec::new();
    for cluster in &clusters {
        let mut members: Vec<&EpisodeRow> = cluster
            .iter()
            .filter_map(|id| by_id.get(id.as_str()).copied())
            .collect();
        if members.len() < 2 {
            continue;
        }
        members.sort_by_key(|e| e.ts);
        let earliest = members[0];
        let latest = members[members.len() - 1];
        if (latest.ts - earliest.ts).num_days() < ERA_MIN_SPAN_DAYS {
            continue;
        }

        let shared_symbols = shared_anchor_symbols(earliest, latest, &symbol_df, threshold);
        let shared_tokens = shared_rare_tokens(earliest, latest, &df, threshold);
        if shared_symbols.is_empty() && shared_tokens.len() < ERA_MIN_TOKENS_WITHOUT_SYMBOL {
            // Beyond-file-path-overlap gate: no genuine topical evidence
            // ties the endpoints together, only (at most) coincidental
            // vector similarity — never emitted. A single shared rare token
            // with no shared anchor symbol is one coincidence away from
            // mush (live dry-run evidence), so the token-only path demands
            // corroboration; one real shared SYMBOL remains sufficient.
            continue;
        }
        let topic_key = era_topic_key(&shared_symbols, &shared_tokens);
        let symbol_label = shared_symbols
            .iter()
            .next()
            .cloned()
            .unwrap_or_else(|| shared_tokens.iter().next().cloned().unwrap_or_default());
        let hashes = finalize_hashes(
            shared_symbols
                .iter()
                .map(|s| format!("shared_symbol:{s}"))
                .chain(shared_tokens.iter().map(|t| format!("shared_token:{t}")))
                .collect(),
        );

        let mut pairs: Vec<(&EpisodeRow, &EpisodeRow)> = vec![(earliest, latest)];
        if let Some(middle) = best_middle(&members) {
            pairs.push((earliest, middle));
            pairs.push((middle, latest));
        }
        pairs.truncate(ERA_MAX_PAIRS_PER_CLUSTER);

        for (a, b) in pairs {
            out.push(PairCandidate {
                project: family.name.clone(),
                ep_a: a.episode_id.clone(),
                ep_b: b.episode_id.clone(),
                ts_a: a.ts,
                ts_b: b.ts,
                relation: Relation::ExtendedBy,
                generator: Generator::Era,
                topic_key: topic_key.clone(),
                receipt: PairReceipt {
                    receipt_oid: None,
                    symbol: symbol_label.clone(),
                    hashes: hashes.clone(),
                },
                // Era pairs carry no witness event at all — there is no
                // OID to be "auxiliary" to.
                aux_oid: None,
                // No OID was ever attempted (there is nothing to resolve);
                // `CreatedAtFallback` is the closest of the two CHECK
                // values and directionally correct for scoring (see
                // `rank::gate_score`'s era-gap discount): topical-
                // continuity evidence is inherently softer than a
                // git-verified supersession.
                oid_provenance: OidProvenance::CreatedAtFallback,
                event_time: b.ts,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::witness_ledger::{insert_witness, WitnessLedgerRow as LedgerRow};
    use crate::storage::witness_verdicts::{insert_verdict_if_changed, WitnessVerdictRow};

    fn open() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::storage::migrations::run(&conn).unwrap();
        conn
    }

    // Single-key family fixtures — every pre-family test ran against the
    // one project key "proj"; these keep those tests byte-identical in
    // intent while exercising the family-shaped signatures.
    fn relapse_pairs_for_proj(conn: &Connection, episodes: &[EpisodeRow]) -> Vec<PairCandidate> {
        let family = Family::single("proj");
        let ledger = FamilyLedgerIndex::load(conn, &family).unwrap();
        generate_relapse_pairs(conn, &family, &ledger, episodes).unwrap()
    }

    fn ledger_pairs_for_proj(conn: &Connection, episodes: &[EpisodeRow]) -> Vec<PairCandidate> {
        let family = Family::single("proj");
        let ledger = FamilyLedgerIndex::load(conn, &family).unwrap();
        generate_ledger_pairs(conn, &family, &ledger, episodes).unwrap()
    }

    fn v2_episode(
        session_id: &str,
        project: &str,
        ts: &str,
        outcome: &str,
        anchors_json: &str,
    ) -> String {
        format!(
            r#"{{
                "schema": "v2",
                "session_id": "{session_id}",
                "project": "{project}",
                "timestamp": "{ts}",
                "request": "work on thing",
                "investigated": [],
                "completed": "did some of it",
                "next_steps": null,
                "blockers": null,
                "outcome": "{outcome}",
                "error_signatures": [],
                "tools_used": [],
                "files_modified": [],
                "message_count": 1,
                "duration_minutes": 1,
                "todos": [],
                "approved_plan": null,
                "prev_episode_id": null,
                "anchors": {anchors_json}
            }}"#
        )
    }

    fn insert_episode(
        conn: &Connection,
        id: &str,
        session_id: &str,
        project: &str,
        ts: &str,
        outcome: &str,
        anchors_json: &str,
    ) {
        conn.execute(
            "INSERT INTO reflections (id, content, tags, timestamp) VALUES (?1, ?2, '[]', ?3)",
            params![
                id,
                v2_episode(session_id, project, ts, outcome, anchors_json),
                ts
            ],
        )
        .unwrap();
        // Tests read `episode_index`, not `reflections`, directly (matching
        // production, where the daemon runs Stage 0 before any later
        // stage) — materialize immediately so each insert is visible to
        // `load_project_episodes` right away.
        crate::storage::dream_backfill::materialize_episode_index(conn).unwrap();
    }

    fn anchor_json(file: &str, name: &str, body_hash: &str) -> String {
        format!(
            r#"[{{"file":"{file}","node_kind":"function","name":"{name}","body_hash":"{body_hash}"}}]"#
        )
    }

    fn seed_ledger_witness_full(
        conn: &Connection,
        project: &str,
        file: &str,
        symbol: &str,
        stamp: &str,
        at_oid: &str,
        source_id: Option<&str>,
    ) -> i64 {
        insert_witness(
            conn,
            &LedgerRow {
                id: 0,
                project: project.to_string(),
                file: file.to_string(),
                symbol: Some(symbol.to_string()),
                span_start: None,
                span_end: None,
                stamp: stamp.to_string(),
                tier: "committed".to_string(),
                at_oid: Some(at_oid.to_string()),
                source_kind: if source_id.is_some() {
                    "conversation".to_string()
                } else {
                    "backfill".to_string()
                },
                source_id: source_id.map(str::to_string),
            },
        )
        .unwrap();
        conn.query_row(
            "SELECT id FROM witness_ledger WHERE project = ?1 AND file = ?2 AND symbol = ?3 AND stamp = ?4",
            params![project, file, symbol, stamp],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn seed_ledger_witness(
        conn: &Connection,
        project: &str,
        file: &str,
        symbol: &str,
        stamp: &str,
    ) -> i64 {
        seed_ledger_witness_full(conn, project, file, symbol, stamp, "deadbeef", None)
    }

    // -----------------------------------------------------------------
    // A-b: minimal real-git-repo scaffolding, used by the two relapse
    // tests below whose fixtures need a git-verifiable death (see
    // `death_time::resolve_relapse_death_time` — a `CreatedAtFallback`
    // death is unorderable and can no longer satisfy this generator's
    // gate, so a fixture with no real repo behind it now correctly yields
    // zero pairs instead of one).
    // -----------------------------------------------------------------

    fn strip_git_env(cmd: &mut Command) {
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(&k);
            }
        }
    }

    fn init_repo(dir: &std::path::Path) -> bool {
        std::fs::create_dir_all(dir).unwrap();
        let mut cmd = Command::new("git");
        strip_git_env(&mut cmd);
        cmd.arg("init").arg("-q").arg(dir);
        cmd.status().map(|s| s.success()).unwrap_or(false)
    }

    fn commit_all(dir: &std::path::Path) -> Option<String> {
        let run = |args: &[&str]| -> bool {
            let mut cmd = Command::new("git");
            strip_git_env(&mut cmd);
            cmd.arg("-C").arg(dir).args(args);
            cmd.status().map(|s| s.success()).unwrap_or(false)
        };
        if !run(&["add", "-A"])
            || !run(&[
                "-c",
                "user.email=t@example.com",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "seed",
            ])
        {
            return None;
        }
        let mut cmd = Command::new("git");
        strip_git_env(&mut cmd);
        cmd.arg("-C").arg(dir).arg("rev-parse").arg("HEAD");
        let out = cmd.output().ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
    }

    fn mark_obsolete(conn: &Connection, witness_id: i64, observed_head: &str) {
        insert_verdict_if_changed(
            conn,
            &WitnessVerdictRow {
                witness_id,
                verdict: VerdictKind::AnchorObsolete,
                successor_witness_id: None,
                receipt_oid: None,
                observed_head_oid: observed_head.to_string(),
            },
        )
        .unwrap();
    }

    // -----------------------------------------------------------------
    // G-relapse: join correctness (incl. ts ordering)
    // -----------------------------------------------------------------

    #[test]
    fn relapse_pairs_a_pre_verdict_episode_with_the_earliest_post_verdict_re_touch() {
        // A-b: this generator's event time now comes from a git-verified
        // death (`death_time::resolve_relapse_death_time`), so the fixture
        // needs a real repo behind the witness -- a `CreatedAtFallback`
        // death is unorderable and would otherwise yield zero pairs.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return; // git unavailable in this environment -- fail-soft skip
        }
        let file = repo.join("a.rs");
        std::fs::write(&file, "fn foo() {\n    1\n}\n").unwrap();
        let Some(c0) = commit_all(&repo) else {
            return;
        };
        let stamp = codewitness::StampKind::Raw
            .compute(&std::fs::read(&file).unwrap())
            .as_str()
            .to_string();
        // The death commit: foo's body actually changes here.
        std::fs::write(&file, "fn foo() {\n    2\n}\n").unwrap();
        let Some(c1) = commit_all(&repo) else {
            return;
        };
        let file_str = file.to_string_lossy().to_string();

        let conn = open();
        let wid = seed_ledger_witness_full(&conn, "proj", &file_str, "foo", &stamp, &c0, None);
        mark_obsolete(&conn, wid, &c1);

        // Before the verdict: touches foo.
        insert_episode(
            &conn,
            "ep-before",
            "sess-before",
            "proj",
            "2020-01-01T00:00:00Z",
            "completed",
            &anchor_json(&file_str, "foo", "hash1"),
        );
        // After the verdict: an unrelated episode that does NOT touch foo —
        // must never be picked as the relapse.
        insert_episode(
            &conn,
            "ep-unrelated",
            "sess-unrelated",
            "proj",
            "2099-01-01T00:00:00Z",
            "completed",
            &anchor_json(&file_str, "bar", "hashX"),
        );
        // After the verdict: the actual relapse, later still.
        insert_episode(
            &conn,
            "ep-relapse-late",
            "sess-relapse-late",
            "proj",
            "2099-02-01T00:00:00Z",
            "partial",
            &anchor_json(&file_str, "foo", "hash2"),
        );
        // After the verdict, EARLIER than ep-relapse-late — this is the
        // correct relapse pick (earliest post-verdict re-touch).
        insert_episode(
            &conn,
            "ep-relapse-early",
            "sess-relapse-early",
            "proj",
            "2099-01-15T00:00:00Z",
            "partial",
            &anchor_json(&file_str, "foo", "hash3"),
        );

        let episodes = load_project_episodes(&conn, "proj").unwrap();
        let pairs = relapse_pairs_for_proj(&conn, &episodes);
        assert_eq!(pairs.len(), 1, "exactly one symbol has a negative verdict");
        let p = &pairs[0];
        assert_eq!(p.ep_a, "ep-before");
        assert_eq!(p.ep_b, "ep-relapse-early");
        assert!(p.ts_a < p.ts_b, "ts ordering must hold");
        assert_eq!(p.generator, Generator::Relapse);
        assert_eq!(p.topic_key, "symbol:foo");
        assert_eq!(
            p.oid_provenance,
            OidProvenance::GitDerived,
            "a span-bisected death must surface as git-derived"
        );
    }

    #[test]
    fn relapse_pairs_with_no_resolvable_repo_yield_nothing_not_a_backfill_run_time_guess() {
        // A-b's core regression test: the OLD basis (receipt_oid / verdict
        // created_at) would happily have manufactured a relapse pair here
        // ordered by backfill-run wall-clock time. With no real repo behind
        // the witness, death time can only resolve to `CreatedAtFallback`,
        // which is unorderable -- zero pairs, not a guess.
        let conn = open();
        let wid = seed_ledger_witness(&conn, "proj", "a.rs", "foo", "b3:aaa");
        mark_obsolete(&conn, wid, "headoid");

        insert_episode(
            &conn,
            "ep-before",
            "sess-before",
            "proj",
            "2020-01-01T00:00:00Z",
            "completed",
            &anchor_json("a.rs", "foo", "hash1"),
        );
        insert_episode(
            &conn,
            "ep-relapse",
            "sess-relapse",
            "proj",
            "2099-01-01T00:00:00Z",
            "partial",
            &anchor_json("a.rs", "foo", "hash2"),
        );

        let episodes = load_project_episodes(&conn, "proj").unwrap();
        let pairs = relapse_pairs_for_proj(&conn, &episodes);
        assert!(
            pairs.is_empty(),
            "an unorderable death must never satisfy the relapse gate"
        );
    }

    #[test]
    fn relapse_requires_a_pre_verdict_touching_episode_to_form_a_pair() {
        let conn = open();
        let wid = seed_ledger_witness(&conn, "proj", "a.rs", "foo", "b3:aaa");
        mark_obsolete(&conn, wid, "headoid");
        // Only a post-verdict episode exists — no `E_before` to pair with.
        insert_episode(
            &conn,
            "ep-relapse",
            "sess",
            "proj",
            "2099-01-01T00:00:00Z",
            "partial",
            &anchor_json("a.rs", "foo", "hash"),
        );
        let episodes = load_project_episodes(&conn, "proj").unwrap();
        let pairs = relapse_pairs_for_proj(&conn, &episodes);
        assert!(pairs.is_empty());
    }

    #[test]
    fn relapse_ignores_symbols_with_no_negative_verdict() {
        let conn = open();
        // Witness exists but no verdict was ever recorded for it.
        seed_ledger_witness(&conn, "proj", "a.rs", "foo", "b3:aaa");
        insert_episode(
            &conn,
            "ep-1",
            "sess-1",
            "proj",
            "2020-01-01T00:00:00Z",
            "completed",
            &anchor_json("a.rs", "foo", "hash"),
        );
        insert_episode(
            &conn,
            "ep-2",
            "sess-2",
            "proj",
            "2020-02-01T00:00:00Z",
            "completed",
            &anchor_json("a.rs", "foo", "hash2"),
        );
        let episodes = load_project_episodes(&conn, "proj").unwrap();
        let pairs = relapse_pairs_for_proj(&conn, &episodes);
        assert!(pairs.is_empty());
    }

    // -----------------------------------------------------------------
    // G-ledger
    // -----------------------------------------------------------------

    #[test]
    fn ledger_pairs_old_advocate_with_earliest_successor_touch() {
        let conn = open();
        let old_wid = seed_ledger_witness(&conn, "proj", "a.rs", "foo", "b3:old");
        let new_wid = seed_ledger_witness(&conn, "proj", "b.rs", "bar", "b3:new");
        // Give the successor a real record so `witness_by_id` resolves it,
        // and wire successor_witness_id directly in the one verdict event.
        insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: old_wid,
                verdict: VerdictKind::SupersededBy,
                successor_witness_id: Some(new_wid),
                receipt_oid: Some("receiptoid".to_string()),
                observed_head_oid: "headoid".to_string(),
            },
        )
        .unwrap();

        insert_episode(
            &conn,
            "ep-old",
            "sess-old",
            "proj",
            "2020-01-01T00:00:00Z",
            "completed",
            &anchor_json("a.rs", "foo", "hashold"),
        );
        // Touches the successor's (file, symbol) after the verdict.
        insert_episode(
            &conn,
            "ep-new-early",
            "sess-new-early",
            "proj",
            "2099-01-10T00:00:00Z",
            "completed",
            &anchor_json("b.rs", "bar", "hashnew1"),
        );
        insert_episode(
            &conn,
            "ep-new-late",
            "sess-new-late",
            "proj",
            "2099-02-10T00:00:00Z",
            "completed",
            &anchor_json("b.rs", "bar", "hashnew2"),
        );

        let episodes = load_project_episodes(&conn, "proj").unwrap();
        let pairs = ledger_pairs_for_proj(&conn, &episodes);
        assert_eq!(pairs.len(), 1);
        let p = &pairs[0];
        assert_eq!(p.ep_a, "ep-old");
        assert_eq!(p.ep_b, "ep-new-early", "earliest successor touch wins");
        assert_eq!(p.relation, Relation::ReplacedBy);
        assert_eq!(p.oid_provenance, OidProvenance::CreatedAtFallback);
    }

    #[test]
    fn ledger_falls_back_to_same_file_after_receipt_when_no_successor_touch_exists() {
        let conn = open();
        let wid = seed_ledger_witness(&conn, "proj", "a.rs", "foo", "b3:old");
        mark_obsolete(&conn, wid, "headoid");

        insert_episode(
            &conn,
            "ep-old",
            "sess-old",
            "proj",
            "2020-01-01T00:00:00Z",
            "completed",
            &anchor_json("a.rs", "foo", "hashold"),
        );
        // No episode touches a successor (there is none — AnchorObsolete),
        // but a later episode DOES touch the same file again.
        insert_episode(
            &conn,
            "ep-samefile",
            "sess-samefile",
            "proj",
            "2099-01-01T00:00:00Z",
            "completed",
            &anchor_json("a.rs", "somethingelse", "hashy"),
        );

        let episodes = load_project_episodes(&conn, "proj").unwrap();
        let pairs = ledger_pairs_for_proj(&conn, &episodes);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].ep_b, "ep-samefile");
    }

    // -----------------------------------------------------------------
    // P2 (F2): ledger-edge-first endpoint resolution beats geometric
    // reconstruction for a same-session supersession.
    // -----------------------------------------------------------------

    fn seed_ledger_witness_with_source(
        conn: &Connection,
        project: &str,
        file: &str,
        symbol: &str,
        stamp: &str,
        source_id: &str,
    ) -> i64 {
        seed_ledger_witness_full(
            conn,
            project,
            file,
            symbol,
            stamp,
            "deadbeef",
            Some(source_id),
        )
    }

    #[test]
    fn ledger_pairs_prefers_the_witness_source_session_over_the_latest_geometric_touch() {
        let conn = open();
        // The witness's OWN recorded source is sess-old (ep-old's session).
        let old_wid =
            seed_ledger_witness_with_source(&conn, "proj", "a.rs", "foo", "b3:old", "sess-old");
        mark_obsolete(&conn, old_wid, "headoid");

        // ep-old: the TRUE advocate, touches foo, session sess-old.
        insert_episode(
            &conn,
            "ep-old",
            "sess-old",
            "proj",
            "2020-01-01T00:00:00Z",
            "completed",
            &anchor_json("a.rs", "foo", "hashold"),
        );
        // ep-same: ALSO touches foo, LATER than ep-old but still before the
        // verdict's (fallback) event time — under pure geometric
        // reconstruction ("latest pre-event-time touch"), this episode
        // would be wrongly picked as e_old instead of the true advocate.
        insert_episode(
            &conn,
            "ep-same",
            "sess-same",
            "proj",
            "2020-01-02T00:00:00Z",
            "completed",
            &anchor_json("a.rs", "foo", "hashsame"),
        );
        // A genuinely later episode (past the AnchorObsolete verdict's
        // real-wall-clock fallback event time, since no receipt_oid was
        // given) so e_new's own geometric fallback has something to find —
        // this test is only exercising e_old's source-preference, not e_new.
        insert_episode(
            &conn,
            "ep-new",
            "sess-new",
            "proj",
            "2099-01-01T00:00:00Z",
            "completed",
            &anchor_json("a.rs", "somethingelse", "hashnew"),
        );

        let episodes = load_project_episodes(&conn, "proj").unwrap();
        let pairs = ledger_pairs_for_proj(&conn, &episodes);
        assert_eq!(pairs.len(), 1);
        assert_eq!(
            pairs[0].ep_a, "ep-old",
            "the witness's own source session must win over the later geometric touch"
        );
    }

    #[test]
    fn relapse_pairs_prefers_the_witness_source_session_for_e_before() {
        // A-b: needs a real, git-verifiable death (see the comment on
        // `relapse_pairs_a_pre_verdict_episode_with_the_earliest_post_verdict_re_touch`).
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return;
        }
        let file = repo.join("a.rs");
        std::fs::write(&file, "fn foo() {\n    1\n}\n").unwrap();
        let Some(c0) = commit_all(&repo) else {
            return;
        };
        let stamp = codewitness::StampKind::Raw
            .compute(&std::fs::read(&file).unwrap())
            .as_str()
            .to_string();
        std::fs::write(&file, "fn foo() {\n    2\n}\n").unwrap();
        let Some(c1) = commit_all(&repo) else {
            return;
        };
        let file_str = file.to_string_lossy().to_string();

        let conn = open();
        let old_wid = seed_ledger_witness_full(
            &conn,
            "proj",
            &file_str,
            "foo",
            &stamp,
            &c0,
            Some("sess-old"),
        );
        mark_obsolete(&conn, old_wid, &c1);

        insert_episode(
            &conn,
            "ep-old",
            "sess-old",
            "proj",
            "2020-01-01T00:00:00Z",
            "completed",
            &anchor_json(&file_str, "foo", "hashold"),
        );
        insert_episode(
            &conn,
            "ep-same",
            "sess-same",
            "proj",
            "2020-01-02T00:00:00Z",
            "completed",
            &anchor_json(&file_str, "foo", "hashsame"),
        );
        // The actual relapse, after the verdict.
        insert_episode(
            &conn,
            "ep-relapse",
            "sess-relapse",
            "proj",
            "2099-01-01T00:00:00Z",
            "partial",
            &anchor_json(&file_str, "foo", "hashrelapse"),
        );

        let episodes = load_project_episodes(&conn, "proj").unwrap();
        let pairs = relapse_pairs_for_proj(&conn, &episodes);
        assert_eq!(pairs.len(), 1);
        assert_eq!(
            pairs[0].ep_a, "ep-old",
            "the witness's own source session must win e_before over the later geometric touch"
        );
        assert_eq!(pairs[0].ep_b, "ep-relapse");
    }

    #[test]
    fn ledger_pairs_falls_back_to_geometric_when_source_id_is_blank_or_unresolvable() {
        let conn = open();
        // Blank source_id (empty string, not NULL) must fall back exactly
        // like `None` does.
        let old_wid = seed_ledger_witness_with_source(&conn, "proj", "a.rs", "foo", "b3:old", "");
        mark_obsolete(&conn, old_wid, "headoid");

        insert_episode(
            &conn,
            "ep-old",
            "sess-old",
            "proj",
            "2020-01-01T00:00:00Z",
            "completed",
            &anchor_json("a.rs", "foo", "hashold"),
        );
        insert_episode(
            &conn,
            "ep-samefile",
            "sess-samefile",
            "proj",
            "2099-01-01T00:00:00Z",
            "completed",
            &anchor_json("a.rs", "somethingelse", "hashy"),
        );

        let episodes = load_project_episodes(&conn, "proj").unwrap();
        let pairs = ledger_pairs_for_proj(&conn, &episodes);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].ep_a, "ep-old");
        assert_eq!(pairs[0].ep_b, "ep-samefile");
    }

    // -----------------------------------------------------------------
    // Era topic-key stability across recluster
    // -----------------------------------------------------------------

    #[test]
    fn era_topic_key_is_stable_across_different_shared_symbol_or_token_orderings() {
        let mut symbols_a: BTreeSet<String> = BTreeSet::new();
        symbols_a.insert("Foo::bar".to_string());
        symbols_a.insert("Baz::qux".to_string());
        let mut tokens_a: BTreeSet<String> = BTreeSet::new();
        tokens_a.insert("frobnicate".to_string());

        // Same sets, different insertion order — BTreeSet already normalizes
        // this, but the point under test is the *function's* stability, not
        // BTreeSet's.
        let mut symbols_b: BTreeSet<String> = BTreeSet::new();
        symbols_b.insert("Baz::qux".to_string());
        symbols_b.insert("Foo::bar".to_string());
        let mut tokens_b: BTreeSet<String> = BTreeSet::new();
        tokens_b.insert("frobnicate".to_string());

        assert_eq!(
            era_topic_key(&symbols_a, &tokens_a),
            era_topic_key(&symbols_b, &tokens_b)
        );
    }

    #[test]
    fn era_topic_key_is_identical_for_all_pairs_from_the_same_cluster_regardless_of_middle() {
        // Simulates the task's literal scenario: two runs of the SAME
        // three-episode story, but a different middle episode is chosen
        // (e.g. because clustering merged in a fourth near-duplicate that
        // changed which one is closest to the temporal midpoint). The
        // endpoints (earliest/latest) are unchanged between runs, so every
        // pair emitted from the cluster — earliest-latest, earliest-middle,
        // middle-latest — must carry the SAME topic_key in both runs.
        let earliest = mk_episode(
            "e1",
            "2020-01-01T00:00:00Z",
            &["Shared::sym"],
            "shared request text",
        );
        let latest = mk_episode(
            "e3",
            "2020-02-01T00:00:00Z",
            &["Shared::sym"],
            "shared request text",
        );
        let middle_run1 = mk_episode("e2a", "2020-01-10T00:00:00Z", &[], "unrelated");
        let middle_run2 = mk_episode(
            "e2b",
            "2020-01-20T00:00:00Z",
            &[],
            "different unrelated text",
        );

        let shared_symbols = shared_anchor_symbols(
            &earliest,
            &latest,
            &anchor_symbol_frequency(&[earliest.clone(), latest.clone()]),
            10,
        );
        let shared_tokens = shared_rare_tokens(
            &earliest,
            &latest,
            &document_frequency(&[earliest.clone(), latest.clone(), middle_run1.clone()]),
            10,
        );
        let key_run1 = era_topic_key(&shared_symbols, &shared_tokens);

        let shared_tokens_run2 = shared_rare_tokens(
            &earliest,
            &latest,
            &document_frequency(&[earliest.clone(), latest.clone(), middle_run2.clone()]),
            10,
        );
        let key_run2 = era_topic_key(&shared_symbols, &shared_tokens_run2);

        assert_eq!(
            key_run1, key_run2,
            "topic_key must not depend on which episode was picked as the middle"
        );
    }

    fn mk_episode(id: &str, ts: &str, symbols: &[&str], text: &str) -> EpisodeRow {
        EpisodeRow {
            episode_id: id.to_string(),
            session_id: format!("sess-{id}"),
            ts: parse_timestamp(ts).unwrap(),
            outcome: "completed".to_string(),
            todo_count: 0,
            blockers: None,
            request: text.to_string(),
            completed: String::new(),
            next_steps: None,
            anchors: symbols
                .iter()
                .map(|s| AnchorRow {
                    file: "a.rs".to_string(),
                    node_kind: "function".to_string(),
                    name: s.to_string(),
                    body_hash: "h".to_string(),
                })
                .collect(),
            present_at_head: None,
        }
    }

    // -----------------------------------------------------------------
    // Era clustering / theta fit
    // -----------------------------------------------------------------

    #[test]
    fn fit_era_theta_falls_back_to_default_with_no_prev_chain_data() {
        let vectors: HashMap<String, Vec<f32>> = HashMap::new();
        assert_eq!(fit_era_theta(&vectors, &[]), DEFAULT_ERA_THETA);
    }

    #[test]
    fn fit_era_theta_picks_a_grid_value_that_keeps_a_chain_pair_together() {
        let mut vectors = HashMap::new();
        vectors.insert("root".to_string(), vec![1.0, 0.0, 0.0]);
        // Near-identical to root — should co-cluster even at a fairly
        // strict theta.
        vectors.insert("desc".to_string(), vec![0.99, 0.01, 0.0]);
        // A totally unrelated episode in the same project — must not force
        // theta down just because it exists.
        vectors.insert("other".to_string(), vec![0.0, 0.0, 1.0]);

        let chain_pairs = vec![("root".to_string(), "desc".to_string())];
        let theta = fit_era_theta(&vectors, &chain_pairs);

        let clusters = agglomerative_cluster(&vectors, theta);
        let membership = cluster_membership(&clusters);
        assert_eq!(membership.get("root"), membership.get("desc"));
        assert!(
            theta >= 0.90,
            "near-identical chain vectors should fit a high theta, got {theta}"
        );
    }

    #[test]
    fn era_gate_kills_file_path_only_cohesion() {
        let conn = open();
        // Two episodes touching the SAME file but with no shared anchor
        // symbol and no shared rare token in their text — must not emit.
        insert_episode(
            &conn,
            "ep-1",
            "sess-1",
            "proj",
            "2020-01-01T00:00:00Z",
            "completed",
            &anchor_json("shared_file.rs", "alpha", "h1"),
        );
        insert_episode(
            &conn,
            "ep-2",
            "sess-2",
            "proj",
            "2020-02-01T00:00:00Z",
            "completed",
            &anchor_json("shared_file.rs", "beta", "h2"),
        );
        let episodes = load_project_episodes(&conn, "proj").unwrap();
        let mut vectors = HashMap::new();
        vectors.insert("ep-1".to_string(), vec![1.0, 0.0]);
        vectors.insert("ep-2".to_string(), vec![0.99, 0.01]);

        let pairs =
            generate_era_pairs(&conn, &Family::single("proj"), &episodes, &vectors).unwrap();
        assert!(
            pairs.is_empty(),
            "no shared symbol or rare token beyond the shared file path"
        );
    }
}
