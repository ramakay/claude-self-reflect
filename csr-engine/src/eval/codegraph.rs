//! Code-graph release gate (`csr-engine eval --codegraph`).
//!
//! The default gate is deterministic and CI-safe: it builds a small graph in a
//! migrated in-memory SQLite database, then exercises the production extractor,
//! resolver, degree ranker, graph-slice producer, and storage queries. The live
//! variant measures the same thresholds against the real graph without writing
//! to it. Gates that require writes (rank recomputation and edit round-trip) run
//! against an in-memory shadow of the live nodes and edges.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use ast_grep_language::SupportLang;

use super::{EvalReport, EvalResult};
use crate::extraction::ast_analysis::lang_from_path_str;
use crate::extraction::codegraph::{extract_graph_fragment, extract_graph_fragment_for_file};
use crate::extraction::repo_path::canonical_repo_path;
use crate::extraction::repo_scan;
use crate::extraction::resolver::ResolveStats;
use crate::hooks::prompt_submit::build_graph_slices;
use crate::injection::formatter::estimate_tokens;
use crate::storage::codegraph::{EdgeRow, NodeRow};
use crate::storage::Storage;

const CATEGORY: &str = "codegraph";
const PROJECT: &str = "codegraph-eval";
const REPO: &str = "fixture-repo";
const RESOLUTION_RATE_MIN: f64 = 0.70;
/// Witness-closure gate threshold (WCR Phase 4a). Pre-registered 2026-07-30,
/// BEFORE the first live measurement: `bound + external + method` (every
/// evidenced outcome — bound, or explicitly classified as crossing a real
/// boundary) must together cover >= 90% of placeholder `calls`/`imports`
/// edges. See `witness_closure_gate` and `resolver::ResolveStats::closure_rate`.
const WITNESS_CLOSURE_MIN: f64 = 0.90;
/// Internal-binding gate threshold (WCR Phase 4a). Pre-registered 2026-07-30,
/// BEFORE the first live measurement: of the edges NOT already explained away
/// as `external`/`method` boundary crossings, >= 70% must actually bind to a
/// real definition — not merely be explained. See `internal_binding_gate` and
/// `resolver::ResolveStats::internal_binding_rate`.
const INTERNAL_BINDING_MIN: f64 = 0.70;
/// Cap on `code_evolution` rows copied into a WCR shadow (`shadow_for_wcr`).
/// Rows are ordered by `timestamp DESC, id DESC` (the `id` tiebreaker keeps
/// the cutoff deterministic across rebuilds — Finding 3, WCR truth pass)
/// before the cap applies, so the most recent — most behaviorally relevant
/// — co-edit signal survives on large corpora.
const CODE_EVOLUTION_SHADOW_CAP: usize = 50_000;
/// Cap on the number of projects `repo_scan::scan_all` is run against per WCR
/// shadow build. Repo scanning reads the real filesystem per project; on a
/// corpus spanning many projects this bounds gate wall-time. Ranked by edge
/// count (see `scan_repo_defs`) so the projects that matter most to closure
/// get scanned first; any remaining projects are noted, not silently dropped.
const WCR_SCAN_PROJECT_CAP: usize = 12;
const INJECTION_TOKEN_MAX: usize = 500;
const EXTRACTION_LATENCY_MAX_MS: f64 = 50.0;
const QUERY_P95_MAX_MS: f64 = 5.0;
const QUERY_SAMPLES_PER_MODE: usize = 20;
const QUERY_TRIALS: usize = 3;
/// Max plausible length for a single imported identifier. Measured against the
/// live corpus: the longest real identifier actually stored in `code_nodes` is
/// 64 chars (`task_dir_all_deleted_is_authoritative_empty_and_failures_counted`),
/// and legitimate SCREAMING_CASE constants reach 43
/// (`HOME_ONBOARDING_SPOTLIGHT_FALLBACK_DELAY_MS`). 80 clears both with margin.
///
/// This is only a backstop for keyword-free concatenations; the primary
/// detector is the multi-keyword rule in `import_key_sanity_gate`. An earlier
/// value of 40 was set from a grep of `pub fn` signatures alone and produced
/// false positives on real constants.
const IMPORT_KEY_MAX_LEN: usize = 80;

const API_FILE: &str = "src/api.rs";
const SERVICE_FILE: &str = "src/service.rs";
const WORKER_FILE: &str = "src/worker.rs";
const API_SOURCE: &str = r#"
pub fn beta_fn() -> usize { 2 }
pub fn gamma_fn() -> usize { 3 }
"#;
const SERVICE_SOURCE: &str = r#"
use crate::api::beta_fn;
pub fn alpha_fn() -> usize { beta_fn() }
"#;
// `use std;` and the `omega_undefined_method` receiver call are deliberate:
// both are unbindable by any bind tier (no `code_nodes`/`repo_defs` def, no
// repo scan in the fixture path) but ARE evidence-classifiable — `std`
// matches a Rust builtin namespace (X1 `external`), and the receiver call
// has `callee_kind = "method"` with no def anywhere (X2 `method`). Without
// these the fixture graph fully binds via B0/B1/B2 alone, leaving zero
// pending edges for the WCR gates (Phase 4a) to measure — a vacuous 0/0
// pass. These two edges give `witness_closure_gate`/`internal_binding_gate`
// a non-trivial (but still fully-explained, >=90%/>=70%) fixture case.
const WORKER_SOURCE: &str = r#"
use crate::api::gamma_fn;
use std;
pub fn delta_fn() -> usize { gamma_fn() + beta_fn() }
pub fn omega_fn() -> usize {
    let receiver = Receiver;
    receiver.omega_undefined_method()
}
"#;

// TypeScript and Python fixture files. Both import a symbol already defined
// in API_SOURCE (`beta_fn` / `gamma_fn`) so the resolver's project-unique
// rule resolves them deterministically without dragging down the
// calls+imports resolution rate (see RESOLUTION_RATE_MIN).
const TS_FILE: &str = "src/util.ts";
const PY_FILE: &str = "src/consumer.py";
const TS_SOURCE: &str = r#"
import { beta_fn } from './api';

export function epsilon_fn(): number {
    return 5;
}
"#;
const PY_SOURCE: &str = r#"
from api import gamma_fn


def zeta_fn():
    return 7
"#;

#[derive(Debug, Clone, Default)]
struct GraphSnapshot {
    nodes: Vec<NodeRow>,
    edges: Vec<EdgeRow>,
    file_states: Vec<(String, String, bool)>,
    evolution_projects: BTreeSet<String>,
}

fn judge(name: &str, started: Instant, passed: bool, detail: String) -> EvalResult {
    let duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    if passed {
        EvalResult::pass(name, CATEGORY, duration_ms, detail)
    } else {
        EvalResult::fail(name, CATEGORY, duration_ms, detail)
    }
}

/// Count only resolution-eligible edge kinds. `defines` edges are resolved by
/// construction and including them would inflate the graph's resolution rate.
fn eligible_resolution_counts<I, S>(edges: I) -> (usize, usize, f64)
where
    I: IntoIterator<Item = (S, bool)>,
    S: AsRef<str>,
{
    let mut resolved = 0usize;
    let mut total = 0usize;
    for (kind, is_resolved) in edges {
        if matches!(kind.as_ref(), "calls" | "imports") {
            total += 1;
            resolved += usize::from(is_resolved);
        }
    }
    let rate = if total == 0 {
        0.0
    } else {
        resolved as f64 / total as f64
    };
    (resolved, total, rate)
}

fn resolution_gate(snapshot: &GraphSnapshot) -> EvalResult {
    let started = Instant::now();
    let (resolved, total, rate) = eligible_resolution_counts(
        snapshot
            .edges
            .iter()
            .map(|edge| (edge.kind.as_str(), edge.resolved != 0)),
    );
    judge(
        "Resolution rate",
        started,
        rate >= RESOLUTION_RATE_MIN,
        format!(
            "{resolved}/{total} calls+imports resolved ({:.1}%, threshold >= {:.0}%); defines excluded",
            rate * 100.0,
            RESOLUTION_RATE_MIN * 100.0
        ),
    )
}

/// Witness-closure gate (WCR Phase 4a). Pre-registered 2026-07-30, BEFORE the
/// first live measurement. PASS iff `closure_rate >= WITNESS_CLOSURE_MIN`:
/// bound edges plus evidenced-boundary (`external`/`method`) classifications
/// must together account for >= 90% of all placeholder `calls`/`imports`
/// edges. Complements `resolution_gate` (which only counts bind-tier
/// `resolved`) by also crediting X1/X2 classification as a legitimate,
/// evidenced outcome rather than silence. Takes `&ResolveStats` directly so
/// the pass/fail boundary is unit-testable without a database.
///
/// `local=` (WCR truth pass, TASK 2) added to the detail string alongside
/// the pre-registered `bound`/`external`/`method`/`stale`/`internal_module`/
/// `drifted` fields — reporting only, the SAME `>= 90%` threshold and the
/// SAME `stats.closure_rate` field drive pass/fail exactly as before; the X4
/// class is already folded into `closure_rate` by `resolve_edges` itself
/// (see `ResolveStats::local`'s doc comment), never re-derived here.
fn witness_closure_gate(stats: &ResolveStats) -> EvalResult {
    let started = Instant::now();
    judge(
        "Witness closure",
        started,
        stats.closure_rate >= WITNESS_CLOSURE_MIN,
        format!(
            "bound={} external={} method={} stale={} internal_module={} drifted={} local={} unexplained={} ambiguous={} closure={:.1}% (threshold >= 90%)",
            stats.bound,
            stats.external,
            stats.method,
            stats.stale,
            stats.internal_module,
            stats.drifted,
            stats.local,
            stats.unexplained,
            stats.ambiguous_remaining,
            stats.closure_rate * 100.0
        ),
    )
}

/// Internal-binding gate (WCR Phase 4a). Pre-registered 2026-07-30, BEFORE
/// the first live measurement. PASS iff `internal_binding_rate >=
/// INTERNAL_BINDING_MIN`: of the edges NOT already explained away as
/// `external`/`method` boundary crossings, >= 70% must actually bind to a
/// real definition rather than merely being explained. Takes `&ResolveStats`
/// directly so the pass/fail boundary is unit-testable without a database.
///
/// `eligible`'s formula (WCR truth pass, TASK 2) now also subtracts
/// `stats.local` — reporting only, matching `resolve_edges`'s own
/// `internal_denominator` formula exactly (`ResolveStats::internal_binding_rate`
/// already excludes `local` from its denominator; this local `eligible`
/// re-derivation exists only to print the same number in the detail string,
/// so it must track that formula 1:1, same as it already did for
/// `internal_module`/`drifted` when THOSE classes were added). The `>= 70%`
/// threshold and `stats.internal_binding_rate` field driving pass/fail are
/// unchanged.
fn internal_binding_gate(stats: &ResolveStats) -> EvalResult {
    let started = Instant::now();
    let eligible = stats.total.saturating_sub(
        stats.external
            + stats.method
            + stats.stale
            + stats.internal_module
            + stats.drifted
            + stats.local,
    );
    judge(
        "Internal binding",
        started,
        stats.internal_binding_rate >= INTERNAL_BINDING_MIN,
        format!(
            "bound={} / eligible={} = {:.1}% (threshold >= 70%); denominator excludes evidence-classified external+method+stale+internal_module+drifted+local",
            stats.bound,
            eligible,
            stats.internal_binding_rate * 100.0
        ),
    )
}

/// Zero `imports` edges whose placeholder key (dst_id with the `name:` prefix
/// stripped) is not a plausible single identifier. Only unresolved
/// (`name:`-prefixed) import edges are in scope — a resolved dst_id is a real
/// `code_nodes.id` (a sha256 hash), not a name blob, and is not a placeholder
/// key at all. Catches the text-mangling bug that ran a helper over the whole
/// import statement instead of per-symbol AST fields (verified live examples:
/// `name:ArcOnce`, `name:BTreeMapBTreeSet`, `name:importfileURLToPathfromurl`,
/// `name:AtomicBoolOrdering`).
fn import_key_sanity_gate(snapshot: &GraphSnapshot) -> EvalResult {
    let started = Instant::now();
    let mut total = 0usize;
    let mut offenders: Vec<&str> = Vec::new();
    for edge in &snapshot.edges {
        if edge.kind != "imports" || !edge.dst_id.starts_with("name:") {
            continue;
        }
        total += 1;
        let key = edge.dst_id.strip_prefix("name:").unwrap_or(&edge.dst_id);
        // A single keyword substring is NOT evidence of the bug: real
        // identifiers legitimately contain them (`requireApiAuth`,
        // `extract_name_from_def`, `resolve_project_from_cwd`, `SeekFrom`).
        // The blob signature is statement text collapsed into one token, which
        // necessarily carries TWO or more syntax keywords
        // (`importReactfromreact`, `importfileURLToPathfromurl`). A key that is
        // exactly a bare keyword is also malformed.
        // NB: a key equal to a bare keyword is NOT an offender either. This
        // crate has a module literally named `import`, so `use crate::import;`
        // correctly yields `name:import`.
        let keyword_hits = ["import", "from", "require"]
            .iter()
            .filter(|needle| key.contains(*needle))
            .count();
        if keyword_hits >= 2 || key.len() > IMPORT_KEY_MAX_LEN {
            offenders.push(key);
        }
    }
    let bad = offenders.len();
    let examples: Vec<&str> = offenders.iter().take(3).copied().collect();
    judge(
        "Import key sanity",
        started,
        bad == 0,
        format!(
            "{bad}/{total} unresolved import edge keys are implausible (>=2 of import/from/require, or over {IMPORT_KEY_MAX_LEN} chars); examples={examples:?}"
        ),
    )
}

/// Zero `code_nodes` rows with `project == ""`. The hook path used to resolve
/// the project from an env var that is never set in hook processes, leaving
/// nodes unscoped and invisible to project-filtered queries.
fn project_attribution_gate(snapshot: &GraphSnapshot) -> EvalResult {
    let started = Instant::now();
    let total = snapshot.nodes.len();
    let unscoped = snapshot
        .nodes
        .iter()
        .filter(|node| node.project.is_empty())
        .count();
    judge(
        "Project attribution",
        started,
        unscoped == 0,
        format!("{unscoped}/{total} code_nodes rows have project == \"\" (unscoped)"),
    )
}

const LEAK_PROJECT: &str = "codegraph-eval-leak";
const LEAK_REPO: &str = "fixture-leak-repo";
const LEAK_CALLER_FILE: &str = "src/leak_caller.rs";
const LEAK_TARGET_FILE: &str = "src/leak_target.rs";
const LEAK_CALLER_SOURCE: &str = r#"
pub fn leak_caller_fn() -> usize { real_callee_fn() + unresolvable_callee_fn() }
"#;
const LEAK_TARGET_SOURCE: &str = r#"
pub fn real_callee_fn() -> usize { 1 }
"#;

/// `build_graph_slices`' symbol-anchored slice must never render an
/// unresolved (placeholder) callee name as an undecorated call target
/// indistinguishable from a verified one.
///
/// Property asserted: build an isolated fixture where one outbound `calls`
/// edge resolves (`real_callee_fn`) and one deliberately cannot
/// (`unresolvable_callee_fn` is never defined anywhere). Use
/// `code_query_callees` directly to get the ground-truth set of unresolved
/// callee names (surfaced with `kind == "unresolved"`), then render the same
/// prompt through `build_graph_slices` and check that none of those names
/// appear as an exact, undecorated token in the comma-separated call-target
/// list of the rendered "<name> calls -> <targets>" line. A name may be
/// absent from that list, or present but visibly annotated as unverified —
/// either satisfies the property. It may not appear bare.
fn placeholder_leak_gate() -> EvalResult {
    let started = Instant::now();
    let measured = (|| -> Result<(bool, String)> {
        let storage = Arc::new(Storage::open_memory()?);
        extract_and_store(
            &storage,
            LEAK_TARGET_SOURCE,
            LEAK_TARGET_FILE,
            LEAK_REPO,
            LEAK_PROJECT,
            "conv-leak-target",
        )?;
        extract_and_store(
            &storage,
            LEAK_CALLER_SOURCE,
            LEAK_CALLER_FILE,
            LEAK_REPO,
            LEAK_PROJECT,
            "conv-leak-caller",
        )?;
        storage.resolve_code_edges(LEAK_PROJECT)?;

        let node = storage
            .code_nodes_by_name("leak_caller_fn", LEAK_PROJECT, 1)?
            .into_iter()
            .next()
            .context("placeholder-leak fixture: leak_caller_fn node missing")?;
        let callees = storage.code_query_callees(&node.id, 6)?;
        let unresolved_names: Vec<String> = callees
            .iter()
            .filter(|callee| callee.kind == "unresolved")
            .map(|callee| callee.name.clone())
            .collect();
        if unresolved_names.is_empty() {
            anyhow::bail!(
                "placeholder-leak fixture setup error: expected >=1 unresolved callee, got {:?}",
                callees
                    .iter()
                    .map(|callee| (&callee.name, &callee.kind))
                    .collect::<Vec<_>>()
            );
        }

        let slices = build_graph_slices(
            &storage,
            "Review leak_caller_fn in src/leak_caller.rs",
            &[LEAK_CALLER_FILE.to_string()],
            LEAK_PROJECT,
        );
        let rendered = slices.join("\n");

        let mut leaked: Vec<String> = Vec::new();
        let mut parsed_call_lines = 0usize;
        for line in rendered.lines() {
            let parts: Vec<&str> = line.splitn(2, "calls → ").collect();
            if parts.len() != 2 {
                continue;
            }
            parsed_call_lines += 1;
            let after_arrow = parts[1];
            let token_list = after_arrow
                .split(" (last changed")
                .next()
                .unwrap_or(after_arrow);
            let tokens: Vec<&str> = token_list.split(", ").map(str::trim).collect();
            for name in &unresolved_names {
                if tokens.contains(&name.as_str()) {
                    leaked.push(name.clone());
                }
            }
        }

        // Vacuous-pass guard (CodeRabbit PR #279, same shape the fixture
        // comment above protects the WCR gates from): if the renderer's
        // "calls → " separator changes or the outbound-calls line vanishes,
        // zero lines parse and `leaked` stays empty while asserting nothing.
        // Require at least one parsed call-target line for a PASS.
        if parsed_call_lines == 0 {
            let detail = format!(
                "vacuous: no 'calls → ' line parsed from injection render — separator changed or \
                 outbound-calls line missing; rendered={rendered:?}"
            );
            return Ok((false, detail));
        }
        let detail = format!(
            "parsed_call_lines={parsed_call_lines}; unresolved callees={unresolved_names:?}; rendered={rendered:?}; leaked as bare undecorated tokens={leaked:?}"
        );
        Ok((leaked.is_empty(), detail))
    })();

    match measured {
        Ok((passed, detail)) => judge("No placeholder leak in injection", started, passed, detail),
        Err(error) => judge(
            "No placeholder leak in injection",
            started,
            false,
            format!("placeholder-leak gate error: {error:#}"),
        ),
    }
}

fn extract_and_store(
    storage: &Arc<Storage>,
    source: &str,
    file: &str,
    repo: &str,
    project: &str,
    conv_id: &str,
) -> Result<()> {
    let lang = lang_from_path_str(file).context("fixture file must have a supported language")?;
    let fragment = extract_graph_fragment(
        source,
        lang,
        file,
        repo,
        project,
        conv_id,
        "codegraph-eval-session",
    );
    // NOTE: deliberately does NOT populate `NodeRow::repo_root` here. This
    // helper seeds synthetic in-memory eval fixtures using bare relative
    // paths like "src/api.rs" — several of which happen to alias real
    // subdirectories of the developer's own checkout (`csr-engine/src`).
    // Running `extraction::repo_root::repo_root_for_file` against them would
    // spawn `git -C src rev-parse --show-toplevel` and leak the REAL repo's
    // absolute path into a supposedly hermetic fixture, making the eval gate
    // depend on where it happens to be checked out. Fixture nodes stay
    // `repo_root: None` (honest: these paths are not really inside a repo);
    // real production writers (`hooks::post_tool_use`, `import::backfill`,
    // `import::coedit_backfill`) populate it from the actual edited file.
    for node in &fragment.nodes {
        storage.upsert_code_node(node)?;
    }
    storage.replace_code_file_edges(project, file, &fragment.edges)?;
    storage.upsert_code_file_state(project, file, "", false)?;
    Ok(())
}

fn seed_fixture() -> Result<Arc<Storage>> {
    // Storage::open_memory applies the production migrations to a fresh SQLite
    // connection, keeping the release gate independent of the user's database.
    let storage = Arc::new(Storage::open_memory()?);
    for (source, file, conv_id) in [
        (API_SOURCE, API_FILE, "conv-api"),
        (SERVICE_SOURCE, SERVICE_FILE, "conv-service"),
        (WORKER_SOURCE, WORKER_FILE, "conv-worker"),
        (TS_SOURCE, TS_FILE, "conv-util"),
        (PY_SOURCE, PY_FILE, "conv-consumer"),
    ] {
        extract_and_store(&storage, source, file, REPO, PROJECT, conv_id)?;
    }
    storage.resolve_code_edges(PROJECT)?;
    Ok(storage)
}

fn snapshot(storage: &Arc<Storage>) -> Result<GraphSnapshot> {
    storage.with_connection(|conn| {
        let nodes = {
            let mut stmt = conn.prepare(
                "SELECT id, repo, project, file, lang, kind, name, fqname, body_hash,
                        span_start, span_end, first_conv_id, last_conv_id, last_session_id
                 FROM code_nodes ORDER BY id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(NodeRow {
                    id: row.get(0)?,
                    repo: row.get(1)?,
                    project: row.get(2)?,
                    file: row.get(3)?,
                    lang: row.get(4)?,
                    kind: row.get(5)?,
                    name: row.get(6)?,
                    fqname: row.get(7)?,
                    body_hash: row.get(8)?,
                    span_start: row.get(9)?,
                    span_end: row.get(10)?,
                    first_conv_id: row.get(11)?,
                    last_conv_id: row.get(12)?,
                    last_session_id: row.get(13)?,
                    // Not selected above — this snapshot compares graph
                    // structure (nodes/edges), never repo identity.
                    repo_root: None,
                    // Snapshot row read straight from code_nodes: a stored
                    // definition, never a name-only query match.
                    name_only: false,
                    // Not selected above — this snapshot compares graph
                    // structure, never attribution.
                    attribution: String::new(),
                })
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };

        let edges = {
            let mut stmt = conn.prepare(
                "SELECT src_id, dst_id, kind, src_file, resolved, weight, conv_id, session_id,
                        callee_kind, src_content_hash
                 FROM code_edges ORDER BY src_id, dst_id, kind",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(EdgeRow {
                    src_id: row.get(0)?,
                    dst_id: row.get(1)?,
                    kind: row.get(2)?,
                    src_file: row.get(3)?,
                    resolved: row.get(4)?,
                    weight: row.get(5)?,
                    conv_id: row.get(6)?,
                    session_id: row.get(7)?,
                    // `callee_kind` is captured (unlike Phase 1) because the
                    // Phase 4a WCR gates' X2 (method-call) classify tier reads
                    // it back out of a shadow's `code_edges` table once this
                    // row round-trips through `shadow_from_snapshot`.
                    // `boundary`/`evidence` remain out of scope: the resolver
                    // recomputes both from scratch for every still-pending edge
                    // it reprocesses, so a stale snapshot value would only ever
                    // be immediately overwritten.
                    callee_kind: row.get(8)?,
                    // `src_content_hash` (Codex round 7 adversarial review):
                    // UNLIKE `boundary`/`evidence`, this is NEVER recomputed by
                    // the resolver — it is write-time provenance that must
                    // survive the snapshot -> shadow round-trip unchanged, or
                    // `historical_src_content_unchanged`'s re-point gate would
                    // see every live-DB edge as freshly stamp-less inside the
                    // shadow regardless of what the real `code_edges` row
                    // carries, silently reintroducing a "drops the sweep"
                    // instance of the exact bug class this fix closes.
                    src_content_hash: row.get(9)?,
                    ..EdgeRow::default()
                })
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };

        let file_states = {
            let mut stmt = conn.prepare(
                "SELECT project, file, dirty FROM code_graph_file_state
                 ORDER BY project, file",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? != 0,
                ))
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };

        let evolution_projects = {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT project_name FROM code_evolution
                 WHERE project_name != '' ORDER BY project_name",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<std::result::Result<BTreeSet<_>, _>>()?
        };

        Ok(GraphSnapshot {
            nodes,
            edges,
            file_states,
            evolution_projects,
        })
    })
}

fn shadow_from_snapshot(snapshot: &GraphSnapshot) -> Result<Arc<Storage>> {
    let shadow = Arc::new(Storage::open_memory()?);
    for node in &snapshot.nodes {
        shadow.upsert_code_node(node)?;
    }
    let mut edges_by_file: BTreeMap<&str, Vec<EdgeRow>> = BTreeMap::new();
    for edge in &snapshot.edges {
        edges_by_file
            .entry(edge.src_file.as_str())
            .or_default()
            .push(edge.clone());
    }
    for (file, edges) in edges_by_file {
        shadow.replace_code_file_edges("", file, &edges)?;
    }
    Ok(shadow)
}

/// All `code_evolution` columns, in a stable order shared by the WCR
/// shadow's read (from the live DB) and write (into the shadow).
const EVOLUTION_COLS: &str = "id, session_id, project_name, file_path, language, timestamp, \
     tool_name, functions_added, functions_removed, types_added, types_removed, \
     imports_added, imports_removed";
const EVOLUTION_COL_COUNT: usize = 13;

/// Extend the standard rank/round-trip shadow (`shadow_from_snapshot`) with
/// the extra ground truth witness-closure resolution needs: `code_evolution`
/// (co-edit signal for the B3 bind tier) and `repo_defs` (whole-repo scan
/// backing B1/B2 disambiguation and X1 external classification). Builds a
/// shadow independent of any shadow already built from `snapshot` for other
/// gates, so running the resolver here can never perturb rank-determinism or
/// round-trip results computed elsewhere. Never touches the live DB —
/// `code_evolution` rows are read-only copies, and `repo_scan::scan_all` only
/// reads the real filesystem.
///
/// Returns the shadow plus a detail-string suffix noting any projects the
/// repo-scan cap skipped (empty when nothing was skipped).
fn shadow_for_wcr(live: &Arc<Storage>, snapshot: &GraphSnapshot) -> Result<(Arc<Storage>, String)> {
    let shadow = shadow_from_snapshot(snapshot)?;

    let projects: BTreeSet<&str> = snapshot
        .nodes
        .iter()
        .map(|node| node.project.as_str())
        .filter(|project| !project.is_empty())
        .collect();
    if projects.is_empty() {
        return Ok((shadow, String::new()));
    }

    copy_code_evolution(live, &shadow, &projects)?;
    let skip_note = scan_repo_defs(&shadow, snapshot, &projects)?;
    Ok((shadow, skip_note))
}

/// Copy `code_evolution` rows for `projects` from `live` into `shadow`,
/// newest first, capped at `CODE_EVOLUTION_SHADOW_CAP`. Read-only on `live`.
fn copy_code_evolution(
    live: &Arc<Storage>,
    shadow: &Arc<Storage>,
    projects: &BTreeSet<&str>,
) -> Result<usize> {
    copy_code_evolution_capped(live, shadow, projects, CODE_EVOLUTION_SHADOW_CAP)
}

/// `cap`-parameterized copy so the cap-respecting behavior is unit-testable
/// without seeding `CODE_EVOLUTION_SHADOW_CAP` (50,000) rows.
fn copy_code_evolution_capped(
    live: &Arc<Storage>,
    shadow: &Arc<Storage>,
    projects: &BTreeSet<&str>,
    cap: usize,
) -> Result<usize> {
    let placeholders = vec!["?"; projects.len()].join(", ");
    // Finding 3 (WCR truth pass): `id` (TEXT PRIMARY KEY, so already unique
    // per row — see the `code_evolution` table definition in
    // `storage::migrations`) is a deterministic tiebreaker for rows sharing
    // the same `timestamp`. Without it, `LIMIT` selects an
    // implementation-defined subset among timestamp-tied rows at the cutoff
    // — SQLite is free to return a different one across query plans/DB
    // rebuilds, silently changing which `code_evolution` rows feed the B3
    // co-edit-weight bind tier between otherwise-identical gate runs.
    let select_sql = format!(
        "SELECT {EVOLUTION_COLS} FROM code_evolution
         WHERE project_name IN ({placeholders})
         ORDER BY timestamp DESC, id DESC
         LIMIT {cap}"
    );
    let project_params: Vec<&str> = projects.iter().copied().collect();

    let rows: Vec<Vec<rusqlite::types::Value>> = live.with_connection(|conn| {
        let mut stmt = conn.prepare(&select_sql)?;
        let mapped = stmt.query_map(rusqlite::params_from_iter(project_params.iter()), |row| {
            (0..EVOLUTION_COL_COUNT)
                .map(|i| row.get::<_, rusqlite::types::Value>(i))
                .collect::<rusqlite::Result<Vec<_>>>()
        })?;
        mapped
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    })?;

    let count = rows.len();
    if count > 0 {
        let insert_sql = format!(
            "INSERT OR REPLACE INTO code_evolution ({EVOLUTION_COLS}) VALUES ({})",
            vec!["?"; EVOLUTION_COL_COUNT].join(", ")
        );
        shadow.with_connection(|conn| {
            let tx = conn.unchecked_transaction()?;
            {
                let mut stmt = tx.prepare(&insert_sql)?;
                for row in &rows {
                    stmt.execute(rusqlite::params_from_iter(row.iter()))?;
                }
            }
            tx.commit()?;
            Ok(())
        })?;
    }
    Ok(count)
}

/// Run `repo_scan::scan_all` against `shadow` for at most
/// `WCR_SCAN_PROJECT_CAP` of `projects`, ranked by edge count (most-connected
/// project first, ties broken by project name for determinism) so scan
/// wall-time stays bounded on a corpus spanning many projects. Returns a
/// detail-string suffix listing any projects the cap skipped (empty when
/// nothing was skipped).
fn scan_repo_defs(
    shadow: &Arc<Storage>,
    snapshot: &GraphSnapshot,
    projects: &BTreeSet<&str>,
) -> Result<String> {
    let project_by_node: HashMap<&str, &str> = snapshot
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node.project.as_str()))
        .collect();
    let mut edge_counts: BTreeMap<&str, usize> = projects.iter().map(|p| (*p, 0usize)).collect();
    for edge in &snapshot.edges {
        if let Some(project) = project_by_node.get(edge.src_id.as_str()) {
            if let Some(count) = edge_counts.get_mut(project) {
                *count += 1;
            }
        }
    }

    let mut ranked: Vec<(&str, usize)> = edge_counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

    let scanned: Vec<&str> = ranked
        .iter()
        .take(WCR_SCAN_PROJECT_CAP)
        .map(|(project, _)| *project)
        .collect();
    let skipped: Vec<&str> = ranked
        .iter()
        .skip(WCR_SCAN_PROJECT_CAP)
        .map(|(project, _)| *project)
        .collect();

    for project in &scanned {
        shadow.with_connection(|conn| repo_scan::scan_all(conn, project).map(|_stats| ()))?;
    }

    if skipped.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!(
            "; repo-scan skipped {} project(s) (cap={WCR_SCAN_PROJECT_CAP}): {skipped:?}",
            skipped.len()
        ))
    }
}

/// Files over this size are skipped by `backfill_wcr_witnesses` — bounds gate
/// wall-time against a corpus containing very large source files.
const BACKFILL_MAX_FILE_BYTES: u64 = 512 * 1024;

/// Codex round 6 adversarial review (legacy-attribution correspondence is
/// NOT cross-version site identity): `call_legacy_correspondence` (see its
/// own doc comment, in `backfill_wcr_witnesses` below) proves that SOME
/// fresh call/import site's NAME/KIND attribution, under the frozen
/// pre-92179d1 rule, matches a historical pending edge's own src. It does
/// NOT prove that fresh site is the SAME physical call the historical edge
/// recorded — a genuinely REMOVED call from one def (e.g. a closure inside
/// `Alpha`) and an unrelated, newly ADDED call from another def (a closure
/// inside `Beta`) can both legacy-attribute to the identical historical src
/// (the module node, when both are anonymous closures) — indistinguishable
/// from attribution skew by correspondence alone, exactly as candidate COUNT
/// alone was indistinguishable in round 4 (`calls_any_src`'s doc comment).
/// Re-pointing on correspondence alone credits gate closure that the
/// evidence does not support.
///
/// Codex round 7 adversarial review (this gate's OWN round-6 implementation
/// was itself unsound): the round-6 version compared `code_nodes.body_hash`
/// — the SRC NODE's stored hash — against a fresh re-parse. That column is
/// MUTABLE: `storage::codegraph::upsert_node` refreshes it on every sighting,
/// and does so in a SEPARATE transaction from `replace_file_edges` (every
/// live write path — `hooks::post_tool_use::update_code_graph`,
/// `import::backfill`, `eval::codegraph::extract_and_store` — upserts nodes
/// FIRST, then replaces edges), with import continuing past per-file
/// failures. A partial failure (nodes refreshed to CURRENT content, edge
/// replace failed or simply never ran) left a STALE edge joined to a FRESH
/// node hash — the gate would see "content unchanged" and authenticate a
/// re-point the evidence never supported. The node's content identity says
/// nothing about whether THIS edge was ever re-examined against it.
///
/// The fix moves the hash onto the EDGE itself: `EdgeRow::src_content_hash`
/// is stamped by `extract_inner`'s `add_edge` closure at the SAME moment
/// (same extraction, same whole-file hash) the edge is created, and written
/// atomically with it by `replace_file_edges`'s single delete+insert
/// transaction — see that function's doc comment. The gate is now sound
/// regardless of `code_nodes` state: it never reads `code_nodes.body_hash`
/// at all. `edge_src_content_hash` is the PENDING edge's own stored stamp
/// (from the `code_edges.src_content_hash` column, read by
/// `backfill_wcr_witnesses`'s `pending_edges` query); `fresh_file_hash` is
/// the SAME hash (`extraction::codegraph::body_hash`) recomputed from the
/// file's CURRENT on-disk content by this backfill pass, once per file.
/// Equal hashes are a mechanical proof, not a guess: the OLD attribution
/// rule, applied to content byte-identical to today's, is what produced the
/// historical edge — so the historical edge and the fresh
/// legacy-corresponding site are the same physical call, full stop.
///
/// An empty `edge_src_content_hash` — never stamped (a legacy edge written
/// before this column existed) or stamped by a path that dropped it — is
/// categorically INELIGIBLE, never a guess: `body_hash` (a sha256 digest) is
/// never the empty string for any real text, so an empty stored hash can
/// never coincidentally equal a real fresh one. This is checked explicitly
/// anyway so the "missing never guesses" invariant is documented, not merely
/// accidental.
///
/// This function alone cannot distinguish WHY it returned `false` — empty
/// (no evidence at all) and non-empty-but-mismatched (positive evidence of
/// change) both come back `false` — but its two callers, in
/// `backfill_wcr_witnesses`, deliberately do NOT treat those two shapes the
/// same (WCR truth pass, Codex round 7, item 3): an EMPTY hash means the
/// pending edge is left COMPLETELY untouched (pending/unexplained forever,
/// never a guessed drift — the call may still exist under the new
/// attribution, this backfill simply never re-examined it), while a
/// MISMATCHED hash falls through to the ordinary drift guard (`can_drift`) —
/// the Alpha/Beta scenario, where the historical edge's own stamp proves the
/// file's content genuinely changed since it was recorded, so an absent
/// fresh site really is drift, not merely unproven. See each call site's own
/// comment for the split.
fn historical_src_content_unchanged(edge_src_content_hash: &str, fresh_file_hash: &str) -> bool {
    !edge_src_content_hash.is_empty() && edge_src_content_hash == fresh_file_hash
}

/// Witness backfill (WCR Phase 5, TASK 2; evidence propagation extended WCR
/// Phase 7; drifted-edge classification added WCR Phase 8, TASK B): for
/// every distinct (project, src_file) with pending (`resolved = 0`)
/// `calls`/`imports` edges in `shadow`, if the file still exists on disk,
/// re-extract it fresh and use the result to update ONLY the matching
/// pending edges **in the shadow**: a `calls` edge's `callee_kind` AND
/// `evidence` (`via:<qualifier>`, WCR Phase 6 TASK A), and an `imports`
/// edge's `evidence` (`from:<module>`). Matching is by `(src node name,
/// kind, bare target name)` — name-based, not id-based, since a freshly
/// re-extracted node's id may not equal the shadow's stale id.
///
/// Edges with NO match in the fresh extraction are candidates for `boundary
/// = 'drifted'`, `evidence = 'not_in_current_source'` (WCR Phase 8, TASK B)
/// — but ONLY when the absence is trustworthy evidence that the call/import
/// site itself genuinely vanished, not a symptom of extraction having
/// failed on this file. Truth-pass invariant (Finding 1): drift requires a
/// LIVE, PARSEABLE file whose CALLING SYMBOL SURVIVED re-extraction —
/// concretely, both of:
///   (b) the fresh fragment contains at least one def node (kind !=
///       "module") — proof the parse actually produced structure. A
///       fragment with zero def nodes is indistinguishable from an
///       extractor regression/panic/unsupported-language/undersized-source
///       short-circuit (`extract_graph_fragment` returns
///       `GraphFragment::default()` in every one of those cases) — an
///       extraction failure would otherwise unconditionally drift-classify
///       EVERY pending edge of an otherwise-readable file, and the release
///       gates would PASS precisely when extraction is broken.
///   (c) the edge's SOURCE node name is present among the fresh fragment's
///       def node names — the calling function itself still exists; only
///       ITS call genuinely vanished. A renamed/deleted caller is not
///       evidence that a specific call site inside it drifted — the whole
///       function is gone, which is a different (unaddressed here) fact.
/// (Extraction "returning successfully" is not checked separately: every
/// failure mode above collapses to zero total nodes, which already fails
/// (b).) When (b) or (c) fail, the edge is left COMPLETELY UNTOUCHED —
/// stays pending, stays unexplained — rather than misclassified as drift;
/// an extractor regression therefore cannot masquerade as drift and inflate
/// the closure-rate gate.
///
/// A genuinely drifted edge is round-trip-consistent with the live
/// pipeline: `replace_file_edges` would simply DELETE such an edge the next
/// time this file is touched for real; the read-only gate's shadow can't
/// delete (it never writes back to the live DB), so it classifies instead
/// — honest, evidenced silence, not a guess. `resolve_edges` recognizes a
/// pre-set `boundary = 'drifted'` and skips every tier for it (see
/// `resolver::ResolveStats::drifted`). Drifted marking only ever touches
/// edges this backfill did NOT match — a matched edge's `boundary` is left
/// exactly as `replace_file_edges` originally wrote it (`''`), never
/// overwritten here.
///
/// Only applies when fresh extraction actually ran: a file that fails to
/// re-extract (missing, unreadable, over `BACKFILL_MAX_FILE_BYTES`) is
/// `continue`d out of the loop BEFORE any of this runs, so its pending edges
/// are untouched here — left for the resolver's own `stale` tier
/// (`resolve_stale_or_unexplained`) to classify by disk-existence instead.
///
/// Never touches the live DB: `shadow` is always a `shadow_for_wcr`-built
/// in-memory copy. Returns `(files re-extracted, edges updated)` — the
/// second element counts only MATCHED edges (drifted edges are not
/// "updated" in the witness-propagation sense; query the shadow directly to
/// observe drift counts).
fn backfill_wcr_witnesses(shadow: &Arc<Storage>) -> Result<(usize, usize)> {
    let pending_files: Vec<(String, String)> = shadow.with_connection(|conn| {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT n.project, e.src_file
             FROM code_edges e JOIN code_nodes n ON n.id = e.src_id
             WHERE e.resolved = 0 AND e.dst_id LIKE 'name:%' AND e.kind IN ('calls', 'imports')
             ORDER BY n.project, e.src_file",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    })?;

    let mut files_touched = 0usize;
    let mut edges_updated = 0usize;

    for (project, src_file) in pending_files {
        let canon = canonical_repo_path(Path::new(&src_file));
        if !canon.is_file() {
            continue;
        }
        let metadata = match fs::metadata(&canon) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if metadata.len() > BACKFILL_MAX_FILE_BYTES {
            continue;
        }
        let source = match fs::read_to_string(&canon) {
            Ok(s) => s,
            Err(_) => continue,
        };
        // Codex round 7 adversarial review: the SAME whole-file hash
        // `extract_inner` would stamp onto every fresh edge from this file
        // (`extraction::codegraph::body_hash`) — recomputed here, once per
        // file, as the "current on-disk content" side of the re-point
        // eligibility gate (`historical_src_content_unchanged`, below).
        // Never a second hashing scheme.
        let fresh_file_hash = crate::extraction::codegraph::body_hash(&source);

        let fragment = extract_graph_fragment_for_file(
            &source,
            &src_file,
            "",
            &project,
            "wcr-backfill",
            "wcr-backfill",
        );
        files_touched += 1;

        // TASK 2 (WCR truth pass, X4 tier): persist local-binding names from
        // THIS SAME fresh-extraction fragment — never a second parse. See
        // `extraction::codegraph::GraphFragment::local_bindings`'s doc
        // comment. Same error-propagation convention as every other shadow
        // write in this function (`?` — a DB write failure here is exactly
        // as fatal to the backfill pass as any other).
        persist_local_bindings(shadow, &project, &src_file, &fragment.local_bindings)?;

        // Finding 3 (Codex round 4 adversarial review): kind-qualified —
        // `id -> (kind, name)`, not `id -> name` alone. `node_id` itself
        // already disambiguates by kind (the id hash is
        // `sha256(repo|file|kind|name)`), so a bare name CAN collide across
        // DIFFERENT def kinds in the same file (e.g. a `function helper`
        // alongside a top-level `const helper`) — throwing the kind away
        // before any name-based candidate lookup is what let
        // `shadow_name_to_id` silently resolve to the WRONG node's id on
        // such a collision.
        let id_to_name_kind: HashMap<&str, (&str, &str)> = fragment
            .nodes
            .iter()
            .map(|n| (n.id.as_str(), (n.kind.as_str(), n.name.as_str())))
            .collect();

        // Codex round 3 adversarial review (the SAME id-mismatch class the
        // finding below is about): `fragment` is a THROWAWAY re-parse —
        // `extract_graph_fragment_for_file` below is called with `repo = ""`
        // (a fixed backfill placeholder), so every id in `fragment.nodes`/
        // `fragment.edges` is `node_id("", file, kind, name)`. The LIVE
        // pipeline (`hooks::post_tool_use`, `import::backfill`) calls the
        // SAME extractor with `repo = project` (the real project name) —
        // so a fresh fragment's own ids NEVER match this shadow's actual
        // `code_nodes` rows (copied verbatim from the live DB) whenever
        // `project` is non-empty, which is effectively always. Only NAMES
        // are safe to compare across the two id spaces (`def_names`,
        // `id_to_name_kind`'s VALUES, `local_bindings`'s scope strings —
        // none of those are ids). `shadow_name_to_id` is the bridge: the
        // REAL, disk-verified `code_nodes.id` for a given (kind, name) in
        // THIS shadow — required any time this backfill needs to WRITE a
        // node id into `code_edges`/`edge_scope_chains` (re-pointing
        // `src_id`, and translating `fragment.call_scope_chains`'
        // fresh-fragment keys into real ones before persisting), never for
        // pure name-to-name comparisons, which never needed this in the
        // first place.
        //
        // Finding 3 (Codex round 4 adversarial review): keyed by
        // `(kind, name)`, never bare `name` — a bare-name collision across
        // TWO DIFFERENT kinds in this shadow's own `code_nodes` (e.g. a
        // `function helper` node AND a `type helper` node both present for
        // this file) used to be silently resolved by picking whichever id
        // sorted lexicographically smallest (`ORDER BY name, id` +
        // first-insert-wins) — a GUESS, indistinguishable from a correct
        // match at the call site. A `(kind, name)` pair that resolves to
        // MORE THAN ONE id (should be structurally impossible given
        // `node_id`'s own hash includes `kind`, but the shadow is a copy
        // that could in principle carry duplicate/legacy rows) is treated
        // identically to a MISSING match — removed from the map entirely —
        // so every caller's `.get()` uniformly means "no valid match",
        // never "pick one anyway".
        let shadow_name_to_id: HashMap<(String, String), String> = {
            let rows: Vec<(String, String, String)> = shadow.with_connection(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT kind, name, id FROM code_nodes WHERE project = ?1 AND file = ?2
                     ORDER BY kind, name, id",
                )?;
                let mapped = stmt.query_map(rusqlite::params![project, src_file], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?;
                mapped
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(Into::into)
            })?;
            let mut counts: HashMap<(String, String), u32> = HashMap::new();
            let mut map: HashMap<(String, String), String> = HashMap::new();
            for (kind, name, id) in rows {
                let key = (kind, name);
                *counts.entry(key.clone()).or_insert(0) += 1;
                map.entry(key).or_insert(id);
            }
            map.retain(|key, _| counts.get(key) == Some(&1));
            map
        };

        // X4 adversarial review, Finding 4; multi-chain-per-edge as of the
        // Codex round 4 adversarial review, Finding 1: persist EVERY fresh
        // calls/imports edge's OWN DISTINCT scope chains, unconditionally —
        // never filtered to only the edges some pending row happens to
        // match (same philosophy as `persist_local_bindings`, immediately
        // above). Keys are TRANSLATED from `fragment.call_scope_chains`'
        // fresh-fragment ids into real shadow ids (via `id_to_name_kind`
        // then the kind-qualified `shadow_name_to_id`, per that map's own
        // doc comment — Finding 3) before persisting — the raw
        // fresh-fragment key would never match this shadow's
        // `code_edges.src_id` at read time otherwise. A fresh call/import
        // site whose owning (kind, name) has no unique shadow `code_nodes`
        // row (missing OR ambiguous — Finding 3) is skipped: no key exists
        // for `resolve_edges`'s chain lookup to ever need anyway. Chain SETS
        // for the same translated key are merged (`.extend`), not
        // overwritten, so two fresh-fragment src ids that both translate to
        // the same real shadow id (should not normally happen, but never
        // silently drops data if it does) keep every chain either
        // contributed.
        let mut translated_chains: BTreeMap<(String, String, String), BTreeSet<String>> =
            BTreeMap::new();
        for ((fresh_src_id, dst_id, kind), chains) in &fragment.call_scope_chains {
            let Some((fresh_kind, fresh_name)) = id_to_name_kind.get(fresh_src_id.as_str()) else {
                continue;
            };
            let lookup_key = (fresh_kind.to_string(), fresh_name.to_string());
            let Some(real_id) = shadow_name_to_id.get(&lookup_key) else {
                continue;
            };
            translated_chains
                .entry((real_id.clone(), dst_id.clone(), kind.clone()))
                .or_default()
                .extend(chains.iter().cloned());
        }
        persist_call_scope_chains(shadow, &project, &src_file, &translated_chains)?;

        // Finding 1 (WCR truth pass): def node names actually present in the
        // FRESH fragment — condition (b) from this function's doc comment,
        // "the parse actually produced structure". `kind != "module"`
        // because the synthetic module node is unconditionally present even
        // when extraction found zero functions/types/consts (or panicked
        // and fell back to `GraphFragment::default()`, which has no nodes
        // at all — `def_names` is empty either way). Used below, per pending
        // edge, to gate `mark_drifted`: an unmatched edge only drifts when
        // this set is non-empty AND contains the edge's own calling
        // function's name (condition (c)) — never on name identity of the
        // TARGET, which is exactly what this backfill is trying to explain.
        let def_names: HashSet<&str> = fragment
            .nodes
            .iter()
            .filter(|n| n.kind != "module")
            .map(|n| n.name.as_str())
            .collect();

        // Codex round 7 adversarial review: the code_nodes-hash-based
        // content-identity map this comment used to describe (keyed
        // `(kind, name) -> code_nodes.body_hash`) is REMOVED — it is the
        // refuted mechanism (see `historical_src_content_unchanged`'s doc
        // comment for the full finding: `code_nodes.body_hash` is mutable
        // and refreshed in a transaction separate from the edge replace, so
        // it cannot soundly authenticate a specific pending EDGE). The
        // re-point gate now reads `fresh_file_hash` (computed once per file,
        // above) against each pending edge's own stored
        // `code_edges.src_content_hash`, fetched by the `pending_edges`
        // query below — no per-(kind, name) map needed.

        // WCR Phase 7: `(callee_kind, evidence)` — the `evidence` half is
        // the `via:<qualifier>` data captured at extraction time (WCR Phase
        // 6, TASK A), added AFTER this backfill function was originally
        // written (WCR Phase 5, TASK 2 — which only knew about
        // `callee_kind`). Without it, a shadow's calls edges never carry
        // qualifier evidence at all (`snapshot()` deliberately zeroes
        // `evidence`/`boundary` on copy — see its own doc comment), so
        // `qualifier_tier`/`qualifier_import_tier` get zero live-corpus
        // signal despite being fully exercised in the resolver's own unit
        // tests: real bug, not a hypothetical.
        //
        // Finding (Codex round 3, attribution-skewed re-point): keyed by
        // bare target name -> {src_name -> (src_kind, callee_kind, evidence)}
        // — EVERY fresh edge of this file, regardless of src attribution —
        // rather than a flat `(src_name, bare)` -> value map. The inner
        // `BTreeMap` gives an exact-match lookup at a specific `src_name`
        // key (the pre-existing behavior, unchanged), AND, when that misses,
        // the per-candidate `(src_kind, callee_kind, evidence)` data the
        // re-point below needs once `call_legacy_correspondence` (Codex
        // round 5, immediately below) has decided WHICH candidate — if
        // any — a pending edge's stale src provably corresponds to.
        // `src_kind` (Finding 3, Codex round 4) is threaded alongside so the
        // WINNING candidate's real shadow id can be resolved via the
        // kind-qualified `shadow_name_to_id` instead of a bare-name lookup —
        // see that map's own doc comment. Deliberately NAME-keyed (not
        // `edge.src_id`) — see `shadow_name_to_id`'s doc comment for why the
        // fresh fragment's OWN id is a different (wrong) id space than this
        // shadow's `code_nodes`. `imports` edges are always module-sourced
        // (see `extract_inner`'s imports loop — `src` is always
        // `module_id`), so their inner map in practice never has more than
        // one key; kept symmetric with `calls` rather than special-cased,
        // since a single, shared code path is less risk than two subtly
        // different ones.
        //
        // Codex round 4 (Finding 2) used to gate re-pointing on THIS map's
        // candidate COUNT alone (exactly one entry -> re-point). Codex round
        // 5 adversarial review found that unsound: candidate-count
        // uniqueness is not identity. A genuinely REMOVED call in one
        // function (`ghost`) plus one unrelated, unchanged function
        // (`alpha`) that ALSO happens to call the same bare name is
        // indistinguishable from attribution skew by count alone — and was
        // being wrongly re-pointed to `alpha`, corrupting provenance and
        // suppressing the legitimate drift on `ghost`'s own edge. This map
        // alone is no longer sufficient to decide re-pointing — see
        // `call_legacy_correspondence`, immediately below.
        let mut calls_any_src: BTreeMap<String, BTreeMap<String, (String, String, String)>> =
            BTreeMap::new();
        let mut imports_any_src: BTreeMap<String, BTreeMap<String, (String, String)>> =
            BTreeMap::new();
        for edge in &fragment.edges {
            let Some(bare) = edge.dst_id.strip_prefix("name:") else {
                continue;
            };
            let Some(&(src_kind, src_name)) = id_to_name_kind.get(edge.src_id.as_str()) else {
                continue;
            };
            match edge.kind.as_str() {
                "calls" => {
                    calls_any_src.entry(bare.to_string()).or_default().insert(
                        src_name.to_string(),
                        (
                            src_kind.to_string(),
                            edge.callee_kind.clone(),
                            edge.evidence.clone(),
                        ),
                    );
                }
                "imports" => {
                    imports_any_src.entry(bare.to_string()).or_default().insert(
                        src_name.to_string(),
                        (src_kind.to_string(), edge.evidence.clone()),
                    );
                }
                _ => {}
            }
        }

        // Codex round 5 adversarial review: POSITIVE transition evidence,
        // replacing round 4's candidate-count re-point rule (see
        // `calls_any_src`'s doc comment, immediately above, for the exact
        // finding). Keyed by `(kind, bare_callee, legacy_src_name)` ->
        // the SET of distinct CURRENT src names among fresh call/import
        // SITES whose `legacy_src_attribution` (the FROZEN pre-92179d1
        // rule) equals `legacy_src_name` — built straight from
        // `fragment.call_attribution_pairs` (per-SITE, not aggregated by
        // edge, since two sites aggregated into the same fresh edge can
        // carry DIFFERENT legacy attributions).
        //
        // A pending historical edge `(src_name, callee, kind)` that misses
        // the exact-match lookup above is eligible for re-pointing ONLY when
        // this map has an entry for `(kind, callee, src_name)` — i.e. some
        // fresh call/import site, under the OLD rule, provably attributes to
        // EXACTLY this historical src. That is direct physical evidence the
        // historical edge and that fresh site are the SAME call, just
        // recorded under different attribution rules — not a guess from
        // bare-name survival. When the resulting current-src set has more
        // than one distinct member, WHICH of them owns the historical edge
        // is still unproven (two sites both attribute to the old src under
        // the old rule, but disagree under the new one) — left untouched,
        // same "ambiguous, never guess" outcome round 4 established, just
        // reached via correspondence instead of count. When NO fresh site
        // legacy-corresponds to this historical src at all, this is not
        // attribution skew — the edge falls through to the pre-existing
        // `can_drift` guard below and is judged as ordinary drift evidence,
        // honestly, rather than being permanently stuck merely because the
        // bare name happens to survive SOMEWHERE ELSE in the file.
        let mut call_legacy_correspondence: BTreeMap<(String, String, String), BTreeSet<String>> =
            BTreeMap::new();
        for (legacy_src, current_src, callee, kind) in &fragment.call_attribution_pairs {
            call_legacy_correspondence
                .entry((kind.clone(), callee.clone(), legacy_src.clone()))
                .or_default()
                .insert(current_src.clone());
        }
        // No early bailout when both maps are empty (WCR Phase 8, TASK B):
        // that shape — fresh extraction found NO calls/imports at all in
        // this file — is CONSISTENT WITH drift (every call/import site was
        // removed) but is ALSO exactly what a broken extraction looks like;
        // `def_names` (Finding 1, above) is what actually decides per edge
        // whether that absence is trustworthy. The loop below must still
        // run either way: an unmatched edge that fails the `def_names`
        // check is left untouched, not skipped outright — "still pending,
        // never classified" IS the correct outcome for it, not a shortcut
        // to take before checking.

        // Codex round 7 adversarial review: `e.src_content_hash` — the
        // pending EDGE's OWN stamped hash, not `n.body_hash` (the SRC NODE's
        // mutable, separately-refreshed hash — the refuted round-6
        // mechanism; see `historical_src_content_unchanged`'s doc comment for
        // the full finding) — is pulled alongside `n.name` now, required by
        // the re-point content-identity gate below.
        let pending_edges: Vec<(String, String, String, String, String)> =
            shadow.with_connection(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT e.src_id, e.dst_id, e.kind, n.name, e.src_content_hash
                     FROM code_edges e JOIN code_nodes n ON n.id = e.src_id
                     WHERE e.resolved = 0 AND e.dst_id LIKE 'name:%' AND e.kind IN ('calls', 'imports')
                       AND e.src_file = ?1 AND n.project = ?2
                     ORDER BY e.src_id, e.dst_id, e.kind",
                )?;
                let rows = stmt.query_map(rusqlite::params![src_file, project], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(Into::into)
            })?;

        for (src_id, dst_id, kind, src_name, edge_src_content_hash) in pending_edges {
            let Some(bare) = dst_id.strip_prefix("name:") else {
                continue;
            };
            // Finding 1 (WCR truth pass) conditions (b) + (c): drift is only
            // trustworthy when the fresh fragment produced at least one def
            // node at all AND this edge's OWN calling function is among
            // them — i.e. the caller genuinely still exists, so its
            // specific call/import site (not matched below) is what
            // vanished, rather than the whole file having failed to parse
            // into structure or the caller itself having been renamed/
            // removed (a different, unaddressed fact — see this function's
            // doc comment).
            //
            // TASK 1 (WCR truth pass): `src_name == src_file` extends
            // condition (c) to MODULE-LEVEL (top-level) call/import sites —
            // every `imports` edge, and any `calls` edge whose enclosing def
            // is the file itself rather than a named function, is sourced
            // from the synthetic module node, whose OWN `code_nodes.name` is
            // the file path (see `extract_inner`'s `mk_node`/module-node
            // construction — `name: file.into()`), never a def_names entry
            // (def_names deliberately excludes `kind == "module"`, condition
            // (b)'s own emptiness-diagnostic). Without this, condition (c)
            // could never be satisfied for a module-level edge — comparing a
            // file path against function/type/const names is a category
            // mismatch — so such an edge could NEVER drift no matter how
            // stale, even on a file that re-extracts perfectly cleanly. A
            // module's identity IS the file itself: if the file re-extracted
            // with real structure (condition (b) still holds) and this exact
            // (module, kind, bare name) triple isn't in the fresh fragment,
            // that is exactly as trustworthy as a named function surviving —
            // there is no "renamed enclosing function" ambiguity to guard
            // against at module scope. Verified against a real, live-corpus
            // case: 8 stale `imports`/`calls` edges targeting the literal
            // name `import` (predating the `is_noise_callee` filter that now
            // drops it at extraction time — WCR Phase 7 TASK D, and this same
            // filter applied to `import_symbols`) were permanently stuck
            // `unexplained` because every one of them is module-scoped.
            //
            // Finding 3 (X4 adversarial review): `fragment.parse_clean` is
            // now REQUIRED too, for BOTH module-level and function-level
            // edges. Condition (b) alone (`!def_names.is_empty()`) only
            // proves the parse recovered AT LEAST ONE real def — a partial
            // extraction regression can still recover one function while
            // silently dropping every module-level import/call capture
            // elsewhere in the same degraded parse tree. Without this, such
            // a regression would wrongly drift-classify every historical
            // module-level edge (they'd all look like "genuinely removed"),
            // which both inflates `closure_rate` and, since drift is
            // excluded from `internal_binding_rate`'s denominator, masks the
            // very breakage the release gates exist to catch. A genuinely
            // clean re-parse (no ERROR/MISSING nodes anywhere) is the
            // positive signal that the ABSENCE of a match is trustworthy —
            // see `extraction::codegraph::tree_has_error`'s doc comment.
            let can_drift = !def_names.is_empty()
                && fragment.parse_clean
                && (def_names.contains(src_name.as_str()) || src_name == src_file);
            // Finding (Codex round 3, attribution-skewed re-point): checked
            // BEFORE any drift decision, per kind below — an exact-key miss
            // (`calls_any_src`/`imports_any_src` has no entry at THIS edge's
            // own `src_name`) is not automatically drift when the callee
            // exists SOMEWHERE ELSE in the fresh fragment: it first goes
            // through the `call_legacy_correspondence` check (Codex round 5)
            // below, which decides re-point vs ambiguous vs "fall through to
            // the ordinary drift guard" — see that map's doc comment. Only
            // when the whole per-kind candidate map for this bare name is
            // empty (the callee is absent from the file's ENTIRE fresh edge
            // set, not just from this src) does `can_drift` (parse_clean +
            // def_names, unchanged) get to run WITHOUT going through
            // correspondence first — there is no candidate to correspond to.
            match kind.as_str() {
                "calls" => {
                    match calls_any_src.get(bare) {
                        Some(candidates) if candidates.contains_key(src_name.as_str()) => {
                            // Exact match — same src attribution as before this
                            // finding; only callee_kind/evidence are refreshed.
                            // `src_kind` unused here — no id translation
                            // needed, `src_id` is already this edge's own.
                            let (_src_kind, new_kind, new_evidence) =
                                &candidates[src_name.as_str()];
                            shadow.with_connection(|conn| {
                                conn.execute(
                                    "UPDATE code_edges SET callee_kind = ?1, evidence = ?2
                                 WHERE src_id = ?3 AND dst_id = ?4 AND kind = 'calls'",
                                    rusqlite::params![new_kind, new_evidence, src_id, dst_id],
                                )?;
                                Ok(())
                            })?;
                            edges_updated += 1;
                        }
                        Some(candidates) if !candidates.is_empty() => {
                            // Codex round 5 adversarial review: re-point ONLY
                            // on POSITIVE transition evidence — a fresh call
                            // site that, under the FROZEN pre-92179d1 rule,
                            // provably attributes to THIS historical edge's
                            // own src (`call_legacy_correspondence`'s doc
                            // comment has the full finding). Candidate COUNT
                            // alone (the round 4 rule this replaces) could not
                            // tell "attribution moved" apart from "a real edit
                            // made a second, unrelated caller" — this can.
                            let legacy_key = (kind.clone(), bare.to_string(), src_name.clone());
                            match call_legacy_correspondence.get(&legacy_key) {
                                Some(current_srcs) if current_srcs.len() == 1 => {
                                    let winning = current_srcs.iter().next().expect("len == 1");
                                    // Codex round 6 adversarial review: legacy
                                    // correspondence alone is not cross-version
                                    // site identity — see
                                    // `historical_src_content_unchanged`'s doc
                                    // comment for the full finding (the
                                    // Alpha/Beta case: a removed closure-Alpha
                                    // call and an unrelated added closure-Beta
                                    // call both legacy-attribute to this same
                                    // historical src). Gated DEDUCTIVELY on the
                                    // PENDING EDGE's own stamped content hash
                                    // being provably unchanged since it was
                                    // recorded (Codex round 7: not the src
                                    // node's mutable hash — see that same doc
                                    // comment for why), checked BEFORE
                                    // trusting the correspondence to re-point
                                    // at all.
                                    if historical_src_content_unchanged(
                                        &edge_src_content_hash,
                                        &fresh_file_hash,
                                    ) {
                                        if let Some((src_kind, new_kind, new_evidence)) =
                                            candidates.get(winning)
                                        {
                                            // Finding 3 (Codex round 4): kind-qualified
                                            // lookup — a missing or non-unique
                                            // (kind, name) match in
                                            // `shadow_name_to_id` is NO match, never
                                            // a lexicographic guess. See that map's
                                            // doc comment.
                                            if let Some(new_src_id) = shadow_name_to_id
                                                .get(&(src_kind.clone(), winning.clone()))
                                            {
                                                repoint_or_dedupe_edge(
                                                    shadow,
                                                    &src_id,
                                                    &dst_id,
                                                    "calls",
                                                    new_src_id,
                                                    Some(new_kind),
                                                    new_evidence,
                                                )?;
                                                edges_updated += 1;
                                            }
                                            // else: the winning candidate's (kind,
                                            // name) has no valid real shadow id —
                                            // never guess by falling back to a
                                            // different one. Leave untouched.
                                        }
                                        // else: the legacy-matched winning src isn't
                                        // itself a `calls_any_src` candidate —
                                        // structurally shouldn't happen (every
                                        // `call_attribution_pairs` site also
                                        // produced a `fragment.edges` entry at its
                                        // own current src), but never guess if it
                                        // somehow is. Leave untouched.
                                    } else if edge_src_content_hash.is_empty() {
                                        // WCR truth pass, Codex round 7, item 3:
                                        // an EMPTY edge hash is a legacy row —
                                        // never stamped, so there is NO EVIDENCE
                                        // either way about whether this file's
                                        // content changed since the edge was
                                        // recorded. Drifting it would claim
                                        // positive knowledge ("the call is
                                        // gone") this backfill does not have —
                                        // the call may still exist under the new
                                        // attribution, just unprovably so. Left
                                        // COMPLETELY untouched: pending,
                                        // unexplained — never a guess in either
                                        // direction. Distinct from the mismatch
                                        // case immediately below, which DOES have
                                        // positive evidence.
                                    } else if can_drift {
                                        // Content-identity gate failed on a
                                        // NON-EMPTY, MISMATCHED hash: the
                                        // historical edge's own stamped content
                                        // provably changed since it was recorded
                                        // — cross-version site identity is
                                        // unprovable, but genuine drift evidence
                                        // DOES exist (the Alpha/Beta case — see
                                        // `historical_src_content_unchanged`'s
                                        // doc comment). Fall through to the SAME
                                        // honest drift classification a missing
                                        // legacy correspondence gets, immediately
                                        // below — never a guessed re-point.
                                        mark_drifted(
                                            shadow,
                                            &src_id,
                                            &dst_id,
                                            "calls",
                                            &edge_src_content_hash,
                                        )?;
                                    }
                                }
                                Some(_) => {
                                    // >= 2 distinct current srcs among
                                    // legacy-matched sites: WHICH one owns this
                                    // historical edge is unproven — ambiguous,
                                    // never guess. Left untouched: pending,
                                    // unexplained, the honest bucket for "true
                                    // but unattributable". Never drifted either
                                    // — a legacy-corresponding site DOES exist.
                                }
                                None => {
                                    // NO fresh site legacy-corresponds to this
                                    // historical src at all — this is not
                                    // attribution skew (a genuinely removed
                                    // call, e.g. `ghost`'s own `helper()` call
                                    // deleted, looks exactly like this even
                                    // though `helper` still exists elsewhere in
                                    // the file via an unrelated caller). Fall
                                    // through to the ordinary drift guard —
                                    // honest classification, not permanently
                                    // stuck merely because the bare name
                                    // survives SOMEWHERE ELSE. NOTE: unlike
                                    // the correspondence branch above, this
                                    // arm has no legacy-correspondence match
                                    // to gate on — `can_drift` alone would
                                    // let an empty-stamped legacy edge earn
                                    // drift credit (Codex round 8). The
                                    // empty-stamp invariant is enforced
                                    // structurally inside `mark_drifted`
                                    // itself now, not here — see its doc
                                    // comment.
                                    if can_drift {
                                        mark_drifted(
                                            shadow,
                                            &src_id,
                                            &dst_id,
                                            "calls",
                                            &edge_src_content_hash,
                                        )?;
                                    }
                                }
                            }
                        }
                        _ => {
                            if can_drift {
                                // WCR Phase 8, TASK B (Finding 1 truth pass):
                                // fresh extraction ran cleanly, the calling
                                // function survived, but this callee isn't in
                                // the fresh fragment AT ALL (any src) — the
                                // call site genuinely drifted out of the source
                                // since this edge was recorded. Codex round 8:
                                // `can_drift` says nothing about the edge's
                                // own content stamp — `mark_drifted` itself
                                // is the structural guard against drifting an
                                // empty-stamped legacy edge here; see its doc
                                // comment.
                                mark_drifted(
                                    shadow,
                                    &src_id,
                                    &dst_id,
                                    "calls",
                                    &edge_src_content_hash,
                                )?;
                            }
                            // else: leave untouched — (b) or (c) failed, so an
                            // unmatched edge is not trustworthy drift evidence.
                            // Stays pending and unexplained (honest), not
                            // misclassified.
                        }
                    }
                }
                "imports" => match imports_any_src.get(bare) {
                    Some(candidates) if candidates.contains_key(src_name.as_str()) => {
                        // `src_kind` unused here — no id translation needed.
                        let (_src_kind, new_evidence) = &candidates[src_name.as_str()];
                        shadow.with_connection(|conn| {
                            conn.execute(
                                "UPDATE code_edges SET evidence = ?1
                                 WHERE src_id = ?2 AND dst_id = ?3 AND kind = 'imports'",
                                rusqlite::params![new_evidence, src_id, dst_id],
                            )?;
                            Ok(())
                        })?;
                        edges_updated += 1;
                    }
                    Some(candidates) if !candidates.is_empty() => {
                        // Same correspondence-gated re-point rule as `calls`,
                        // above (Codex round 5) — in practice a near-no-op for
                        // `imports` (both legacy AND current attribution are
                        // ALWAYS the module node, see
                        // `GraphFragment::call_attribution_pairs`'s doc
                        // comment, so this can only ever fire when the
                        // historical src itself already equals the module —
                        // which the exact-match arm above would have already
                        // caught), kept symmetric rather than special-cased.
                        let legacy_key = (kind.clone(), bare.to_string(), src_name.clone());
                        match call_legacy_correspondence.get(&legacy_key) {
                            Some(current_srcs) if current_srcs.len() == 1 => {
                                let winning = current_srcs.iter().next().expect("len == 1");
                                // Codex round 6/7 adversarial review: same
                                // edge-hash content-identity gate as the
                                // `calls` arm above — see
                                // `historical_src_content_unchanged`'s doc
                                // comment.
                                if historical_src_content_unchanged(
                                    &edge_src_content_hash,
                                    &fresh_file_hash,
                                ) {
                                    if let Some((src_kind, new_evidence)) = candidates.get(winning)
                                    {
                                        if let Some(new_src_id) = shadow_name_to_id
                                            .get(&(src_kind.clone(), winning.clone()))
                                        {
                                            repoint_or_dedupe_edge(
                                                shadow,
                                                &src_id,
                                                &dst_id,
                                                "imports",
                                                new_src_id,
                                                None,
                                                new_evidence,
                                            )?;
                                            edges_updated += 1;
                                        }
                                        // else: no valid real shadow id — never guess.
                                    }
                                } else if edge_src_content_hash.is_empty() {
                                    // WCR truth pass, Codex round 7, item 3:
                                    // empty edge hash — no evidence either way,
                                    // left completely untouched. Same reasoning
                                    // as the `calls` arm above.
                                } else if can_drift {
                                    // Content-identity gate failed on a
                                    // non-empty, MISMATCHED hash — same fall
                                    // through as the `calls` arm above.
                                    mark_drifted(
                                        shadow,
                                        &src_id,
                                        &dst_id,
                                        "imports",
                                        &edge_src_content_hash,
                                    )?;
                                }
                            }
                            Some(_) => {
                                // ambiguous — never guess. Leave untouched.
                            }
                            None => {
                                // Codex round 8: same empty-stamp exposure as
                                // the `calls` `None` arm above — no
                                // legacy-correspondence match to gate on
                                // here, so `mark_drifted` itself is what
                                // stops an empty-stamped legacy edge from
                                // earning drift credit; see its doc comment.
                                if can_drift {
                                    mark_drifted(
                                        shadow,
                                        &src_id,
                                        &dst_id,
                                        "imports",
                                        &edge_src_content_hash,
                                    )?;
                                }
                            }
                        }
                    }
                    _ => {
                        // Codex round 8: same as the `calls` catch-all arm
                        // above — `mark_drifted` is the structural guard
                        // against drifting an empty-stamped legacy edge.
                        if can_drift {
                            mark_drifted(
                                shadow,
                                &src_id,
                                &dst_id,
                                "imports",
                                &edge_src_content_hash,
                            )?;
                        }
                        // else: leave untouched — see the `calls` arm above.
                    }
                },
                _ => {}
            }
        }
    }

    Ok((files_touched, edges_updated))
}

/// TASK 2 (WCR truth pass, X4 tier); transactional replace added by the X4
/// adversarial review (Finding 2): persist `bindings` — `(scope, name)`
/// local-binding pairs from the SAME fresh-extraction fragment
/// `backfill_wcr_witnesses` already computed for this (project, file), never
/// a second parse — into the shadow's `local_bindings` witness table.
///
/// DELETE-then-INSERT per (project, file), inside one transaction,
/// UNCONDITIONALLY — including when `bindings` is empty (the DELETE still
/// runs). Before this fix, an empty fresh set hit an early `return Ok(())`
/// and a non-empty set only ever `INSERT OR IGNORE`d, so a file that used to
/// have local bindings and no longer does (all renamed/removed) — or a file
/// reprocessed after edits changed WHICH names are bound — would keep every
/// STALE row from a prior backfill forever: witnesses for names that no
/// longer exist in the file, silently misclassifying future edges. Today's
/// eval path always rebuilds the shadow from scratch per run (so this was
/// latent, not live-observable), but a planned future daemon persisting
/// straight into the LIVE DB across repeated runs would make it a real
/// landmine — this fix closes it at the source rather than relying on the
/// caller to always start from an empty table.
fn persist_local_bindings(
    shadow: &Arc<Storage>,
    project: &str,
    file: &str,
    bindings: &BTreeSet<(String, String)>,
) -> Result<()> {
    shadow.with_connection(|conn| {
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM local_bindings WHERE project = ?1 AND file = ?2",
            rusqlite::params![project, file],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO local_bindings (project, file, scope, name) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (scope, name) in bindings {
                stmt.execute(rusqlite::params![project, file, scope, name])?;
            }
        }
        tx.commit()?;
        Ok(())
    })
}

/// X4 adversarial review, Finding 4; multi-chain-per-edge as of the Codex
/// round 4 adversarial review, Finding 1: persist `chains` — for EVERY
/// fresh `calls`/`imports` edge, ALL of its DISTINCT call/import-site scope
/// chains, from the SAME fresh-extraction fragment `backfill_wcr_witnesses`
/// already computed for this (project, file), never a second parse — into
/// the shadow's `edge_scope_chains` witness table. Same transactional
/// DELETE-then-INSERT-per-(project, file) shape as `persist_local_bindings`,
/// immediately above, and for the same reason: unconditional, including
/// when `chains` is empty, so a file reprocessed after edits changed WHICH
/// call/import sites exist never keeps a stale chain row for a site that no
/// longer exists.
///
/// Keyed by `(src_id, dst_id, kind, chain)` — `edge_scope_chains`' own
/// primary key (Finding 1: `chain` joined the key so a multi-occurrence
/// edge gets one row PER distinct chain, not one row total) — not
/// `(project, file, ...)` in the table's own PK, so this DELETE scopes by
/// `project`/`file` columns rather than by key prefix; see the
/// `edge_scope_chains` migration comment for why a matched OR re-pointed
/// pending edge (`backfill_wcr_witnesses`'s per-edge loop, which runs AFTER
/// this) is guaranteed to end up with at least one row present in this
/// table.
fn persist_call_scope_chains(
    shadow: &Arc<Storage>,
    project: &str,
    file: &str,
    chains: &BTreeMap<(String, String, String), BTreeSet<String>>,
) -> Result<()> {
    shadow.with_connection(|conn| {
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM edge_scope_chains WHERE project = ?1 AND file = ?2",
            rusqlite::params![project, file],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO edge_scope_chains (project, file, src_id, dst_id, kind, chain)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for ((src_id, dst_id, kind), chain_set) in chains {
                for chain in chain_set {
                    stmt.execute(rusqlite::params![project, file, src_id, dst_id, kind, chain])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    })
}

/// Codex round 3 adversarial review: re-point a pending edge's `src_id`
/// from `old_src_id` to `new_src_id`, safe against the case where a row
/// ALREADY EXISTS at the target `(new_src_id, dst_id, kind)` key — a REAL
/// live-corpus scenario, not a hypothetical: a file can carry BOTH a stale,
/// old-attribution edge AND an already-correctly-attributed edge for the
/// SAME callee simultaneously, if only some of its call/import sites were
/// touched by a live re-extraction since commit 92179d1 (which unified
/// closure-nested call src attribution with binding-scope attribution) —
/// confirmed against the live gate, which hit exactly this: a blind
/// `UPDATE ... SET src_id = ?` collided with `code_edges`' own PRIMARY KEY
/// `(src_id, dst_id, kind)`, and the resulting SQLite constraint error
/// propagated all the way out of `backfill_wcr_witnesses` (a single `?` per
/// per-edge write, no per-file catch), silently leaving EVERY file after
/// the collision entirely un-backfilled for that run — not a partial
/// failure, a near-total one.
///
/// When the target already exists, the OLD `(old_src_id, dst_id, kind)` row
/// is a confirmed duplicate of an edge already tracked correctly elsewhere.
/// Finding 4 (Codex round 4 adversarial review): its provenance is MERGED
/// into the surviving (target) row FIRST, in the SAME transaction, before
/// the stale row is deleted — the pre-fix version deleted it unconditionally
/// and left the target completely untouched, silently discarding whatever
/// the stale row alone carried (a higher `weight`, older `conv_id`/
/// `session_id`, and the fresh `callee_kind`/`evidence` this very call was
/// asked to apply, which the old code only ever wrote in the NON-collision
/// branch). The merge, per `code_edges`' own column semantics:
/// - `weight`: SUMMED, not maxed. `extraction::codegraph::extract_inner`'s
///   own `add_edge` establishes this column as ADDITIVE (`weight += 1.0` per
///   physical occurrence folded into one aggregated edge) — the stale and
///   surviving rows are two different extraction generations' occurrence
///   counts for what re-pointing has just determined is the SAME logical
///   edge, so their sum is the honest combined count.
/// - `conv_id`/`session_id`: `code_edges` has only ONE slot for these
///   (unlike `code_nodes`' `first_conv_id`/`last_conv_id` pair) — the pair
///   from whichever of the two rows has the SMALLER `rowid` (SQLite's own
///   monotonic insertion order, exact even within one transaction, unlike
///   `created_at`'s second-resolution timestamp) is kept, preserving the
///   EARLIEST first-seen provenance pairing intact rather than splitting
///   `conv_id` from the `session_id` it was actually recorded with.
/// - `callee_kind`/`evidence`: ALWAYS refreshed from the CURRENT fresh
///   extraction (`new_callee_kind`/`new_evidence`, the caller's own fresh
///   candidate data) — same as the non-collision branch below, now applied
///   uniformly regardless of which branch runs.
///
/// This is deterministic even when several stale rows in the same file
/// converge on the same target: `backfill_wcr_witnesses` processes
/// `pending_edges` in a fixed `ORDER BY src_id, dst_id, kind`, so the first
/// one to be processed creates/updates the target row and every later one
/// finds it already there, merges into it, and deletes itself — same final
/// state on every run. Otherwise (the common case, no collision), a plain
/// `UPDATE` moves `src_id` in place and refreshes the fresh edge's own data
/// — `callee_kind`/`evidence` for `calls` (`new_callee_kind = Some(_)`), or
/// just `evidence` for `imports` (`new_callee_kind = None` — `imports` edges
/// have no `callee_kind` data to refresh); `weight`/`conv_id`/`session_id`
/// are untouched here since no row is lost — it is the SAME row, just
/// re-keyed. Either outcome (merge-then-delete or in-place move) is
/// "handled" from the caller's perspective — both count toward
/// `edges_updated`, matching the exact-match branches' convention.
fn repoint_or_dedupe_edge(
    shadow: &Arc<Storage>,
    old_src_id: &str,
    dst_id: &str,
    kind: &str,
    new_src_id: &str,
    new_callee_kind: Option<&str>,
    new_evidence: &str,
) -> Result<()> {
    shadow.with_connection(|conn| {
        let tx = conn.unchecked_transaction()?;
        let target_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM code_edges WHERE src_id = ?1 AND dst_id = ?2 AND kind = ?3)",
            rusqlite::params![new_src_id, dst_id, kind],
            |r| r.get(0),
        )?;
        if target_exists {
            // Finding 4: merge stale-row provenance into the surviving
            // target row BEFORE deleting the stale row — single transaction.
            let (target_rowid, target_weight, target_conv, target_session): (
                i64,
                f64,
                String,
                String,
            ) = tx.query_row(
                "SELECT rowid, weight, conv_id, session_id FROM code_edges
                 WHERE src_id = ?1 AND dst_id = ?2 AND kind = ?3",
                rusqlite::params![new_src_id, dst_id, kind],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            let (stale_rowid, stale_weight, stale_conv, stale_session): (
                i64,
                f64,
                String,
                String,
            ) = tx.query_row(
                "SELECT rowid, weight, conv_id, session_id FROM code_edges
                 WHERE src_id = ?1 AND dst_id = ?2 AND kind = ?3",
                rusqlite::params![old_src_id, dst_id, kind],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            let merged_weight = target_weight + stale_weight;
            let (merged_conv, merged_session) = if stale_rowid < target_rowid {
                (stale_conv, stale_session)
            } else {
                (target_conv, target_session)
            };
            match new_callee_kind {
                Some(ck) => {
                    tx.execute(
                        "UPDATE code_edges
                         SET weight = ?1, conv_id = ?2, session_id = ?3,
                             callee_kind = ?4, evidence = ?5
                         WHERE src_id = ?6 AND dst_id = ?7 AND kind = ?8",
                        rusqlite::params![
                            merged_weight,
                            merged_conv,
                            merged_session,
                            ck,
                            new_evidence,
                            new_src_id,
                            dst_id,
                            kind
                        ],
                    )?;
                }
                None => {
                    tx.execute(
                        "UPDATE code_edges
                         SET weight = ?1, conv_id = ?2, session_id = ?3, evidence = ?4
                         WHERE src_id = ?5 AND dst_id = ?6 AND kind = ?7",
                        rusqlite::params![
                            merged_weight,
                            merged_conv,
                            merged_session,
                            new_evidence,
                            new_src_id,
                            dst_id,
                            kind
                        ],
                    )?;
                }
            }
            tx.execute(
                "DELETE FROM code_edges WHERE src_id = ?1 AND dst_id = ?2 AND kind = ?3",
                rusqlite::params![old_src_id, dst_id, kind],
            )?;
            tx.commit()?;
            return Ok(());
        }
        match new_callee_kind {
            Some(ck) => {
                tx.execute(
                    "UPDATE code_edges SET src_id = ?1, callee_kind = ?2, evidence = ?3
                     WHERE src_id = ?4 AND dst_id = ?5 AND kind = ?6",
                    rusqlite::params![new_src_id, ck, new_evidence, old_src_id, dst_id, kind],
                )?;
            }
            None => {
                tx.execute(
                    "UPDATE code_edges SET src_id = ?1, evidence = ?2
                     WHERE src_id = ?3 AND dst_id = ?4 AND kind = ?5",
                    rusqlite::params![new_src_id, new_evidence, old_src_id, dst_id, kind],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    })
}

/// WCR Phase 8, TASK B: classify a pending edge `boundary = 'drifted'`,
/// `evidence = 'not_in_current_source'` — see `backfill_wcr_witnesses`'s doc
/// comment for the full rationale. Only ever called on an edge that did NOT
/// match the fresh extraction fragment AND that the caller has already
/// verified passes the Finding 1 truth-pass invariant (`can_drift` in the
/// caller): the edge's `src_file` re-extracted into a live, parseable
/// fragment (at least one def node) whose def names include the edge's own
/// calling symbol. Extraction failures/regressions never reach here — they
/// produce zero def nodes, which fails that check before this function is
/// ever called, so an extractor regression cannot masquerade as drift. The
/// caller's `if`/`else if` structure also makes matched and drifted
/// mutually exclusive per edge, so this never overwrites a
/// `callee_kind`/`evidence` update `backfill_wcr_witnesses` just made.
/// Leaves `resolved` at 0 and `dst_id` untouched — drifted edges never
/// bind, they are classified, exactly like the resolver's `stale` tier.
///
/// WCR truth pass, Codex round 8 (the empty-stamp invariant): `can_drift`
/// alone is NOT sufficient to drift an edge — it says nothing about whether
/// the edge's OWN `src_content_hash` stamp is present. An edge with an empty
/// stamp is a legacy row, written before content stamping existed (see
/// `historical_src_content_unchanged`'s doc comment): it carries no provable
/// link to any recorded file state, so its absence from a fresh extraction
/// proves nothing — the call may still exist, just unprovably so under the
/// new attribution. Round 7 already established this for the two branches
/// that check it explicitly (the re-point content-identity gate's
/// `else if edge_src_content_hash.is_empty()` arms), but every OTHER path
/// into this function — a missing legacy correspondence, an ambiguous bare
/// name with zero candidates, a callee entirely absent from the fresh
/// fragment — called this function on `can_drift` alone, with no
/// content-hash check at all, and so COULD drift an unstamped edge. Rather
/// than trust every present and future call site to repeat that check
/// individually (the round-7 fix required exactly two call sites to
/// remember it; round 8 found four more that hadn't), the guard now lives
/// HERE, structurally, as a required parameter: no path — none, ever — can
/// drift an edge without first supplying its own stamp, and an empty stamp
/// is unconditionally a silent no-op (pending, unexplained, empty boundary
/// — never a guess in either direction). A non-empty, MISMATCHED stamp is
/// unaffected: that IS positive drift evidence (the file provably changed
/// since the edge was recorded) and still drifts, exactly as before — this
/// guard only ever blocks the empty case, never a matched or mismatched one.
fn mark_drifted(
    shadow: &Arc<Storage>,
    src_id: &str,
    dst_id: &str,
    kind: &str,
    edge_src_content_hash: &str,
) -> Result<()> {
    if edge_src_content_hash.is_empty() {
        // No stamp, no evidence — leave pending/unexplained. See the doc
        // comment above for the full invariant.
        return Ok(());
    }
    shadow.with_connection(|conn| {
        conn.execute(
            "UPDATE code_edges SET boundary = 'drifted', evidence = 'not_in_current_source'
             WHERE src_id = ?1 AND dst_id = ?2 AND kind = ?3",
            rusqlite::params![src_id, dst_id, kind],
        )?;
        Ok(())
    })
}

/// TASK C (WCR Phase 6): precompute a disk-existence set for every distinct
/// `src_file` in `snapshot`, once, up front — the live gate's edge count can
/// run into the thousands, and per-edge `fs::metadata` calls inside the
/// resolver's hot loop would be wasteful when most edges share a handful of
/// files. Each distinct file is stat'd exactly once here (via
/// `canonical_repo_path`, matching `resolve_edges`'s "after
/// canonical_repo_path" contract), then the returned set is checked by
/// simple membership — see the closure built in `run_codegraph_live`.
fn build_file_exists_set(snapshot: &GraphSnapshot) -> HashSet<String> {
    let mut existing: HashSet<String> = HashSet::new();
    let mut checked: HashSet<&str> = HashSet::new();
    for edge in &snapshot.edges {
        let src_file = edge.src_file.as_str();
        if !checked.insert(src_file) {
            continue;
        }
        if canonical_repo_path(Path::new(src_file)).is_file() {
            existing.insert(src_file.to_string());
        }
    }
    existing
}

/// CSR_WCR_DUMP diagnostic (off by default): `from:<module>` evidence on
/// still-pending `imports` edges, captured just before `resolve_code_edges`
/// clears it. Read-only; never called unless `CSR_WCR_DUMP` is set.
fn wcr_dump_import_modules(shadow: &Arc<Storage>) -> Result<BTreeSet<(String, String)>> {
    shadow.with_connection(|conn| {
        let mut stmt = conn.prepare(
            "SELECT n.file, substr(e.dst_id, 6) FROM code_edges e
             JOIN code_nodes n ON n.id = e.src_id
             WHERE e.kind = 'imports' AND e.dst_id LIKE 'name:%' AND e.evidence LIKE 'from:%'",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<BTreeSet<_>, _>>()
            .map_err(Into::into)
    })
}

/// CSR_WCR_DUMP diagnostic (off by default): writes the post-resolve
/// composition of the WCR gate's `unexplained`/`ambiguous` buckets to `path`.
/// Read-only reconstruction of `resolver::resolve_edges`'s own
/// `ambiguous_remaining` criterion (>=2 candidate def files, in code_nodes or,
/// failing that, repo_defs) against edges left `resolved=0, boundary=''` —
/// the DB itself no longer distinguishes the two post-`clear_edge`. Never
/// mutates the shadow and never runs unless `CSR_WCR_DUMP` is set.
///
/// WCR truth pass, TASK 2: the query's `e.boundary = ''` filter already
/// EXCLUDES every X4 `local`-classified edge from this dump by construction
/// — `resolve_edges` sets `boundary = 'local'` on a match (see
/// `resolver::resolve_edges`'s X4 tier), so a locally-explained edge is no
/// longer `unexplained`/`ambiguous` at all, and correctly stops appearing
/// here without any bucket-name change. `had_local_binding` is added purely
/// as a regression trip-wire alongside the pre-existing `had_import_module`:
/// for every row that DOES still reach this dump (i.e. `resolve_edges`
/// decided NOT to classify it `local`), this independently re-derives
/// whether `local_bindings` had a matching witness anyway — it must always
/// be `false` in a correct dump (a `true` value would mean the resolver's
/// X4 tier and this dump's read-only reconstruction have diverged).
fn wcr_dump_write(
    shadow: &Arc<Storage>,
    import_modules: &BTreeSet<(String, String)>,
    path: &str,
) -> Result<()> {
    let rows: Vec<serde_json::Value> = shadow.with_connection(|conn| {
        let mut by_name: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        // 'const' (WCR Phase 7, TASK C): must mirror resolver::resolve_edges's
        // own `by_name` def-lookup kind set exactly, or this dump's
        // reconstructed ambiguous/unexplained buckets diverge from what the
        // resolver actually did.
        let mut defs = conn.prepare(
            "SELECT name, file FROM code_nodes WHERE kind IN ('function', 'type', 'method', 'const')",
        )?;
        for row in defs.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (name, file) = row?;
            by_name.entry(name).or_default().insert(file);
        }

        // WCR truth pass, TASK 2; scope-qualified by the X4 adversarial
        // review (Finding 1): the same local_bindings witness set the X4
        // resolver tier reads, for the `had_local_binding` trip-wire field.
        let mut local_bindings: BTreeSet<(String, String, String, String)> = BTreeSet::new();
        let mut lb_stmt = conn.prepare("SELECT project, file, scope, name FROM local_bindings")?;
        for row in lb_stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })? {
            local_bindings.insert(row?);
        }

        // `n.name` (Finding 1, X4 adversarial review): the edge's own
        // calling-symbol name — needed to reconstruct the SAME
        // scope-qualified match the resolver's X4 tier performs (caller
        // scope = `""` for a module-level edge, i.e. `src_name == src_file`,
        // else `src_name` itself).
        let mut stmt = conn.prepare(
            "SELECT e.kind, e.dst_id, e.callee_kind, e.src_file, n.project, n.name
             FROM code_edges e JOIN code_nodes n ON n.id = e.src_id
             WHERE e.resolved = 0 AND e.dst_id LIKE 'name:%' AND e.boundary = ''
             ORDER BY e.src_id, e.dst_id, e.kind",
        )?;
        let edges = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in edges {
            let (kind, dst_id, callee_kind, src_file, project, src_name) = row?;
            let name = dst_id.strip_prefix("name:").unwrap_or(&dst_id).to_string();
            let def_files = by_name.get(&name).map(BTreeSet::len).unwrap_or(0);
            let repo_files = if def_files == 0 {
                crate::storage::codegraph::lookup_repo_defs(conn, &project, &name)?
                    .into_iter()
                    .map(|(file, _kind)| file)
                    .collect::<BTreeSet<_>>()
                    .len()
            } else {
                0
            };
            let bucket = if def_files >= 2 || repo_files >= 2 {
                "ambiguous"
            } else {
                "unexplained"
            };
            let lang = Path::new(&src_file)
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("")
                .to_string();
            let had_import_module = import_modules.contains(&(src_file.clone(), name.clone()));
            let caller_scope = if src_name == src_file { String::new() } else { src_name };
            let had_local_binding = local_bindings.contains(&(
                project.clone(),
                src_file.clone(),
                caller_scope,
                name.clone(),
            ));
            out.push(serde_json::json!({
                "bucket": bucket,
                "kind": kind,
                "name": name,
                "callee_kind": callee_kind,
                "src_file": src_file,
                "project": project,
                "lang": lang,
                "had_import_module": had_import_module,
                "had_local_binding": had_local_binding,
                "def_file_count": def_files,
            }));
        }
        Ok(out)
    })?;
    fs::write(path, serde_json::to_string(&rows)?)
        .with_context(|| format!("CSR_WCR_DUMP: writing {path}"))
}

/// CSR_EVAL_DUMP_SHADOW diagnostic (off by default, zero effect otherwise):
/// copies the WCR shadow's `repo_defs`, `code_edges`, `code_nodes` tables —
/// post-backfill, post-resolve, so `code_edges.boundary` carries the final
/// fine-grained classification (bound/external/method/stale/local/
/// internal_module/unexplained/ambiguous/drifted) and `repo_defs` carries
/// the whole-repo scan — into a fresh on-disk SQLite file at `path` for
/// external (H3) inspection. Read-only on `shadow`: ATTACH/copy/DETACH,
/// never a write back into the shadow's own tables. Any pre-existing file
/// at `path` is removed first so re-runs don't collide with a stale dump.
/// Callers must treat failure as soft (log + continue) — a dump error must
/// never fail the eval gate.
fn dump_shadow_tables(shadow: &Arc<Storage>, path: &str) -> Result<()> {
    let _ = fs::remove_file(path);
    shadow
        .with_connection(|conn| {
            conn.execute("ATTACH DATABASE ?1 AS csr_eval_dump", [path])?;
            let copied = (|| -> Result<()> {
                for table in ["repo_defs", "code_edges", "code_nodes"] {
                    conn.execute_batch(&format!(
                        "CREATE TABLE csr_eval_dump.{table} AS SELECT * FROM main.{table}"
                    ))?;
                }
                Ok(())
            })();
            // DETACH regardless of copy outcome — never leave the shadow
            // connection holding an attached handle on a failed copy.
            conn.execute_batch("DETACH DATABASE csr_eval_dump")?;
            copied
        })
        .with_context(|| format!("CSR_EVAL_DUMP_SHADOW: dumping shadow tables to {path}"))
}

fn ranked_top_20(storage: &Arc<Storage>) -> Result<Vec<String>> {
    storage.with_connection(|conn| {
        let mut stmt = conn.prepare(
            "SELECT n.id FROM code_nodes n
             LEFT JOIN code_node_rank r ON r.node_id = n.id
             ORDER BY COALESCE(r.rank, 0.0) DESC, n.id
             LIMIT 20",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    })
}

fn rank_determinism_gate(storage: &Arc<Storage>) -> EvalResult {
    let started = Instant::now();
    let measured = (|| -> Result<(Vec<String>, Vec<String>)> {
        storage.compute_code_rank("")?;
        let first = ranked_top_20(storage)?;
        storage.compute_code_rank("")?;
        let second = ranked_top_20(storage)?;
        Ok((first, second))
    })();

    match measured {
        Ok((first, second)) => {
            let divergence = first
                .iter()
                .map(Some)
                .chain(std::iter::repeat(None))
                .zip(second.iter().map(Some).chain(std::iter::repeat(None)))
                .take(first.len().max(second.len()))
                .position(|(left, right)| left != right);
            match divergence {
                None => judge(
                    "Rank determinism",
                    started,
                    true,
                    format!("top-{} ordering byte-identical", first.len()),
                ),
                Some(index) => judge(
                    "Rank determinism",
                    started,
                    false,
                    format!(
                        "first divergence at position {}: first={:?}, second={:?}",
                        index + 1,
                        first.get(index),
                        second.get(index)
                    ),
                ),
            }
        }
        Err(error) => judge(
            "Rank determinism",
            started,
            false,
            format!("rank measurement error: {error:#}"),
        ),
    }
}

fn injection_budget_gate(
    storage: &Arc<Storage>,
    prompt: &str,
    files: &[String],
    project: &str,
) -> EvalResult {
    let started = Instant::now();
    let slices = build_graph_slices(storage, prompt, files, project);
    let rendered = slices.join("\n");
    let tokens = estimate_tokens(&rendered);
    judge(
        "Injection slice budget",
        started,
        tokens <= INJECTION_TOKEN_MAX,
        format!(
            "{tokens} tokens across {} slice(s), budget <= {INJECTION_TOKEN_MAX}",
            slices.len()
        ),
    )
}

fn extraction_latency_gate(
    source: &str,
    lang: SupportLang,
    file: &str,
    repo: &str,
    project: &str,
) -> EvalResult {
    let started = Instant::now();
    let fragment = extract_graph_fragment(
        source,
        lang,
        file,
        repo,
        project,
        "conv-latency",
        "session-latency",
    );
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    if elapsed_ms < EXTRACTION_LATENCY_MAX_MS {
        EvalResult::pass(
            "Single-file extraction latency",
            CATEGORY,
            elapsed_ms,
            format!(
                "{elapsed_ms:.2}ms for {file} ({} nodes, {} edges), threshold < {EXTRACTION_LATENCY_MAX_MS:.0}ms",
                fragment.nodes.len(),
                fragment.edges.len()
            ),
        )
    } else {
        EvalResult::fail(
            "Single-file extraction latency",
            CATEGORY,
            elapsed_ms,
            format!(
                "{elapsed_ms:.2}ms for {file} ({} nodes, {} edges), threshold < {EXTRACTION_LATENCY_MAX_MS:.0}ms",
                fragment.nodes.len(),
                fragment.edges.len()
            ),
        )
    }
}

fn percentile_95(samples: &mut [f64]) -> f64 {
    samples.sort_by(f64::total_cmp);
    let index = (samples.len() * 95).div_ceil(100).saturating_sub(1);
    samples[index]
}

fn query_latency_gate(storage: &Arc<Storage>, nodes: &[NodeRow]) -> EvalResult {
    let started = Instant::now();
    // 'const' (WCR Phase 7, TASK C): a legitimate def node now, same as
    // function/type/method — more query-latency sample coverage, no reason
    // to exclude it.
    let definitions: Vec<&NodeRow> = nodes
        .iter()
        .filter(|node| matches!(node.kind.as_str(), "function" | "type" | "method" | "const"))
        .collect();
    if definitions.is_empty() {
        return judge(
            "csr_code_graph query p95",
            started,
            false,
            "0 queries: no definition nodes available (requires >=20 per mode)".to_string(),
        );
    }

    let first = definitions[0];
    if let Err(error) = storage.code_query_callers(&first.name, &first.project, 20) {
        return judge(
            "csr_code_graph query p95",
            started,
            false,
            format!("callers warm-up failed: {error:#}"),
        );
    }
    if let Err(error) = storage.code_query_neighbors(&first.id, None, 20) {
        return judge(
            "csr_code_graph query p95",
            started,
            false,
            format!("neighbors warm-up failed: {error:#}"),
        );
    }

    // Wall-clock microbenchmarks can be interrupted by unrelated CI test
    // threads. Keep the locked 5ms threshold and a real 20-query p95, but take
    // the best complete trial so one scheduler burst cannot make the otherwise
    // deterministic fixture flaky. A consistently slow query fails every trial.
    let mut best: Option<(f64, f64)> = None;
    for trial in 0..QUERY_TRIALS {
        let mut caller_samples = Vec::with_capacity(QUERY_SAMPLES_PER_MODE);
        let mut neighbor_samples = Vec::with_capacity(QUERY_SAMPLES_PER_MODE);
        for index in 0..QUERY_SAMPLES_PER_MODE {
            let node = definitions[index % definitions.len()];

            let query_started = Instant::now();
            if let Err(error) = storage.code_query_callers(&node.name, &node.project, 20) {
                return judge(
                    "csr_code_graph query p95",
                    started,
                    false,
                    format!(
                        "callers trial {} query {} failed: {error:#}",
                        trial + 1,
                        index + 1
                    ),
                );
            }
            caller_samples.push(query_started.elapsed().as_secs_f64() * 1000.0);

            let query_started = Instant::now();
            if let Err(error) = storage.code_query_neighbors(&node.id, None, 20) {
                return judge(
                    "csr_code_graph query p95",
                    started,
                    false,
                    format!(
                        "neighbors trial {} query {} failed: {error:#}",
                        trial + 1,
                        index + 1
                    ),
                );
            }
            neighbor_samples.push(query_started.elapsed().as_secs_f64() * 1000.0);
        }

        let candidate = (
            percentile_95(&mut caller_samples),
            percentile_95(&mut neighbor_samples),
        );
        if best.is_none_or(|current| candidate.0.max(candidate.1) < current.0.max(current.1)) {
            best = Some(candidate);
        }
    }

    let (callers_p95, neighbors_p95) = best.expect("QUERY_TRIALS is non-zero");
    judge(
        "csr_code_graph query p95",
        started,
        callers_p95 < QUERY_P95_MAX_MS && neighbors_p95 < QUERY_P95_MAX_MS,
        format!(
            "callers p95={callers_p95:.3}ms, neighbors p95={neighbors_p95:.3}ms; best of {QUERY_TRIALS} trials, {QUERY_SAMPLES_PER_MODE} queries/mode/trial; threshold < {QUERY_P95_MAX_MS:.0}ms"
        ),
    )
}

fn round_trip_gate(storage: &Arc<Storage>, repo: &str, project: &str) -> EvalResult {
    const TARGET_FILE: &str = "__csr_eval__/targets.rs";
    const CALLER_FILE: &str = "__csr_eval__/caller.rs";
    const TARGET_SOURCE: &str = r#"
pub fn csr_eval_beta() -> usize { 2 }
pub fn csr_eval_gamma() -> usize { 3 }
"#;
    const CALLER_BEFORE: &str = "pub fn csr_eval_alpha() -> usize { csr_eval_beta() }\n";
    const CALLER_AFTER: &str = "pub fn csr_eval_alpha() -> usize { csr_eval_gamma() + 1 }\n";

    let started = Instant::now();
    let measured = (|| -> Result<(bool, bool, bool, bool)> {
        extract_and_store(
            storage,
            TARGET_SOURCE,
            TARGET_FILE,
            repo,
            project,
            "conv-roundtrip-target",
        )?;
        extract_and_store(
            storage,
            CALLER_BEFORE,
            CALLER_FILE,
            repo,
            project,
            "conv-roundtrip-before",
        )?;
        storage.resolve_code_edges(project)?;

        let before_node = storage
            .code_nodes_by_name("csr_eval_alpha", project, 1)?
            .into_iter()
            .next()
            .context("round-trip caller node missing before edit")?;
        let before_callers = storage.code_query_callers("csr_eval_beta", project, 20)?;
        let before_has_alpha = before_callers
            .iter()
            .any(|node| node.name == "csr_eval_alpha");

        extract_and_store(
            storage,
            CALLER_AFTER,
            CALLER_FILE,
            repo,
            project,
            "conv-roundtrip-after",
        )?;
        storage.resolve_code_edges(project)?;

        let after_node = storage
            .code_nodes_by_name("csr_eval_alpha", project, 1)?
            .into_iter()
            .next()
            .context("round-trip caller node missing after edit")?;
        let old_callers = storage.code_query_callers("csr_eval_beta", project, 20)?;
        let new_callers = storage.code_query_callers("csr_eval_gamma", project, 20)?;
        Ok((
            before_has_alpha,
            !old_callers.iter().any(|node| node.name == "csr_eval_alpha"),
            new_callers.iter().any(|node| node.name == "csr_eval_alpha"),
            before_node.body_hash != after_node.body_hash,
        ))
    })();

    match measured {
        Ok((before, removed, added, body_changed)) => judge(
            "Extract-resolve-query round-trip",
            started,
            before && removed && added && body_changed,
            format!(
                "before B<-A={before}; after B<-A removed={removed}; after C<-A={added}; body_hash changed={body_changed}"
            ),
        ),
        Err(error) => judge(
            "Extract-resolve-query round-trip",
            started,
            false,
            format!("round-trip error: {error:#}"),
        ),
    }
}

fn representative_live_source(
    snapshot: &GraphSnapshot,
) -> Result<(String, SupportLang, String, String, String, String)> {
    let mut files: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
    for node in &snapshot.nodes {
        if node.file.ends_with(".rs") {
            files
                .entry(node.file.as_str())
                .or_insert((&node.repo, &node.project));
        }
    }

    let mut readable = Vec::new();
    for (file, (repo, project)) in files {
        // Resolve through canonical_repo_path like build_file_exists_set does
        // (CodeRabbit PR #279) — a stored path that is not in resolved form
        // would otherwise be probed against the process cwd and skipped as
        // missing even though the file exists.
        let path = crate::extraction::repo_path::canonical_repo_path(Path::new(file));
        if !path.is_file() {
            continue;
        }
        let metadata = fs::metadata(&path)?;
        readable.push((metadata.len(), file, repo, project));
    }
    readable.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(right.1)));
    let (_, file, repo, project) = readable
        .get(readable.len() / 2)
        .copied()
        .context("no readable Rust file represented in the live code graph")?;
    let source = fs::read_to_string(crate::extraction::repo_path::canonical_repo_path(
        Path::new(file),
    ))
    .with_context(|| format!("read representative live source {file}"))?;
    let lang =
        lang_from_path_str(file).context("representative live file has unsupported language")?;
    let symbol = snapshot
        .nodes
        .iter()
        .find(|node| {
            node.file == file
                && node.name.len() >= 6
                // 'const' (WCR Phase 7, TASK C): kept for consistency with
                // the shared def-kind set — currently inert here since this
                // function only ever selects a `.rs` representative file
                // (see the `.ends_with(".rs")` filter above) and `const`
                // def nodes are TS/JS/TSX-only.
                && matches!(node.kind.as_str(), "function" | "type" | "method" | "const")
        })
        .map(|node| node.name.clone())
        .unwrap_or_else(|| "codegraph".to_string());
    Ok((
        source,
        lang,
        file.to_string(),
        repo.to_string(),
        project.to_string(),
        symbol,
    ))
}

fn is_worktree_path(file: &str) -> bool {
    file.contains("/.worktrees/") || file.contains("/.claude/worktrees/")
}

/// Six languages `code_nodes` claims AST coverage for (H8's scope, receipt
/// R4). Exactly the extension list from
/// `.plans/2026-07-31-codegraph-shipping-plan.md` WP2 Stage 3 — do not widen
/// or narrow without a new receipt.
const STRUCTURAL_COVERAGE_EXTENSIONS: &[&str] = &["rs", "py", "ts", "tsx", "js", "go"];

/// Path components that mark a file as vendored/generated/build output for
/// structural-coverage purposes — exactly H8's exclusion list. Distinct from
/// `repo_scan::HARD_SKIP_DIRS`: that list backs `repo_defs` (a different
/// table with a different extension/exclusion contract); the two must not be
/// conflated or unified without a receipt.
const STRUCTURAL_COVERAGE_SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    "vendor",
    "venv",
    ".venv",
    "__pycache__",
    "coverage",
];

/// True when `rel` (a path relative to some repo root) is in scope for the
/// H8 structural-coverage counter: one of the six supported extensions, no
/// path component under a vendor/build/generated directory, and not a
/// generated-file pattern (`*.min.js`, `*.d.ts`).
fn structural_coverage_path_is_supported(rel: &Path) -> bool {
    let ext_ok = rel
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            STRUCTURAL_COVERAGE_EXTENSIONS
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(ext))
        })
        .unwrap_or(false);
    if !ext_ok {
        return false;
    }
    let file_name = rel.file_name().and_then(|name| name.to_str()).unwrap_or("");
    if file_name.ends_with(".min.js") || file_name.ends_with(".d.ts") {
        return false;
    }
    rel.components().all(|component| match component {
        std::path::Component::Normal(part) => {
            let part = part.to_str().unwrap_or("");
            !STRUCTURAL_COVERAGE_SKIP_DIRS.contains(&part)
        }
        _ => true,
    })
}

/// Enumerate git-tracked plus non-ignored-untracked supported files under
/// `repo_root` (H8's method: `git ls-files` + `git ls-files --others
/// --exclude-standard`, never the `ignore`-crate walker `repo_scan` uses for
/// `repo_defs`). Returns relative-path strings (git's own output form, never
/// rewritten) filtered by `structural_coverage_path_is_supported`.
///
/// Fail-soft, by design (plan: "missing root → skip with count"): any git
/// failure — root no longer a repo, directory gone, `git` unavailable,
/// non-UTF8 output — yields `None`, never a guess and never a panic. Nothing
/// here is cached; every call re-shells to `git`.
fn structural_coverage_enumerate(repo_root: &str) -> Option<BTreeSet<String>> {
    let mut files = BTreeSet::new();
    for args in [
        vec!["ls-files"],
        vec!["ls-files", "--others", "--exclude-standard"],
    ] {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(&args)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if structural_coverage_path_is_supported(Path::new(line)) {
                files.insert(line.to_string());
            }
        }
    }
    Some(files)
}

/// H8 innovation (WP2 Stage 3, receipt R4): informational
/// `structural_file_coverage` counters appended to the health block. **Not a
/// gate** — `code_nodes` is edit-observed by design (v1 corpus contract);
/// promotion to a pass/fail threshold is deferred to the next corpus version
/// (plan: "pre-registered pair going forward = Internal binding + this").
///
/// Per distinct `repo_root` present in the live graph: AST-indexed supported
/// files (distinct `code_nodes.file` under that root matching the six
/// supported languages) ÷ enumerated supported files (`git ls-files` union,
/// same filter, vendor/build/generated exclusions applied). Overall ratio and
/// the top-5 repo_roots by enumerated count are reported; a repo_root whose
/// enumeration fails (deleted, not a repo any more, `git` unavailable) is
/// skipped and counted rather than guessed at.
///
/// Reads `code_nodes.repo_root` directly (`GraphSnapshot::nodes` doesn't
/// carry it — `snapshot()` deliberately queries only graph-structure columns,
/// see its comment — so this issues its own scoped `SELECT` against `storage`
/// rather than repurposing that struct).
fn structural_file_coverage_detail(storage: &Storage) -> String {
    let file_repo_pairs: Vec<(String, String)> = storage
        .with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT file, repo_root FROM code_nodes \
                 WHERE repo_root IS NOT NULL AND repo_root != ''",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(anyhow::Error::from)
        })
        .unwrap_or_default();

    let mut indexed_by_root: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (file, root) in &file_repo_pairs {
        let Some(rel) = file.strip_prefix(root.as_str()) else {
            continue;
        };
        let rel = rel.trim_start_matches('/');
        if structural_coverage_path_is_supported(Path::new(rel)) {
            indexed_by_root
                .entry(root.clone())
                .or_default()
                .insert(rel.to_string());
        }
    }

    let mut rows: Vec<(&str, usize, usize)> = Vec::new();
    let mut skipped_roots = 0usize;
    for (root, indexed_files) in &indexed_by_root {
        match structural_coverage_enumerate(root) {
            Some(enumerated) => rows.push((root.as_str(), indexed_files.len(), enumerated.len())),
            None => skipped_roots += 1,
        }
    }

    let total_indexed: usize = rows.iter().map(|(_, indexed, _)| *indexed).sum();
    let total_enumerated: usize = rows.iter().map(|(_, _, enumerated)| *enumerated).sum();
    let overall_pct = if total_enumerated == 0 {
        0.0
    } else {
        total_indexed as f64 * 100.0 / total_enumerated as f64
    };

    // Top 5 by enumerated count (largest repos first) — compact reporting,
    // not an exhaustive per-repo dump.
    rows.sort_by_key(|(_, _, enumerated)| std::cmp::Reverse(*enumerated));
    let top5: Vec<String> = rows
        .iter()
        .take(5)
        .map(|(root, indexed, enumerated)| {
            let pct = if *enumerated == 0 {
                0.0
            } else {
                *indexed as f64 * 100.0 / *enumerated as f64
            };
            let label = root.rsplit('/').next().unwrap_or(root);
            format!("{label}={indexed}/{enumerated} ({pct:.1}%)")
        })
        .collect();
    let top5_display = if top5.is_empty() {
        "none".to_string()
    } else {
        top5.join(", ")
    };

    format!(
        "\nstructural_file_coverage (informational, H8 innovation, not gated): \
         overall={total_indexed}/{total_enumerated} ({overall_pct:.1}%); \
         repo_roots measured={}, skipped(enumeration failed)={skipped_roots}; \
         top5 by enumerated count: {top5_display}",
        rows.len(),
    )
}

fn health_result(snapshot: &GraphSnapshot, storage: &Storage) -> EvalResult {
    let started = Instant::now();
    let files: BTreeSet<&str> = snapshot
        .nodes
        .iter()
        .map(|node| node.file.as_str())
        .collect();
    let projects: BTreeSet<&str> = snapshot
        .nodes
        .iter()
        .filter(|node| !node.project.is_empty())
        .map(|node| node.project.as_str())
        .collect();
    let calls = snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == "calls")
        .count();
    let imports = snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == "imports")
        .count();
    let defines = snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == "defines")
        .count();

    let node_by_id: HashMap<&str, &NodeRow> = snapshot
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    // 'const' (WCR Phase 7, TASK C): must mirror resolver::resolve_edges's
    // own `by_name` def-lookup kind set exactly — this map reconstructs the
    // resolver's own resolved/no_definition/ambiguous/pending_unique
    // buckets, and would misclassify const-backed edges as `no_definition`
    // otherwise.
    let mut defs: HashMap<(&str, &str), Vec<&NodeRow>> = HashMap::new();
    for node in snapshot
        .nodes
        .iter()
        .filter(|node| matches!(node.kind.as_str(), "function" | "type" | "method" | "const"))
    {
        defs.entry((node.project.as_str(), node.name.as_str()))
            .or_default()
            .push(node);
    }
    let mut resolved = 0usize;
    let mut no_definition = 0usize;
    let mut ambiguous = 0usize;
    let mut pending_unique = 0usize;
    for edge in snapshot
        .edges
        .iter()
        .filter(|edge| matches!(edge.kind.as_str(), "calls" | "imports"))
    {
        if edge.resolved != 0 {
            resolved += 1;
            continue;
        }
        let name = edge.dst_id.strip_prefix("name:").unwrap_or(&edge.dst_id);
        let source = node_by_id.get(edge.src_id.as_str()).copied();
        let candidates = source
            .and_then(|node| defs.get(&(node.project.as_str(), name)))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if candidates.is_empty() {
            no_definition += 1;
        } else if candidates.len() == 1
            || source.is_some_and(|source| {
                candidates
                    .iter()
                    .any(|candidate| candidate.file == source.file)
            })
        {
            // The live gate is read-only. Expose stale uniquely-resolvable rows
            // instead of mutating them or mislabeling them as ambiguous.
            pending_unique += 1;
        } else {
            ambiguous += 1;
        }
    }

    let unscoped = snapshot
        .nodes
        .iter()
        .filter(|node| node.project.is_empty())
        .count();
    let unscoped_pct = if snapshot.nodes.is_empty() {
        0.0
    } else {
        unscoped as f64 * 100.0 / snapshot.nodes.len() as f64
    };

    let worktree_files = files.iter().filter(|file| is_worktree_path(file)).count();
    // 'const' (WCR Phase 7, TASK C): same def-kind set as above — a
    // duplicated const symbol (worktree + regular copy) is exactly the same
    // hygiene signal a duplicated function/type is.
    let mut def_locations: HashMap<&str, (bool, bool)> = HashMap::new();
    for node in snapshot
        .nodes
        .iter()
        .filter(|node| matches!(node.kind.as_str(), "function" | "type" | "method" | "const"))
    {
        let flags = def_locations
            .entry(node.name.as_str())
            .or_insert((false, false));
        if is_worktree_path(&node.file) {
            flags.0 = true;
        } else {
            flags.1 = true;
        }
    }
    let duplicated_symbols = def_locations
        .values()
        .filter(|(worktree, regular)| *worktree && *regular)
        .count();

    let dirty = snapshot
        .file_states
        .iter()
        .filter(|(_, _, dirty)| *dirty)
        .count();
    let missing = snapshot
        .file_states
        .iter()
        .filter(|(_, file, _)| {
            // canonical_repo_path before the disk probe (CodeRabbit PR #279)
            // so the missing-on-disk counter cannot over-report for stored
            // paths that need worktree/symlink resolution.
            !crate::extraction::repo_path::canonical_repo_path(Path::new(file)).exists()
        })
        .count();

    let detail = format!(
        "inventory: nodes={}, edges={}, files={}, projects={}; edges calls/imports/defines={}/{}/{}\n\
         resolution: resolved={}, no_definition={}, ambiguous={}, pending_unique={}\n\
         scope: unscoped nodes={} ({:.1}%)\n\
         worktrees: distinct files={}, symbols defined in worktree+non-worktree={}\n\
         project coverage: code_nodes projects={}/code_evolution projects={}\n\
         staleness: dirty file-state rows={}, missing-on-disk file-state rows={}",
        snapshot.nodes.len(),
        snapshot.edges.len(),
        files.len(),
        projects.len(),
        calls,
        imports,
        defines,
        resolved,
        no_definition,
        ambiguous,
        pending_unique,
        unscoped,
        unscoped_pct,
        worktree_files,
        duplicated_symbols,
        projects.len(),
        snapshot.evolution_projects.len(),
        dirty,
        missing
    );
    let detail = format!("{detail}{}", structural_file_coverage_detail(storage));
    EvalResult::pass(
        "Health counters (informational)",
        "codegraph-health",
        started.elapsed().as_secs_f64() * 1000.0,
        detail,
    )
}

/// Deterministic, CI-safe code-graph release gate.
pub fn run_codegraph(_storage: &Arc<Storage>) -> Result<EvalReport> {
    let started = Instant::now();
    let fixture = seed_fixture()?;
    let fixture_snapshot = snapshot(&fixture)?;

    let mut results = vec![resolution_gate(&fixture_snapshot)];

    // WCR gates (Phase 4a): shadow built directly from the fixture snapshot,
    // deliberately WITHOUT repo scanning (empty repo_defs) and WITHOUT
    // code_evolution (empty table) — the fixture graph must be fully
    // bindable by B0/B1/B2 alone.
    let wcr_shadow = shadow_from_snapshot(&fixture_snapshot)?;
    match wcr_shadow.resolve_code_edges("") {
        Ok(stats) => {
            results.push(witness_closure_gate(&stats));
            results.push(internal_binding_gate(&stats));
        }
        Err(error) => {
            results.push(EvalResult::fail(
                "Witness closure",
                CATEGORY,
                0.0,
                format!("fixture WCR resolve error: {error:#}"),
            ));
            results.push(EvalResult::fail(
                "Internal binding",
                CATEGORY,
                0.0,
                format!("fixture WCR resolve error: {error:#}"),
            ));
        }
    }

    results.extend([
        rank_determinism_gate(&fixture),
        injection_budget_gate(
            &fixture,
            "Review alpha_fn in src/service.rs",
            &[SERVICE_FILE.to_string()],
            PROJECT,
        ),
        extraction_latency_gate(
            SERVICE_SOURCE,
            SupportLang::Rust,
            SERVICE_FILE,
            REPO,
            PROJECT,
        ),
        query_latency_gate(&fixture, &fixture_snapshot.nodes),
        round_trip_gate(&fixture, REPO, PROJECT),
        import_key_sanity_gate(&fixture_snapshot),
        project_attribution_gate(&fixture_snapshot),
        placeholder_leak_gate(),
    ]);

    Ok(EvalReport {
        results,
        total_ms: started.elapsed().as_secs_f64() * 1000.0,
    })
}

/// Read-only live code-graph release gate plus informational health counters.
pub fn run_codegraph_live(storage: &Arc<Storage>) -> Result<EvalReport> {
    let started = Instant::now();
    let live_snapshot = snapshot(storage)?;
    let shadow = shadow_from_snapshot(&live_snapshot)?;
    let representative = representative_live_source(&live_snapshot);

    let mut results = Vec::new();
    results.push(resolution_gate(&live_snapshot));

    // WCR gates (Phase 4a): a shadow independent of `shadow` above, extended
    // with code_evolution + repo_defs so B1/B2/B3/X1 have real evidence to
    // work with. Never mutates the live DB.
    match shadow_for_wcr(storage, &live_snapshot) {
        Ok((wcr_shadow, skip_note)) => {
            let backfill_note = match backfill_wcr_witnesses(&wcr_shadow) {
                Ok((files, edges)) => {
                    format!("; backfill: {files} files, {edges} edges updated")
                }
                Err(error) => format!("; backfill error: {error:#}"),
            };
            // CSR_WCR_DUMP diagnostic (off by default, zero effect otherwise):
            // capture from:<module> evidence before resolve clears it.
            let wcr_dump_path = env::var("CSR_WCR_DUMP").ok();
            let wcr_dump_modules = match &wcr_dump_path {
                Some(_) => wcr_dump_import_modules(&wcr_shadow).unwrap_or_default(),
                None => BTreeSet::new(),
            };
            // TASK C (WCR Phase 6): precompute the stale-file existence set
            // once against the live snapshot rather than re-stat'ing the
            // filesystem per pending edge inside the resolver's hot loop.
            let existing_files = build_file_exists_set(&live_snapshot);
            let file_exists: &dyn Fn(&str) -> bool = &|file: &str| existing_files.contains(file);
            match wcr_shadow.resolve_code_edges_with_fs_check("", file_exists) {
                Ok(stats) => {
                    let mut closure = witness_closure_gate(&stats);
                    let mut binding = internal_binding_gate(&stats);
                    closure.detail.push_str(&backfill_note);
                    binding.detail.push_str(&backfill_note);
                    if !skip_note.is_empty() {
                        closure.detail.push_str(&skip_note);
                        binding.detail.push_str(&skip_note);
                    }
                    if let Some(path) = &wcr_dump_path {
                        if let Err(error) = wcr_dump_write(&wcr_shadow, &wcr_dump_modules, path) {
                            closure
                                .detail
                                .push_str(&format!("; CSR_WCR_DUMP error: {error:#}"));
                        }
                    }
                    // CSR_EVAL_DUMP_SHADOW diagnostic (off by default, zero
                    // effect otherwise): H3 measurement needs the shadow's
                    // final post-backfill, post-resolve tables intact
                    // on-disk for external inspection. Fails soft — a dump
                    // error must never fail the eval gate.
                    if let Ok(dump_path) = env::var("CSR_EVAL_DUMP_SHADOW") {
                        if let Err(error) = dump_shadow_tables(&wcr_shadow, &dump_path) {
                            tracing::warn!(
                                error = %error,
                                path = %dump_path,
                                "CSR_EVAL_DUMP_SHADOW: failed to dump shadow tables"
                            );
                        }
                    }
                    results.push(closure);
                    results.push(binding);
                }
                Err(error) => {
                    results.push(EvalResult::fail(
                        "Witness closure",
                        CATEGORY,
                        0.0,
                        format!("live WCR resolve error: {error:#}"),
                    ));
                    results.push(EvalResult::fail(
                        "Internal binding",
                        CATEGORY,
                        0.0,
                        format!("live WCR resolve error: {error:#}"),
                    ));
                }
            }
        }
        Err(error) => {
            results.push(EvalResult::fail(
                "Witness closure",
                CATEGORY,
                0.0,
                format!("live WCR shadow build error: {error:#}"),
            ));
            results.push(EvalResult::fail(
                "Internal binding",
                CATEGORY,
                0.0,
                format!("live WCR shadow build error: {error:#}"),
            ));
        }
    }

    results.push(rank_determinism_gate(&shadow));
    match representative {
        Ok((source, lang, file, repo, project, symbol)) => {
            results.push(injection_budget_gate(
                storage,
                &format!("Review {symbol} in {file}"),
                std::slice::from_ref(&file),
                &project,
            ));
            results.push(extraction_latency_gate(
                &source, lang, &file, &repo, &project,
            ));
        }
        Err(error) => {
            results.push(EvalResult::fail(
                "Injection slice budget",
                CATEGORY,
                0.0,
                format!("live representative unavailable: {error:#}"),
            ));
            results.push(EvalResult::fail(
                "Single-file extraction latency",
                CATEGORY,
                0.0,
                format!("live representative unavailable: {error:#}"),
            ));
        }
    }
    results.push(query_latency_gate(storage, &live_snapshot.nodes));
    results.push(round_trip_gate(
        &shadow,
        "__csr_codegraph_eval_repo__",
        "__csr_codegraph_eval_project__",
    ));
    results.push(import_key_sanity_gate(&live_snapshot));
    results.push(project_attribution_gate(&live_snapshot));
    results.push(placeholder_leak_gate());
    results.push(health_result(&live_snapshot, storage));

    Ok(EvalReport {
        results,
        total_ms: started.elapsed().as_secs_f64() * 1000.0,
    })
}

#[cfg(test)]
#[path = "codegraph_tests.rs"]
mod tests;
