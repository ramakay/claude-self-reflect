//! Witness-Closure Resolution (WCR) — tiered, deterministic edge resolver
//! (v9.5 Phase 3, "truth pass").
//!
//! Every `name:<symbol>` placeholder edge (`calls` or `imports`, `resolved = 0`)
//! is driven through **bind tiers** (mint `resolved = 1`, and for real
//! `code_nodes` targets repoint `dst_id`) and, for whatever remains,
//! **classify tiers** (leave `resolved = 0` but attach `boundary` + `evidence`
//! — proof the edge crosses a real boundary, not an assumption). Anything
//! left over is `unexplained`. Ambiguity is FIRST-CLASS throughout: multiple
//! candidates with no disambiguating evidence are never guessed. Deterministic
//! (every map/set iterates in sorted order; running the pass twice on an
//! unchanged DB produces identical stats).
//!
//! ## Bind tiers (never guess)
//!
//! - **B0 `same_file`** — a def of `N` exists in the caller's own file
//!   (pre-existing behavior, unchanged).
//! - **B1 `module_bind`** (WCR Phase 8, TASK A) — the WCR spec's "prefer a
//!   def whose file matches the import's module path when derivable" clause,
//!   no longer inert now that Phase 5 TASK 1 captures `from:<module>`
//!   evidence on `imports` edges (and, for a `calls` edge, its same-file
//!   sibling `imports` edge for the same name — see `module_for_pending`). A
//!   relative (`./x`, `../x`) or alias (`~/x`, `@/x`) module specifier names
//!   ONE SPECIFIC file, not "some file somewhere in the project": when it
//!   resolves to a real on-disk file (`module_bind_tier`, reusing
//!   `internal_module_tier`'s own resolution — see `resolve_module_file`)
//!   and that exact file has a def of `N` (`code_nodes`, or failing that
//!   `repo_defs`), this is precise, unambiguous, textual evidence —
//!   regardless of how many OTHER files elsewhere also define a same-named
//!   symbol (the classic per-component `styles`/`COLORS` const collision:
//!   many `X.styles.ts` files each export `styles`, but a given component's
//!   own `import { styles } from './X.styles'` names exactly one of them).
//!   Runs immediately after B0, BEFORE any ambiguity is declared. `evidence
//!   = "module_bind:<basename>"`. Binds to the real `code_nodes` id when one
//!   exists in the resolved file; when the resolved file only has a
//!   `repo_defs` entry, `dst_id` stays the placeholder (same convention as
//!   the repo_scan-only bind below).
//! - **B1b `import_bound`** — `N` is imported in the caller's file (see
//!   `imports_by_file` below) AND either the graph (`code_nodes`) has exactly
//!   one candidate file for `N`, or it has several but `repo_defs` (the
//!   whole-repo scan) narrows to exactly one file that also has a
//!   `code_nodes` def — bind to that def. Weaker than B1: it only proves "N
//!   was imported here", not which specific file it came from.
//! - **repo_scan-only bind** — `N` has zero `code_nodes` defs but exactly one
//!   `repo_defs` file (import edge not required — this is the general form of
//!   the WCR spec's "Also:" clause). `repo_defs` is a whole-repo scan, not
//!   conversation-attributed provenance, so this never mints a `code_nodes`
//!   id: `dst_id` stays the placeholder, only `resolved` flips to 1 and
//!   `evidence` records the binding (`repo_scan:<file basename>`).
//! - **B2 `unique_def`** — `N` has exactly one `code_nodes` def project-wide
//!   and no import edge backs it (weaker evidence than B1 — no textual proof,
//!   just uniqueness).
//! - **B3 `coedit:<weight>`** — `N` has multiple `code_nodes` defs and no
//!   import/repo_defs disambiguation. Score each candidate file by co-edit
//!   weight: the count of distinct `code_evolution.session_id`s that touched
//!   *both* the caller's file and the candidate file. Bind only when the top
//!   weight is >= 2 and >= 2x the runner-up's (runner-up 0 counts as 0) —
//!   otherwise leave it ambiguous.
//!
//! ## Classify tiers (resolved stays 0 — the edge is explained, not bound)
//!
//! - **X0 `external` (builtin)** — `N` has no def anywhere (code_nodes or
//!   repo_defs) AND matches a per-language curated list of prelude/builtin/
//!   global names (`manifest::classify_builtin`) — e.g. Rust's `Ok`/`Err`/
//!   `println!`, JS's `fetch`/`console`, Python's `print`/`ValueError`, Go's
//!   `make`/`len`. These names are never `use`/`import`ed (they are always in
//!   scope), so X1 below — which requires import evidence — can never reach
//!   them. Runs BEFORE X1. `evidence = "builtin:<lang>"`.
//! - **X1 `external`** — `N` has no def anywhere (code_nodes or repo_defs)
//!   but is an import symbol in the caller's file. Module-aware when the
//!   backing `imports` edge carries `from:<module>` evidence (captured at
//!   extraction time — see `extraction::codegraph::import_symbols`): a
//!   relative module (`./x`, `../x`) is an internal candidate, never
//!   classified external; a bare module classifies external iff its first
//!   path segment matches a manifest dependency or a stdlib/builtin
//!   namespace (`manifest::ExternalNs::classify_module`), evidence
//!   `import:<module>`. Falls back to the degraded bound-symbol-name match
//!   (`manifest::ExternalNs::classify`, evidence `import:<matched>`) only when
//!   no `from:` module data is available (older edges predating this capture,
//!   or synthetic/test edges).
//! - **X1b `qualifier`** (WCR Phase 6, TASK B) — a `calls` edge carrying
//!   `via:<qualifier>` evidence (captured at extraction time — see
//!   `extraction::codegraph::call_qualifier`), classified purely from the
//!   qualifier's ROOT segment, independent of whether the qualifier itself
//!   was textually imported (a fully-qualified path like
//!   `std::fs::read_to_string` never needs a `use`). Runs after X0 and
//!   before the degraded X1/X2 below — see `qualifier_tier`. `evidence =
//!   "import:<qualifier>"`.
//! - **X2 `method`** — a `calls` edge with `callee_kind = 'method'` and no def
//!   anywhere: an unbound receiver call (`x.push()`), not a free-function
//!   reference. Legacy edges with `callee_kind = ''` are left alone — a later
//!   re-extraction backfills the field.
//! - **stale** (WCR Phase 6, TASK C) — the LAST resort: only reached once
//!   every bind tier and every classify tier above (X0/X1b/X1/X2) has failed
//!   to explain the edge. If the edge's `src_file` no longer exists on disk
//!   (per the caller-injected `file_exists` check — see `resolve_edges`),
//!   the call/import site itself is provenance history, not something we
//!   failed to explain: file absence is disk-verifiable evidence, so this is
//!   an evidenced classification (`boundary = 'stale'`, `evidence =
//!   'file_missing'`), not silence. Counted alongside `bound`/`external`/
//!   `method` in `closure_rate`; excluded from `internal_binding_rate`'s
//!   denominator, same as `external`/`method`.
//! - **drifted** (WCR Phase 8, TASK B) — set directly by
//!   `eval::codegraph::backfill_wcr_witnesses` in the shadow BEFORE this
//!   resolve pass runs, never by a tier here: the edge's `src_file` re-
//!   extracted cleanly (unlike `stale`, the file exists and reads fine), but
//!   the fresh extraction fragment no longer contains this exact (src name,
//!   kind, bare target name) triple — the call/import site itself has
//!   changed since the edge was recorded. Round-trip-consistent: the live
//!   pipeline's `replace_file_edges` would simply delete such an edge on the
//!   next real file touch; the read-only gate's shadow can't delete, only
//!   classify. `resolve_edges` recognizes a pre-set `boundary = 'drifted'`
//!   as already-classified — every tier (including B0) is skipped for it,
//!   boundary/evidence are left untouched, and it is counted in
//!   `ResolveStats::drifted`. Same standing as `stale` in both rates:
//!   counted in `closure_rate`'s numerator, excluded from
//!   `internal_binding_rate`'s denominator.
//! - **X4 `local`** (WCR truth pass, TASK 2) — the LAST classify tier,
//!   reached only once X0/X1b/X1c/X1/X2/`internal_module_tier` have all
//!   failed to explain the edge AND `N` has zero defs anywhere (code_nodes
//!   or repo_defs — the SAME never-guess precondition every classify tier
//!   above already requires, so this can never preempt a bind tier). `N` IS,
//!   however, a witnessed LOCAL binding — a function/closure parameter or a
//!   local (non-top-level) variable/destructuring target — in the edge's own
//!   (project, src_file), per the `local_bindings` shadow table (see
//!   `extraction::codegraph::collect_local_bindings`, populated by
//!   `eval::codegraph::backfill_wcr_witnesses` from the same re-extraction
//!   pass that backs every other WCR witness tier). `evidence =
//!   "local_scope:<name>"`. Same standing as `stale`/`internal_module`/
//!   `drifted` in both rates: counted in `closure_rate`'s numerator,
//!   excluded from `internal_binding_rate`'s denominator — a local-scope
//!   witness proves the edge isn't a mystery, not that a repo-wide symbol
//!   was found.
//!
//! Everything else is `unexplained`: no def anywhere, no builtin/qualifier
//! match, no import evidence, not a method call, not a witnessed local
//! binding, and the file still exists. `ambiguous_remaining` is the subset
//! of `unexplained` where multiple candidates existed (code_nodes and/or
//! repo_defs) but could not be disambiguated — as opposed to genuinely no
//! information at all. A would-be `unexplained`/`ambiguous` edge whose file
//! is missing is `stale` instead, not counted in either.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::Result;
use rusqlite::{params, Connection};

use super::manifest;
use super::repo_scan;

/// Resolution outcome surfaced to eval / diagnostics.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolveStats {
    /// Placeholder edges considered.
    pub total: usize,
    /// Placeholder edges bound (`resolved` flipped to 1). Pre-WCR field name
    /// — kept because `import::backfill` reads it directly and is out of
    /// scope for this change.
    pub resolved: usize,
    /// Same value as `resolved` — the WCR-spec name for the same count.
    pub bound: usize,
    /// Edges classified `boundary = 'external'` (X1).
    pub external: usize,
    /// Edges classified `boundary = 'method'` (X2).
    pub method: usize,
    /// Edges classified `boundary = 'stale'` (WCR Phase 6, TASK C): the
    /// edge's `src_file` no longer exists on disk — provenance history, not
    /// an unresolved mystery. See the module doc comment's "stale" tier.
    pub stale: usize,
    /// Edges classified `boundary = 'internal_module'` (WCR Phase 7, TASK
    /// E): a relative (`./x`, `../x`) or alias (`~/x`, `@/x`) `imports` edge
    /// whose module specifier resolves to a real file on disk (see
    /// `internal_module_tier`), but no `code_nodes`/`repo_defs` def for the
    /// bound symbol was found there — the FILE is disk-verified, the
    /// specific SYMBOL binding is not. Counted in `closure_rate`'s numerator
    /// (the edge IS explained — a real project file backs it) but EXCLUDED
    /// from `internal_binding_rate`'s denominator, same treatment as
    /// `external`/`method`/`stale`: it would be misleading to count a
    /// module-path hit as evidence either for or against symbol-level
    /// binding quality.
    pub internal_module: usize,
    /// Edges classified `boundary = 'drifted'` (WCR Phase 8, TASK B): the
    /// edge's `src_file` re-extracted cleanly but the fresh extraction no
    /// longer contains this exact (src name, kind, bare target name) triple
    /// — set by `eval::codegraph::backfill_wcr_witnesses`, never by a tier
    /// in `resolve_edges` itself (see the module doc comment's "drifted"
    /// tier). Counted in `closure_rate`'s numerator, EXCLUDED from
    /// `internal_binding_rate`'s denominator, same treatment as
    /// `external`/`method`/`stale`/`internal_module`.
    pub drifted: usize,
    /// Edges classified `boundary = 'local'` (WCR truth pass, TASK 2, X4
    /// tier): `N` has NO def anywhere (code_nodes or repo_defs — same
    /// zero-defs precondition as X0/X1/X2 above, verified BEFORE this tier
    /// ever runs so it can never preempt a bind tier) but `N` IS a witnessed
    /// LOCAL binding — a function/closure parameter or a local (non-top-level)
    /// variable/destructuring target — in the edge's own (project, src_file),
    /// per `local_bindings` (see `extraction::codegraph::collect_local_bindings`,
    /// populated by `eval::codegraph::backfill_wcr_witnesses` from the SAME
    /// re-extraction pass that backs the other WCR witness tiers). This is
    /// the honest explanation for the WCR truth-pass residual's dominant
    /// shape: JS/TS component-scoped `const playTrack = ...` and closure
    /// params like `reject` — real code, correctly never bound to any
    /// `code_nodes`/`repo_defs` entry because a local binding is by
    /// definition not a repo-wide symbol. Runs LAST among classify tiers —
    /// after X0/X1b/X1c/X1/X2/`internal_module_tier`, before `stale` — see
    /// `classify_only`. `evidence = "local_scope:<name>"`. Counted in
    /// `closure_rate`'s numerator; EXCLUDED from `internal_binding_rate`'s
    /// denominator, same treatment as `external`/`method`/`stale`/
    /// `internal_module`/`drifted` — a local-scope witness is not evidence
    /// either for or against REPO-WIDE symbol-binding quality. Silent (never
    /// fires) whenever `local_bindings` has no rows for the edge's
    /// (project, src_file) — e.g. every resolve pass outside the WCR live
    /// gate, since only `backfill_wcr_witnesses` ever populates the table.
    pub local: usize,
    /// `total - bound - external - method - stale - internal_module -
    /// drifted - local`.
    pub unexplained: usize,
    /// Subset of `unexplained`: a def existed (code_nodes and/or repo_defs)
    /// somewhere but multiple candidates could not be disambiguated.
    pub ambiguous_remaining: usize,
    /// `(bound + external + method + stale + internal_module + drifted +
    /// local) / total`. `1.0` when `total == 0`.
    pub closure_rate: f64,
    /// `bound / (total - external - method - stale - internal_module -
    /// drifted - local)`. `1.0` when the denominator is 0.
    pub internal_binding_rate: f64,
}

/// One unresolved `name:<symbol>` edge, joined with its caller's file/project.
struct Pending {
    src_id: String,
    dst_id: String,
    kind: String,
    src_file: String,
    project: String,
    callee_kind: String,
    /// The edge's own `evidence` column as currently stored. For a still-
    /// pending `imports` edge this is `from:<module>` when extraction
    /// captured one (Phase 5 TASK 1), else `''`. For a still-pending `calls`
    /// edge this is `via:<qualifier>` when extraction captured a path/
    /// attribute-call qualifier (Phase 6 TASK A), else `''`.
    evidence: String,
    /// The edge's own `boundary` column as currently stored. `resolve_edges`
    /// treats one value specially: `"drifted"` (WCR Phase 8, TASK B) — set
    /// externally by `eval::codegraph::backfill_wcr_witnesses`, never by a
    /// tier here — is recognized at the very top of the per-edge loop as
    /// already-classified, and every tier is skipped for it.
    boundary: String,
    name: String,
    /// Second X4 adversarial review, Finding 4; upgraded to ALL of an edge's
    /// distinct chains by the Codex round 4 adversarial review, Finding 1:
    /// every DISTINCT call/import-site scope CHAIN recorded in
    /// `edge_scope_chains` for this edge's `(src_id, dst_id, kind)` —
    /// fetched as a separate `(src_id, dst_id, kind) -> Vec<chain>` map in
    /// `resolve_edges`'s Pass 1e (never a JOIN against the main Pending
    /// query: a JOIN would fan out into one Pending row PER CHAIN for a
    /// multi-occurrence edge, corrupting the one-Pending-per-DB-row
    /// invariant every stat bucket in this module relies on). When
    /// `eval::codegraph::backfill_wcr_witnesses` populated at least one row
    /// (only the WCR live gate ever does — every other resolve pass has no
    /// rows in `edge_scope_chains` at all, exactly the same
    /// silence-by-construction convention as `local_bindings`), this is
    /// that full set. Otherwise, the pre-chain fallback (computed in
    /// `resolve_edges`'s Pass 2, from `code_nodes.name` of `src_id` — the
    /// edge's own calling def, or the synthetic module node's own name,
    /// which IS the file path, for a module-level edge) as the SOLE
    /// element: `""` when that calling "def" IS the file itself, else the
    /// calling def's bare name — i.e. "act as if this call/import sits
    /// directly in its calling def, no closure nesting known", the X4
    /// adversarial review Finding 1 behavior this replaces. Always
    /// non-empty (the fallback guarantees at least one element) —
    /// `classify_only`'s X4 block relies on this: it requires a witness's
    /// binding chain to be a prefix of EVERY element (conservative
    /// universal quantification — Finding 1, Codex round 4: one call site's
    /// chain is no longer allowed to stand in for a whole aggregated edge's
    /// worth of occurrences), and `[].iter().all(..)` is vacuously `true`,
    /// so an accidentally-empty vec here would wrongly witness everything.
    /// Never `src_name` directly — see `chain_prefix_matches`'s doc comment.
    call_scope_chains: Vec<String>,
}

/// Resolve all `name:<symbol>` placeholder edges within `project` (empty =
/// all). `file_exists` is the WCR Phase 6 TASK C disk-existence check for the
/// `stale` tier: given a `Pending::src_file` as stored (raw, not
/// canonicalized), it must return whether that file currently exists on
/// disk. The closure owns its own canonicalization (via
/// `extraction::repo_path::canonical_repo_path`) — callers with many edges
/// to check (e.g. the live eval gate) can precompute a canonicalized
/// existence set once and close over it; callers checking one project at a
/// time (hooks, backfill) can just stat the filesystem directly per call.
/// See `Storage::resolve_code_edges` for the direct-stat default.
pub fn resolve_edges(
    conn: &Connection,
    project: &str,
    file_exists: &dyn Fn(&str) -> bool,
) -> Result<ResolveStats> {
    // Pass 1: name -> sorted [(file, id)] for definition nodes
    // (function/type/method/const — 'const' added WCR Phase 7, TASK C: a
    // top-level TS/JS/TSX const/let/var is a real, bindable def node now,
    // same as a function/type; omitting it here would leave every
    // `COLORS`/`styles`/`AnalyticsEvents`-style import edge unable to bind
    // even though `extraction::codegraph::extract_inner` now emits a def
    // node for it).
    let mut by_name: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, file, name FROM code_nodes
             WHERE kind IN ('function', 'type', 'method', 'const') AND (?1 = '' OR project = ?1)
             ORDER BY name, file, id",
        )?;
        let rows = stmt.query_map(params![project], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for r in rows {
            let (id, file, name) = r?;
            by_name.entry(name).or_default().push((file, id));
        }
    }

    // Pass 1b: per-file import symbol sets. Covers both still-placeholder
    // imports (`dst_id = 'name:<sym>'`) and imports a prior resolve pass
    // already bound (`dst_id` = a real node id — symbol comes from that
    // node's name). This is the textual "N was imported here" evidence the
    // B1 and X1 tiers key off.
    let mut imports_by_file: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT n.file,
                    CASE WHEN e.dst_id LIKE 'name:%' THEN substr(e.dst_id, 6) ELSE n2.name END
             FROM code_edges e
             JOIN code_nodes n ON n.id = e.src_id
             LEFT JOIN code_nodes n2 ON n2.id = e.dst_id
             WHERE e.kind = 'imports' AND (?1 = '' OR n.project = ?1)",
        )?;
        let rows = stmt.query_map(params![project], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        for r in rows {
            let (file, sym) = r?;
            if let Some(sym) = sym {
                if !sym.is_empty() {
                    imports_by_file.entry(file).or_default().insert(sym);
                }
            }
        }
    }

    // Pass 1c: per-file, per-symbol module map — `from:<module>` evidence
    // captured at extraction time (Phase 5 TASK 1) on still-pending imports
    // edges. Feeds the X1 module-aware tier for `calls` edges (which never
    // carry their own `from:` evidence — only the sibling `imports` edge in
    // the same file does). Only currently-pending edges are in scope: once an
    // imports edge binds or gets classified, its evidence is overwritten with
    // the tier's own evidence string, so any module data it carried is only
    // reliable for THIS pass, read before the main loop mutates anything.
    let mut imports_module_by_file: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT n.file, substr(e.dst_id, 6), e.evidence
             FROM code_edges e JOIN code_nodes n ON n.id = e.src_id
             WHERE e.kind = 'imports' AND e.dst_id LIKE 'name:%' AND e.evidence LIKE 'from:%'
               AND (?1 = '' OR n.project = ?1)",
        )?;
        let rows = stmt.query_map(params![project], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for r in rows {
            let (file, sym, evidence) = r?;
            if let Some(module) = evidence.strip_prefix("from:") {
                if !sym.is_empty() && !module.is_empty() {
                    imports_module_by_file
                        .entry(file)
                        .or_default()
                        .insert(sym, module.to_string());
                }
            }
        }
    }

    // Pass 1d (WCR truth pass, TASK 2); scope-qualified by the X4
    // adversarial review (Finding 1), CHAIN-qualified by the second X4
    // adversarial review (Finding 4): per (project, file) set of
    // `(scope, name)` local-binding pairs — the X4 tier's witness table,
    // populated only by `eval::codegraph::backfill_wcr_witnesses` (see
    // `extraction::codegraph::collect_local_bindings`). Empty on every DB
    // this table hasn't been backfilled into — the X4 tier is then silent
    // by construction (an empty/missing `BTreeMap` entry never matches),
    // not an error. `scope` is the nearest enclosing named def's full scope
    // CHAIN (or `""` for a module-level binding) — WITHOUT it, a flat
    // name-only set per (project, file) let a parameter named `handler` in
    // one function (or one anonymous closure) wrongly classify an unrelated
    // sibling scope's own `handler()` call as local; see `classify_only`'s
    // X4 block and `chain_prefix_matches` for the prefix check this feeds.
    let mut local_bindings: BTreeMap<(String, String), BTreeSet<(String, String)>> =
        BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT project, file, scope, name FROM local_bindings WHERE (?1 = '' OR project = ?1)",
        )?;
        let rows = stmt.query_map(params![project], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        for r in rows {
            let (proj, file, scope, name) = r?;
            local_bindings
                .entry((proj, file))
                .or_default()
                .insert((scope, name));
        }
    }

    // Pass 1e (Codex round 4 adversarial review, Finding 1): every fresh
    // call/import site's own scope CHAIN for a pending edge's own
    // `(src_id, dst_id, kind)` key — potentially MULTIPLE rows per key now
    // (`edge_scope_chains`' primary key grew `chain` — see its migration
    // comment), one per distinct physical occurrence
    // `extraction::codegraph::extract_inner` recorded for the aggregated
    // edge. Fetched as a SEPARATE map, never via a JOIN in the Pass 2 query
    // below — a LEFT JOIN would fan out into one row PER CHAIN for a
    // multi-occurrence edge, corrupting the one-Pending-per-DB-row
    // invariant every stat bucket in this module relies on (double-counting
    // `resolved` and every other counter). Grouped in ascending `chain`
    // order (the SQL `ORDER BY` — deterministic regardless of INSERT
    // order, which is exactly what the Finding 1 "test both orders"
    // regression needs).
    let mut edge_scope_chains: BTreeMap<(String, String, String), Vec<String>> = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT src_id, dst_id, kind, chain FROM edge_scope_chains
             WHERE (?1 = '' OR project = ?1)
             ORDER BY src_id, dst_id, kind, chain",
        )?;
        let rows = stmt.query_map(params![project], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        for r in rows {
            let (src_id, dst_id, kind, chain) = r?;
            edge_scope_chains
                .entry((src_id, dst_id, kind))
                .or_default()
                .push(chain);
        }
    }

    // Pass 2: collect placeholder edges (calls + imports), deterministically.
    let mut pending: Vec<Pending> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT e.src_id, e.dst_id, e.kind, n.file, n.project, e.callee_kind, e.evidence, e.boundary, n.name
             FROM code_edges e JOIN code_nodes n ON n.id = e.src_id
             WHERE e.resolved = 0 AND e.dst_id LIKE 'name:%'
               AND (?1 = '' OR n.project = ?1)
             ORDER BY e.src_id, e.dst_id, e.kind",
        )?;
        let rows = stmt.query_map(params![project], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            ))
        })?;
        for r in rows {
            let (
                src_id,
                dst_id,
                kind,
                src_file,
                edge_project,
                callee_kind,
                evidence,
                boundary,
                src_name,
            ) = r?;
            let name = dst_id.strip_prefix("name:").unwrap_or(&dst_id).to_string();
            // Second X4 adversarial review, Finding 4; upgraded to a Vec by
            // Finding 1 (Codex round 4): when `edge_scope_chains` has at
            // least one row for this edge, use ALL of them. Otherwise, the
            // fallback mirrors the pre-chain `caller_scope` computation that
            // used to live in `classify_only` itself: module-level (this
            // edge's own calling "def" IS the synthetic module node, whose
            // `code_nodes.name` is the file path) maps to `""`; otherwise
            // the bare calling-def name, unchanged from before chains
            // existed — see `Pending::call_scope_chains`'s doc comment. The
            // fallback vec always has exactly one element — `call_scope_chains`
            // must never be empty (see that field's own doc comment).
            let call_scope_chains =
                match edge_scope_chains.get(&(src_id.clone(), dst_id.clone(), kind.clone())) {
                    Some(chains) if !chains.is_empty() => chains.clone(),
                    _ => vec![if src_name == src_file {
                        String::new()
                    } else {
                        src_name.clone()
                    }],
                };
            pending.push(Pending {
                src_id,
                dst_id,
                kind,
                src_file,
                project: edge_project,
                callee_kind,
                evidence,
                boundary,
                name,
                call_scope_chains,
            });
        }
    }

    let total = pending.len();
    let mut resolved = 0usize;
    let mut external = 0usize;
    let mut method = 0usize;
    let mut stale = 0usize;
    let mut internal_module = 0usize;
    let mut drifted = 0usize;
    let mut local = 0usize;
    let mut ambiguous_remaining = 0usize;
    // TASK E (WCR Phase 6): memoizes `repo_scan::project_roots` per project
    // across the whole pass — a DB query, and many pending edges typically
    // share a project.
    let mut memo = Memo::default();

    for p in &pending {
        // --- drifted (WCR Phase 8, TASK B): already classified externally,
        // before this pass ever ran — skip every tier, including B0. See
        // the module doc comment's "drifted" tier and `Pending::boundary`.
        if p.boundary == "drifted" {
            drifted += 1;
            continue;
        }

        // Finding 2 (WCR truth pass): receiver-aware method-call gating —
        // see `method_bind_gate`'s doc comment. Computed before B0 since it
        // decides whether B0 itself is even reachable.
        let gate = method_bind_gate(p);

        // --- B0: same-file def, first (sorted) match wins. ---------------
        let defs = by_name.get(&p.name);
        if gate != MethodGate::NoBind {
            let same_file = defs.and_then(|d| d.iter().find(|(file, _)| file == &p.src_file));
            if let Some((_, id)) = same_file {
                bind(conn, p, id, "same_file")?;
                resolved += 1;
                continue;
            }
        }

        if gate != MethodGate::Unrestricted {
            // SelfOnly's only allowed bind tier (B0) just failed above, or
            // this is NoBind (no bind tier allowed at all): name identity is
            // not binding evidence for a receiver call, so skip straight to
            // the classify tiers — B1/B1b/B2/repo-scan-bind/B3 never run for
            // this edge, regardless of whether `code_nodes`/`repo_defs` has a
            // same-named def elsewhere in the project.
            let module_evidence = module_for_pending(p, &imports_module_by_file);
            let has_import = imports_by_file
                .get(&p.src_file)
                .is_some_and(|s| s.contains(&p.name));
            // X4 precondition (WCR truth pass, TASK 2): `defs` above only
            // reflects THIS receiver-gated edge's B0 same-file eligibility —
            // it says nothing about whether `code_nodes`/`repo_defs` has a
            // def of `N` elsewhere in the project (a method-gated edge
            // deliberately never looks past B0). The X4 tier must never
            // preempt a bind tier, so it independently verifies zero defs
            // ANYWHERE (code_nodes across the whole project, not just this
            // file, AND repo_defs) before it is even offered the chance to
            // fire inside `classify_only`.
            let zero_defs_anywhere = defs.map(|d| d.is_empty()).unwrap_or(true)
                && repo_candidate_files(conn, &p.project, &p.name)?.is_empty();
            match classify_only(
                conn,
                p,
                has_import,
                module_evidence,
                &imports_module_by_file,
                &mut memo,
                file_exists,
                zero_defs_anywhere,
                &local_bindings,
            )? {
                Some("external") => external += 1,
                Some("method") => method += 1,
                Some("internal_module") => internal_module += 1,
                Some("stale") => stale += 1,
                Some("local") => local += 1,
                _ => {}
            }
            continue;
        }

        // --- B1: module-path precise bind (WCR Phase 8, TASK A) ----------
        // An import naming one specific file is not ambiguous even when
        // many OTHER files define the same symbol name — must run BEFORE
        // any ambiguity below is declared. No-ops (returns `None`) whenever
        // there is no relative/alias module evidence, the module doesn't
        // resolve to a real on-disk file, or that file has no def of `N`.
        // Only reached for `gate == Unrestricted` (a non-method edge, or a
        // legacy `callee_kind = ''` edge predating the extraction capture —
        // see `MethodGate::Unrestricted`'s doc comment) — a receiver call
        // never reaches a cross-file name-only tier like this one.
        let module_evidence = module_for_pending(p, &imports_module_by_file);
        if let Some((dst, evidence)) = module_bind_tier(
            conn,
            p,
            module_evidence,
            defs,
            &project_roots_for(conn, &p.project, &mut memo),
            &mut memo,
        )? {
            bind(conn, p, &dst, &evidence)?;
            resolved += 1;
            continue;
        }

        // Distinct candidate files -> first (sorted) def id at that file.
        let mut distinct: BTreeMap<String, String> = BTreeMap::new();
        if let Some(d) = defs {
            for (file, id) in d {
                distinct.entry(file.clone()).or_insert_with(|| id.clone());
            }
        }

        let has_import = imports_by_file
            .get(&p.src_file)
            .is_some_and(|s| s.contains(&p.name));

        if distinct.is_empty() {
            // No code_nodes def anywhere. repo_defs (whole-repo scan) may
            // still know about it.
            let repo_files = repo_candidate_files(conn, &p.project, &p.name)?;
            if repo_files.len() == 1 {
                let file = repo_files.iter().next().expect("len == 1");
                bind(conn, p, &p.dst_id, &format!("repo_scan:{}", basename(file)))?;
                resolved += 1;
            } else if repo_files.len() >= 2 {
                // Multiple repo-scanned candidates, nothing to disambiguate
                // with (no code_nodes signal, that's the branch we're in) —
                // genuinely ambiguous, never guess. Unless the file itself is
                // gone (TASK C): then the ambiguity is moot, it's history.
                if resolve_stale_or_unexplained(conn, p, file_exists, &mut memo)? {
                    stale += 1;
                } else {
                    ambiguous_remaining += 1;
                }
            } else {
                // X4 precondition (WCR truth pass, TASK 2): reached only
                // when `distinct.is_empty()` (zero code_nodes defs) AND
                // `repo_files` is neither len==1 nor len>=2 — i.e. also
                // empty. Zero defs anywhere is therefore ALREADY guaranteed
                // here, unconditionally — no extra query needed (unlike the
                // gate-based call site above, which has to verify it itself).
                match classify_only(
                    conn,
                    p,
                    has_import,
                    module_evidence,
                    &imports_module_by_file,
                    &mut memo,
                    file_exists,
                    true,
                    &local_bindings,
                )? {
                    Some("external") => external += 1,
                    Some("method") => method += 1,
                    Some("internal_module") => internal_module += 1,
                    Some("stale") => stale += 1,
                    Some("local") => local += 1,
                    _ => {}
                }
            }
            continue;
        }

        if distinct.len() == 1 {
            let (_, id) = distinct.iter().next().expect("len == 1");
            let evidence = if has_import {
                "import_bound"
            } else {
                "unique_def"
            };
            bind(conn, p, id, evidence)?;
            resolved += 1;
            continue;
        }

        // distinct.len() >= 2: ambiguous by code_nodes alone. An import edge
        // plus a unique repo_defs file that DOES have a matching code_nodes
        // def there is strong enough evidence to disambiguate confidently.
        if has_import {
            let repo_files = repo_candidate_files(conn, &p.project, &p.name)?;
            if repo_files.len() == 1 {
                let repo_file = repo_files.iter().next().expect("len == 1").as_str();
                if let Some(id) = distinct.get(repo_file) {
                    bind(conn, p, id, "import_bound")?;
                    resolved += 1;
                    continue;
                }
                // repo_defs points at a file code_nodes has no def in, but
                // code_nodes DOES have candidates elsewhere. Picking the
                // repo_scan file here would silently discard those
                // candidates — conservative: fall through to B3 instead.
            }
        }

        // B3: corpus-witnessed via co-edit weight, strict 2x margin.
        let f_canon = canon_path(&p.src_file, &mut memo);
        let mut scored: Vec<(&String, i64)> = Vec::with_capacity(distinct.len());
        for (file, id) in &distinct {
            let d_canon = canon_path(file, &mut memo);
            let weight = coedit_weight(conn, &f_canon, &d_canon, &mut memo)?;
            scored.push((id, weight));
        }
        // `distinct` iterates file-sorted; a stable sort by weight desc keeps
        // ties in file-ascending order — deterministic tie-break.
        scored.sort_by_key(|(_, weight)| std::cmp::Reverse(*weight));
        let (top_id, weight_top) = scored[0];
        let weight_runnerup = scored.get(1).map(|s| s.1).unwrap_or(0);
        if weight_top >= 2 && weight_top >= 2 * weight_runnerup {
            bind(conn, p, top_id, &format!("coedit:{weight_top}"))?;
            resolved += 1;
        } else if resolve_stale_or_unexplained(conn, p, file_exists, &mut memo)? {
            stale += 1;
        } else {
            ambiguous_remaining += 1;
        }
    }

    let unexplained =
        total - resolved - external - method - stale - internal_module - drifted - local;
    let closure_rate = if total == 0 {
        1.0
    } else {
        (resolved + external + method + stale + internal_module + drifted + local) as f64
            / total as f64
    };
    let internal_denominator =
        total - external - method - stale - internal_module - drifted - local;
    let internal_binding_rate = if internal_denominator == 0 {
        1.0
    } else {
        resolved as f64 / internal_denominator as f64
    };

    Ok(ResolveStats {
        total,
        resolved,
        bound: resolved,
        external,
        method,
        stale,
        internal_module,
        drifted,
        local,
        unexplained,
        ambiguous_remaining,
        closure_rate,
        internal_binding_rate,
    })
}

/// X0: language-defined prelude/builtin/global tier. The caller only reaches
/// here once it has confirmed no def exists anywhere (code_nodes or
/// repo_defs) for the name — checked BEFORE X1/X2 since these names are
/// never `use`/`import`ed (always in scope), so X1's import-evidence
/// requirement can never see them.
fn builtin_tier(p: &Pending) -> Option<(String, String)> {
    let lang_key = manifest::builtin_lang_key_from_file(&p.src_file)?;
    if manifest::classify_builtin(lang_key, &p.name) {
        Some(("external".to_string(), format!("builtin:{lang_key}")))
    } else {
        None
    }
}

/// The `from:<module>`/sibling-import module specifier backing `p`'s bound
/// name, when available — shared by `module_bind_tier` (WCR Phase 8, TASK A)
/// and `classify_tier`'s X1 caller (WCR Phase 5/7). For an `imports` edge
/// this is the edge's OWN evidence; for a `calls` edge (which never carries
/// `from:` evidence directly) it is the SAME-FILE SIBLING `imports` edge's
/// module for the same bound name (`imports_module_by_file`).
///
/// Replay-safety (WCR Phase 7): `classify_tier`'s module-based branch
/// overwrites an `imports` edge's own evidence from `from:<module>` to
/// `import:<module>` — the identical module string, just re-prefixed.
/// Recognizing `import:` as an equally-valid prefix here (alongside
/// `from:`) makes a second `resolve_edges` pass over an unchanged DB
/// re-derive the SAME module value instead of losing it and falling back to
/// a degraded match. `import:` can only have been written onto an `imports`
/// edge by that same branch (no other tier prefixes an `imports` edge's
/// evidence with `import:`), so the recognition is unambiguous.
fn module_for_pending<'a>(
    p: &'a Pending,
    imports_module_by_file: &'a BTreeMap<String, BTreeMap<String, String>>,
) -> Option<&'a str> {
    if p.kind == "imports" {
        p.evidence
            .strip_prefix("from:")
            .or_else(|| p.evidence.strip_prefix("import:"))
            .filter(|m| !m.is_empty())
    } else {
        imports_module_by_file
            .get(&p.src_file)
            .and_then(|by_name| by_name.get(&p.name))
            .map(String::as_str)
    }
}

/// X1 (external) then X2 (method) — the caller only reaches here once it has
/// confirmed no def exists anywhere (code_nodes or repo_defs) for the name,
/// no X0 builtin match, and no X1b qualifier match either.
///
/// `module` is the `from:<module>` specifier backing this name's import
/// evidence (Phase 5 TASK 1/4), when available: the pending edge's own
/// evidence for an `imports` edge, or a same-file sibling `imports` edge's
/// module for a `calls` edge (see `module_for_pending`). When present it
/// fully replaces the degraded bound-symbol-name match — a relative module
/// is never external, a bare module is classified by its first path segment
/// (`manifest::ExternalNs::classify_module`). When absent (no `from:` data —
/// older edges, synthetic/test edges), falls back to the original
/// symbol-name-only match (`manifest::ExternalNs::classify`). `ns` is the
/// caller-resolved namespace set (WCR Phase 6 TASK E: includes the
/// project-root fallback — see `external_ns_for`) for `p.src_file`.
fn classify_tier(
    p: &Pending,
    has_import: bool,
    module: Option<&str>,
    ns: &manifest::ExternalNs,
) -> Option<(String, String)> {
    // X1: only meaningful when the name was actually imported in the
    // caller's file — no textual evidence, no external classification.
    if has_import {
        match module {
            Some(m) if manifest::is_relative_module(m) => {
                // Internal candidate (relative specifier / tsconfig alias) —
                // never external.
            }
            Some(m) => {
                if ns.classify_module(m) {
                    return Some(("external".to_string(), format!("import:{m}")));
                }
                // TASK B (WCR Phase 7): installed-package witness — the
                // manifest doesn't declare `m` directly, but it may still be
                // a real, installed (transitive/bundled) dependency. JS/TS
                // only: `node_modules` is a JS/Node concept, and a bare
                // module here is already guaranteed non-relative/non-alias
                // by the sibling match arm above.
                if manifest::builtin_lang_key_from_file(&p.src_file) == Some("js") {
                    let dir = Path::new(&p.src_file)
                        .parent()
                        .unwrap_or_else(|| Path::new("."));
                    if let Some(pkg) = manifest::node_modules_package_witness(dir, m) {
                        return Some(("external".to_string(), format!("installed:{pkg}")));
                    }
                }
            }
            None => {
                if let Some(matched) = ns.classify(&p.name) {
                    return Some(("external".to_string(), format!("import:{matched}")));
                }
            }
        }
    }
    // X2: a `calls` edge invoked off a receiver. Finding 2 (WCR truth pass):
    // this is reached both from the historical "no def anywhere" path AND,
    // as of `method_bind_gate`, directly for any gated method edge — a
    // same-named def existing elsewhere in the project is not evidence for
    // THIS receiver call, so "no def anywhere" is no longer a precondition
    // for classifying `boundary = 'method'` here.
    if p.kind == "calls" && p.callee_kind == "method" {
        return Some(("method".to_string(), "receiver_call".to_string()));
    }
    None
}

/// Classify tiers only (X0 `builtin_tier` / X1b `qualifier_tier` / X1c
/// `qualifier_import_tier` / X1+X2 `classify_tier` / `internal_module_tier` /
/// X4 local-binding witness / `stale`-or-clear) — no bind tier runs here.
/// Two callers:
///
/// - The historical "no `code_nodes` def anywhere AND no (or ambiguous)
///   `repo_defs` candidate" path in `resolve_edges`'s `distinct.is_empty()`
///   branch (unchanged behavior, just extracted into a function).
/// - Finding 2 (WCR truth pass) `method_bind_gate`'s `SelfOnly`/`NoBind`
///   edges: a receiver call (`obj.save()`) skips every bind tier and lands
///   here directly, REGARDLESS of whether `code_nodes`/`repo_defs` has a
///   same-named def — a shared bare name is not binding evidence for a
///   receiver call, so `classify_tier`'s X2 method classification must be
///   reachable even when such defs exist elsewhere in the project.
///
/// `zero_defs_anywhere` (WCR truth pass, TASK 2) gates the X4 tier alone:
/// `true` iff `N` has NO def anywhere (code_nodes project-wide, AND
/// repo_defs) — the caller computes this itself (see each call site in
/// `resolve_edges`; the `distinct.is_empty()` call site gets it for free
/// from checks it already made, the method-gate call site verifies it
/// independently since a gated edge's own `defs` lookup only reflects B0
/// same-file eligibility). `local_bindings` is the X4 witness table (see
/// `ResolveStats::local`'s doc comment).
///
/// Performs the DB write itself (`classify_edge`, or `clear_edge` via
/// `resolve_stale_or_unexplained`) and reports which `ResolveStats` bucket
/// to increment; `None` means the edge ends this pass `unexplained` (no
/// counter to bump — `unexplained` is derived arithmetically from `total`).
#[allow(clippy::too_many_arguments)]
fn classify_only(
    conn: &Connection,
    p: &Pending,
    has_import: bool,
    module: Option<&str>,
    imports_module_by_file: &BTreeMap<String, BTreeMap<String, String>>,
    memo: &mut Memo,
    file_exists: &dyn Fn(&str) -> bool,
    zero_defs_anywhere: bool,
    local_bindings: &BTreeMap<(String, String), BTreeSet<(String, String)>>,
) -> Result<Option<&'static str>> {
    if let Some((boundary, evidence)) = builtin_tier(p) {
        // X0: language-defined prelude/builtin/global — runs before X1
        // since these names are never import-evidenced.
        classify_edge(conn, p, &boundary, &evidence)?;
        return Ok(Some("external"));
    }
    if let Some((boundary, evidence)) = qualifier_tier(conn, p, memo)? {
        // X1b (TASK B): `via:<qualifier>` evidence classifies purely from
        // the qualifier's root, independent of import evidence.
        classify_edge(conn, p, &boundary, &evidence)?;
        return Ok(Some("external"));
    }
    // Replay-safety (WCR Phase 7): `classify_tier`'s module-based branch
    // below overwrites this edge's own evidence from `from:<module>` to
    // `import:<module>` — the identical module string, just re-prefixed.
    // `module_for_pending` recognizes `import:` as an equally valid prefix
    // (alongside `from:`), so a second pass over an unchanged DB re-derives
    // the SAME classification from the SAME module value instead of losing
    // it. See `module_for_pending`'s own doc comment.
    let ns = external_ns_for(conn, &p.src_file, &p.project, memo);
    if let Some((boundary, evidence)) = qualifier_import_tier(p, imports_module_by_file, &ns) {
        // X1c (WCR Phase 7, TASK A): qualifier -> import two-hop chain — the
        // qualifier's root itself is an imported symbol, join through its
        // module.
        classify_edge(conn, p, &boundary, &evidence)?;
        return Ok(Some("external"));
    }
    if let Some((boundary, evidence)) = classify_tier(p, has_import, module, &ns) {
        classify_edge(conn, p, &boundary, &evidence)?;
        return Ok(Some(if boundary == "method" {
            "method"
        } else {
            "external"
        }));
    }
    if let Some((boundary, evidence)) =
        internal_module_tier(p, &project_roots_for(conn, &p.project, memo))
    {
        // WCR Phase 7, TASK E: relative/alias import module resolves to a
        // real on-disk file — explained, but not a symbol-level bind (see
        // `ResolveStats::internal_module`).
        classify_edge(conn, p, &boundary, &evidence)?;
        return Ok(Some("internal_module"));
    }
    // X4 (WCR truth pass, TASK 2): local-binding witness — the LAST classify
    // tier, and ONLY for names with zero defs anywhere (never guessed, never
    // able to preempt a bind tier — see `zero_defs_anywhere`'s doc comment
    // above and `ResolveStats::local`). `N` is a witnessed function/closure
    // parameter or local variable/destructuring target in the edge's own
    // (project, src_file) per `local_bindings` — real, disk-verified AST
    // evidence, not a name-shape guess.
    //
    // Scope-qualified as of the X4 adversarial review (Finding 1), and
    // CHAIN-qualified (prefix match, not exact) as of the second X4
    // adversarial review (Finding 4): a witness only counts when its scope
    // CHAIN is this edge's own call-site chain OR AN OUTER PREFIX OF IT —
    // never merely "some binding of this name exists somewhere in the
    // file", and never merely "same enclosing NAMED def" either. Finding 1
    // alone (flat scope-equality on the enclosing named def's bare name)
    // was still too coarse: `nearest_def_node` deliberately flattens every
    // anonymous closure onto the outer named def for GRAPH attribution, so
    // a parameter named `handler` bound in one anonymous closure and an
    // unrelated `handler()` call in a SIBLING anonymous closure — both
    // nested in the SAME named function — reported the SAME bare scope and
    // wrongly matched. `chain_prefix_matches` fixes this: "outer bindings
    // flow inward" (a binding declared directly in a function's body is
    // visible to every closure nested inside it) but "never sideways" (a
    // binding declared inside one closure is invisible to an unrelated
    // sibling closure, even though both share the same outer named def).
    // See `chain_prefix_matches`'s own doc comment.
    //
    // UNIVERSAL quantification over `p.call_scope_chains` as of the Codex
    // round 4 adversarial review, Finding 1: `code_edges` AGGREGATES every
    // physical call/import site sharing this edge's `(src_id, dst_id, kind)`
    // into ONE row, so `call_scope_chains` can hold MORE THAN ONE chain —
    // one per distinct site folded into the aggregate. A binding only
    // witnesses the WHOLE aggregate when its chain is a prefix of EVERY ONE
    // of them (`.all(..)`), not merely one — the previous "first chain
    // recorded" behavior was order-dependent and could falsely witness an
    // aggregate via a binding that only encloses ONE of its (possibly many)
    // sibling-closure occurrences. `call_scope_chains` is guaranteed
    // non-empty (see that field's own doc comment), so `.all(..)` here is
    // never vacuously `true` by construction.
    if zero_defs_anywhere {
        if let Some(pairs) = local_bindings.get(&(p.project.clone(), p.src_file.clone())) {
            let hit = pairs.iter().any(|(binding_chain, name)| {
                name == &p.name
                    && p.call_scope_chains
                        .iter()
                        .all(|call_chain| chain_prefix_matches(binding_chain, call_chain))
            });
            if hit {
                classify_edge(conn, p, "local", &format!("local_scope:{}", p.name))?;
                return Ok(Some("local"));
            }
        }
    }
    if resolve_stale_or_unexplained(conn, p, file_exists, memo)? {
        return Ok(Some("stale"));
    }
    Ok(None)
}

/// Second X4 adversarial review, Finding 4 (sibling anonymous-closure
/// conflation): `true` iff `binding_chain` is `call_chain` itself, OR a
/// strict OUTER PREFIX of it on `>`-delimited chain-segment boundaries (see
/// `extraction::codegraph::scope_chain`'s doc comment for the chain
/// format) — "outer bindings flow inward, a binding can never witness a
/// sibling closure it doesn't enclose".
///
/// Segment-boundary-safe: a plain `str::starts_with` would wrongly match
/// `"Component"` against `"ComponentX>anon1"` (a different function that
/// merely shares a name prefix) — `strip_prefix` followed by requiring the
/// remainder start with `'>'` rules that out; only a REAL chain-segment
/// boundary counts as "nested inside".
///
/// `binding_chain == ""` (module-level, per `scope_chain`'s convention) is
/// the one deliberate non-generalization: kept as an EXACT match against
/// `call_chain == ""` only, not extended to "module scope encloses
/// everything so it should prefix-match every chain" — unchanged behavior
/// from before chains existed (WCR truth pass, Finding B spec), and out of
/// scope for this fix.
fn chain_prefix_matches(binding_chain: &str, call_chain: &str) -> bool {
    if binding_chain.is_empty() {
        return call_chain.is_empty();
    }
    binding_chain == call_chain
        || call_chain
            .strip_prefix(binding_chain)
            .is_some_and(|rest| rest.starts_with('>'))
}

/// Lookups a resolve pass would otherwise repeat for every pending edge. A
/// pass is single-threaded and sees the tables it reads unchanged (the loop
/// only rewrites `code_edges`), so each answer is computed once and kept.
///
/// Measured 2026-10-21 on the owner's 9.9 GB database: one pass over a 56-edge
/// file took 32 s, 70% of it in `coedit_weight`'s self-join and 17% in
/// `repo_has_module_named` re-reading every file of the project per edge.
#[derive(Default)]
struct Memo {
    /// `repo_scan::project_roots` per project (see `project_roots_for`).
    roots: BTreeMap<String, Vec<PathBuf>>,
    /// Per project, every file stem and directory name of its `repo_defs` and
    /// `code_nodes` files: the answer set of `repo_has_module_named`.
    module_names: BTreeMap<String, BTreeSet<String>>,
    /// Per project, the base names of its Python `repo_defs` files.
    python_files: BTreeMap<String, BTreeSet<String>>,
    /// `canon_path` per input.
    canon: BTreeMap<String, String>,
    /// The caller's `file_exists` answer per `Pending::src_file`.
    src_file_exists: BTreeMap<String, bool>,
    /// `external_ns_for` per `(src_file, project)`.
    ns: BTreeMap<(String, String), Rc<manifest::ExternalNs>>,
    /// Per canonical file `f`, every file edited in a session that also edited
    /// `f`, with the count of those sessions (see `coedit_weight`).
    coedit: BTreeMap<String, BTreeMap<String, i64>>,
}

/// Memoized `repo_scan::project_roots` lookup shared by `external_ns_for`
/// (X1's manifest fallback, WCR Phase 6 TASK E) and `internal_module_tier`
/// (WCR Phase 7 TASK E's alias-module resolution) — both need "this
/// project's known roots" and a resolve pass typically has many pending
/// edges sharing a project, so the DB query only runs once per project.
fn project_roots_for(conn: &Connection, project: &str, memo: &mut Memo) -> Vec<PathBuf> {
    if !memo.roots.contains_key(project) {
        let roots = repo_scan::project_roots(conn, project).unwrap_or_default();
        memo.roots.insert(project.to_string(), roots);
    }
    memo.roots.get(project).cloned().unwrap_or_default()
}

/// TASK E (WCR Phase 6): resolve the external namespaces reachable from a
/// pending edge's file, falling back to the project's other known roots
/// when the direct single-file ancestor walk finds no manifest at all.
/// `roots_cache` memoizes `repo_scan::project_roots` per project across the
/// whole resolve pass (see `project_roots_for`); the answer itself is kept per
/// `(src_file, project)`.
fn external_ns_for(
    conn: &Connection,
    src_file: &str,
    project: &str,
    memo: &mut Memo,
) -> Rc<manifest::ExternalNs> {
    let key = (src_file.to_string(), project.to_string());
    if let Some(ns) = memo.ns.get(&key) {
        return Rc::clone(ns);
    }
    let ns = manifest::external_namespaces(Path::new(src_file));
    let ns = if !ns.rust_deps.is_empty() || !ns.js_deps.is_empty() {
        ns
    } else {
        let roots = project_roots_for(conn, project, memo);
        manifest::apply_project_root_fallback(ns, &roots)
    };
    let ns = Rc::new(ns);
    memo.ns.insert(key, Rc::clone(&ns));
    ns
}

/// The root segment of a qualifier path: everything before the first `::`
/// (Rust) or `.` (Python) — whichever appears. TASK B (WCR Phase 6).
fn qualifier_root(qualifier: &str) -> &str {
    match qualifier.find("::") {
        Some(i) => &qualifier[..i],
        None => match qualifier.find('.') {
            Some(i) => &qualifier[..i],
            None => qualifier,
        },
    }
}

/// Finding 2 (WCR truth pass): how far a `calls` edge is allowed to travel
/// through the BIND tiers (B0/B1/B1b/B2/repo-scan-bind/B3) before name
/// identity alone would be treated as binding evidence. See
/// `method_bind_gate`'s doc comment for the receiver-evidence rule this
/// encodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MethodGate {
    /// Not a receiver call (`p.kind != "calls"`, or `callee_kind` is `""`
    /// legacy/unset or `"direct"`) — the full, unrestricted tier order runs
    /// exactly as before this finding.
    Unrestricted,
    /// A method call off `self`/`this`/`cls` — an instance/class method
    /// calling its own class's method. Only the B0 same-file bind tier is
    /// receiver-consistent with that; every cross-file name-only tier
    /// (B1 onward) is skipped.
    SelfOnly,
    /// A method call off any other receiver, or a method call with no
    /// `via:<qualifier>` evidence at all (e.g. Rust `field_expression`
    /// receiver calls — `call_qualifier` never captures one for that AST
    /// shape, see `extraction::codegraph::call_qualifier` — or any Go
    /// `selector_expression` call, which `call_qualifier` doesn't handle at
    /// all). No bind tier is receiver-consistent, including B0: two
    /// unrelated types can each happen to define a same-named method, and a
    /// same-file def with that bare name is no more this receiver's method
    /// than a cross-file one. Every bind tier is skipped.
    NoBind,
}

/// Receiver names that denote "this call targets a method on the object's
/// OWN instance/class", not some other qualifier: Python/Rust/JS/TS `self`,
/// JS/TS `this`, Python `cls`. Distinct from `PY_RECEIVER_NAMES` below (that
/// list excludes these from X1b's *external-namespace* classification; this
/// one governs the receiver-aware *bind*-tier gate, Finding 2).
const METHOD_SELF_RECEIVERS: &[&str] = &["self", "this", "cls"];

/// Finding 2 (WCR truth pass): `receiver.save()` binding to an unrelated
/// same-named `save` purely because the bare callee names match is a real
/// provenance-corrupting bug — name identity alone is never binding
/// evidence for a receiver call — while flattering the internal-binding
/// metric (a false bind counts as `bound`). Only a `self`/`this`/`cls`
/// receiver is receiver-consistent with the existing B0 same-file tier (an
/// instance method calling its own class's method); every other receiver,
/// or a method call with no `via:<qualifier>` evidence captured at all,
/// gets NO bind tier — it proceeds straight to the classify tiers
/// (qualifier/external/method — see `classify_only`), where a `calls` edge
/// with `callee_kind == "method"` and no other explanation lands on X2
/// (`boundary = "method"`) regardless of whether same-named defs exist
/// elsewhere in the project (they are not evidence for THIS receiver call).
///
/// Reads `p.evidence`'s `via:<qualifier>` (extraction-time capture) or,
/// replay-safely, an already-classified `import:<qualifier>` (X1b/X1c may
/// have rewritten it on a prior pass — same recognition convention as
/// `module_for_pending`) to find the qualifier, then takes its ROOT segment
/// (`qualifier_root`) — `self.x.run()`'s root is `self`, same as
/// `self.run()`'s.
fn method_bind_gate(p: &Pending) -> MethodGate {
    if p.kind != "calls" || p.callee_kind != "method" {
        return MethodGate::Unrestricted;
    }
    let Some(qualifier) = p
        .evidence
        .strip_prefix("via:")
        .or_else(|| p.evidence.strip_prefix("import:"))
    else {
        return MethodGate::NoBind;
    };
    if qualifier.is_empty() {
        return MethodGate::NoBind;
    }
    let root = qualifier_root(qualifier);
    if METHOD_SELF_RECEIVERS.contains(&root) {
        MethodGate::SelfOnly
    } else {
        MethodGate::NoBind
    }
}

/// Python receiver-name conventions that are NEVER a module: `self`/`cls`
/// are how every instance/class method call spells its qualifier
/// (`self.run()`), and Python's `attribute` AST node can't syntactically
/// distinguish that from a real module-qualified call (`json.dumps()`) —
/// both get a `via:` qualifier captured (TASK A). Excluding these two
/// reserved-by-convention names here keeps `qualifier_tier` from
/// misclassifying essentially every instance-method call in a Python
/// codebase as "external `self`". Not a builtin/global (already covered by
/// `manifest::PY_BUILTINS`); this is specifically about a call receiver
/// identity, a distinct concern.
const PY_RECEIVER_NAMES: &[&str] = &["self", "cls"];

/// X1b qualifier-aware tier (WCR Phase 6, TASK B): classify a pending
/// `calls` edge carrying `via:<qualifier>` evidence (extraction TASK A)
/// purely from the qualifier's ROOT segment, independent of whether the
/// qualifier itself was textually `use`/`import`ed — a fully-qualified path
/// call (`std::fs::read_to_string`, or `fs::read_to_string` after
/// `use std::fs;`) never needs one. Only reached once the bare callee name
/// has already exhausted every def-based tier (no code_nodes def, no
/// repo_defs candidate) and the X0 builtin check on the bare name has
/// already failed — see the caller in `resolve_edges`.
///
/// - Rust: external iff the root is `std`/`core`/`alloc`/`proc_macro` or a
///   Cargo dependency (`ExternalNs::classify`, reusing the exact same
///   manifest machinery as the degraded X1 tier) — UNLESS the root also
///   matches a repo module name (a `repo_defs`/`code_nodes` file stem or
///   path-component/directory name for this project): an internal module
///   sharing a name with a builtin/dep namespace (a local `src/fs.rs`
///   wrapper, say) must never be misclassified as external — left
///   unclassified instead, eligible for a bind tier or `unexplained`.
/// - Python: external iff the root is a `py_stdlib` top-level module name,
///   OR (conservatively) there is no `repo_defs` file named `<root>.py` for
///   this project — i.e. positive stdlib evidence, or at minimum an
///   absence-of-repo-ownership signal. `self`/`cls` are excluded outright
///   (see `PY_RECEIVER_NAMES`) — they are call-receiver convention, not a
///   module, regardless of repo file names.
/// - Any other language, or no match at all -> `None` (unexplained), never
///   a guess.
///
/// Determinism note: `classify_edge` overwrites the edge's `evidence`
/// column with this tier's own `import:<qualifier>` output, so a SECOND
/// `resolve_edges` call re-reads `import:<qualifier>`, not the original
/// `via:<qualifier>` extraction wrote. Recognizing `import:` as an
/// equally-valid prefix here (alongside `via:`) makes re-derivation
/// byte-identical on rerun — required for "running the pass twice on an
/// unchanged DB produces identical stats". Safe in practice: the only other
/// producer of `import:`-prefixed evidence on a `calls` edge is
/// `classify_tier`'s own X1, which requires the qualified call's BARE
/// trailing name to ALSO be separately `use`/`import`ed in the same file —
/// self-contradictory real-world code (you'd call it bare, not qualified,
/// if you'd imported the trailing name), so the two tiers' inputs don't
/// realistically overlap.
fn qualifier_tier(
    conn: &Connection,
    p: &Pending,
    memo: &mut Memo,
) -> Result<Option<(String, String)>> {
    if p.kind != "calls" {
        return Ok(None);
    }
    let Some(qualifier) = p
        .evidence
        .strip_prefix("via:")
        .or_else(|| p.evidence.strip_prefix("import:"))
    else {
        return Ok(None);
    };
    if qualifier.is_empty() {
        return Ok(None);
    }
    let root = qualifier_root(qualifier);
    if root.is_empty() {
        return Ok(None);
    }

    let lang_key = manifest::builtin_lang_key_from_file(&p.src_file);
    let is_external = match lang_key {
        Some("rust") => {
            if repo_has_module_named(conn, &p.project, root, memo)? {
                false
            } else {
                let ns = manifest::external_namespaces(Path::new(&p.src_file));
                ns.classify(root).is_some()
            }
        }
        Some("python") => {
            if PY_RECEIVER_NAMES.contains(&root) {
                false
            } else {
                let ns = manifest::external_namespaces(Path::new(&p.src_file));
                ns.py_stdlib.contains(root)
                    || !repo_has_python_file_named(conn, &p.project, root, memo)?
            }
        }
        _ => false,
    };

    Ok(if is_external {
        Some(("external".to_string(), format!("import:{qualifier}")))
    } else {
        None
    })
}

/// X1c qualifier -> import two-hop chain (WCR Phase 7, TASK A): a pending
/// `calls` edge carrying `via:<qualifier>` evidence whose root segment R
/// (see `qualifier_root`) is NOT itself a builtin namespace or manifest dep
/// name (`qualifier_tier` already returned `None` — the caller only reaches
/// here after that) can still be explained when R is the LOCAL BINDING NAME
/// of an actual import in the SAME file: `use std::time::Instant;` then
/// `Instant::now()` — the qualifier is `Instant`, root `Instant`, and
/// `Instant` is not itself `std`/`core`/a crate name, so the direct
/// `qualifier_tier` check misses it. But the sibling `imports` edge for
/// symbol `Instant` carries `from:std::time` evidence (captured at
/// extraction time), and joining through THAT module — via the exact same
/// module matcher X1 already uses (`ExternalNs::classify_module`), never a
/// separate/looser rule — proves the call external: `import:std::time` +
/// `::` + `Instant` reconstructs the fully-qualified path the `use`
/// statement elided (`std::time::Instant`). Also covers the bare
/// bring-into-scope case `use std::fs;` (symbol `fs` from module `std`)
/// backing a later `fs::read_to_string(...)` call — `fs` is the qualifier
/// root but is not itself a builtin/dep name.
///
/// Restricted to Rust/Python (`qualifier_tier`'s own language set) so the
/// module in the reconstructed evidence (`import:<module>::<qualifier>`)
/// always shares a first segment with a name `qualifier_tier` itself can
/// directly re-derive on a SECOND resolve pass (its own `import:` prefix
/// recognition — see that function's determinism note): the module's first
/// segment is exactly what `ExternalNs::classify_module` matched to return
/// `true`, so `qualifier_root` of the reconstructed string on replay lands
/// back on that same segment, and `qualifier_tier` classifies it directly
/// without ever needing this two-hop join again. JS/TS is deliberately out
/// of scope here — `qualifier_tier` never re-derives a JS `import:` value
/// on replay (its match arm is `_ => false`), so accepting only the
/// ORIGINAL `via:` prefix (not `import:`) keeps this tier itself from
/// re-processing its own prior output — no replay ambiguity, by construction.
fn qualifier_import_tier(
    p: &Pending,
    imports_module_by_file: &BTreeMap<String, BTreeMap<String, String>>,
    ns: &manifest::ExternalNs,
) -> Option<(String, String)> {
    if p.kind != "calls" {
        return None;
    }
    let lang_key = manifest::builtin_lang_key_from_file(&p.src_file);
    if !matches!(lang_key, Some("rust") | Some("python")) {
        return None;
    }
    let qualifier = p.evidence.strip_prefix("via:")?;
    if qualifier.is_empty() {
        return None;
    }
    let root = qualifier_root(qualifier);
    if root.is_empty() {
        return None;
    }
    let module = imports_module_by_file.get(&p.src_file)?.get(root)?;
    if manifest::is_relative_module(module) {
        return None;
    }
    if ns.classify_module(module) {
        Some((
            "external".to_string(),
            format!("import:{module}::{qualifier}"),
        ))
    } else {
        None
    }
}

/// True when `root` matches a repo module name for `project`: the file stem
/// (filename minus extension) or any path-component (directory name) of any
/// `repo_defs`/`code_nodes` file recorded for this project. Guards
/// `qualifier_tier`'s Rust rule against misclassifying a same-named INTERNAL
/// module path (a local `src/fs.rs` wrapper's `fs::helper()`) as an external
/// stdlib/dependency reference just because the root segment happens to
/// collide with a builtin/dep namespace name.
fn repo_has_module_named(
    conn: &Connection,
    project: &str,
    root: &str,
    memo: &mut Memo,
) -> Result<bool> {
    if !memo.module_names.contains_key(project) {
        let mut names = BTreeSet::new();
        let mut stmt = conn.prepare(
            "SELECT file FROM repo_defs WHERE project = ?1
             UNION
             SELECT file FROM code_nodes WHERE project = ?1",
        )?;
        let files = stmt.query_map(params![project], |row| row.get::<_, String>(0))?;
        for file in files {
            let file = file?;
            let path = Path::new(&file);
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                names.insert(stem.to_string());
            }
            for component in path.components() {
                if let std::path::Component::Normal(n) = component {
                    if let Some(n) = n.to_str() {
                        names.insert(n.to_string());
                    }
                }
            }
        }
        memo.module_names.insert(project.to_string(), names);
    }
    Ok(memo
        .module_names
        .get(project)
        .is_some_and(|names| names.contains(root)))
}

/// True when `repo_defs` (for `project`) has a Python file whose basename is
/// exactly `<root>.py`. Backs `qualifier_tier`'s Python rule: the absence of
/// such a file is (conservative) positive evidence the qualifier root is not
/// a local module the repo itself defines.
fn repo_has_python_file_named(
    conn: &Connection,
    project: &str,
    root: &str,
    memo: &mut Memo,
) -> Result<bool> {
    if !memo.python_files.contains_key(project) {
        let mut names = BTreeSet::new();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT file FROM repo_defs WHERE project = ?1 AND lang = 'python'",
        )?;
        let files = stmt.query_map(params![project], |row| row.get::<_, String>(0))?;
        for file in files {
            let file = file?;
            if let Some(name) = Path::new(&file).file_name().and_then(|n| n.to_str()) {
                names.insert(name.to_string());
            }
        }
        memo.python_files.insert(project.to_string(), names);
    }
    let target = format!("{root}.py");
    Ok(memo
        .python_files
        .get(project)
        .is_some_and(|names| names.contains(&target)))
}

/// File extensions/index-suffixes tried, in order, when resolving a
/// relative/alias `imports` module specifier to an on-disk file (WCR Phase
/// 7, TASK E) — the module specifier itself is extensionless (`./util`,
/// never `./util.ts`), same as every JS/TS bundler resolution algorithm.
const INTERNAL_MODULE_EXTENSIONS: &[&str] =
    &[".ts", ".tsx", ".js", ".jsx", "/index.ts", "/index.tsx"];

/// Candidate extensionless base paths for TASK E's internal-module-file
/// witness (WCR Phase 7): a relative module (`./x`, `../x`) resolves
/// against `src_file`'s own parent directory; an alias module (`~/x`,
/// `@/x`) resolves against each of the project's known roots
/// (`repo_scan::project_roots`, passed in by the caller — see
/// `internal_module_tier`), per the "project-root-relative" alias
/// convention already documented on `manifest::is_relative_module` (no
/// `src/`-prefix guess — that mapping isn't derivable without reading
/// tsconfig, which is out of scope here). Multiple roots means multiple
/// candidates tried in the roots' own deterministic order; first hit wins.
fn internal_module_candidate_bases(
    src_file: &str,
    module: &str,
    project_roots: &[PathBuf],
) -> Vec<PathBuf> {
    if let Some(subpath) = module
        .strip_prefix("~/")
        .or_else(|| module.strip_prefix("@/"))
    {
        return project_roots
            .iter()
            .map(|root| root.join(subpath))
            .collect();
    }
    match Path::new(src_file).parent() {
        Some(dir) => vec![dir.join(module)],
        None => Vec::new(),
    }
}

/// Shared by `internal_module_tier` (WCR Phase 7, TASK E) and
/// `module_bind_tier` (WCR Phase 8, TASK A): resolve a relative/alias module
/// specifier to the first candidate on-disk file, trying each
/// `internal_module_candidate_bases` base bare, then with each of
/// `INTERNAL_MODULE_EXTENSIONS` appended, in order. `None` when nothing on
/// disk matches any candidate. Returns the RAW joined path (not
/// canonicalized) — same convention `internal_module_tier` always used for
/// its own `module_file:<basename>` evidence; callers that need to compare
/// against `code_nodes`/`repo_defs` file strings should canonicalize
/// themselves (see `module_bind_tier`'s use of `canon_path`).
fn resolve_module_file(src_file: &str, module: &str, project_roots: &[PathBuf]) -> Option<PathBuf> {
    for base in internal_module_candidate_bases(src_file, module, project_roots) {
        if base.is_file() {
            return Some(base);
        }
        for ext in INTERNAL_MODULE_EXTENSIONS {
            let mut candidate = base.clone().into_os_string();
            candidate.push(ext);
            let candidate = PathBuf::from(candidate);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// B1 module-path precise bind (WCR Phase 8, TASK A): implements the WCR
/// spec's "prefer a def whose file matches the import's module path when
/// derivable" clause — no longer inert now that Phase 5 TASK 1 captures
/// `from:<module>` evidence. `module` is `p`'s import-module specifier (see
/// `module_for_pending`); `defs` is `by_name.get(&p.name)` — every
/// `code_nodes` def of the bound symbol, project-wide, regardless of file
/// count. Resolves `module` to a candidate on-disk file (same resolution as
/// `internal_module_tier`, via `resolve_module_file`) and checks whether
/// EXACTLY THAT FILE has a def of `p.name` — first in `defs`, falling back
/// to `repo_defs` (mirroring the repo_scan-only bind's whole-repo-scan
/// standing). A relative import naming one specific file is precise,
/// unambiguous, textual evidence regardless of how many OTHER files
/// elsewhere also define a same-named symbol (the classic per-component
/// `styles`/`COLORS` const collision — many `X.styles.ts` files each export
/// `styles`, but `import { styles } from './X.styles'` names exactly one).
/// Must run — and does, see its call site at the top of `resolve_edges`'s
/// per-edge loop, right after B0 — BEFORE any ambiguity is declared.
///
/// Returns `None` (never a guess) whenever `module` is absent/not
/// relative-or-alias, doesn't resolve to a real file, or that file has no
/// def of `p.name` in either table. On a hit, returns `(dst_id, evidence)`:
/// the real `code_nodes` id when the def lives there (dst_id gets
/// repointed), else `p.dst_id` itself (the placeholder stays — same
/// convention as the existing repo_scan-only bind) when only `repo_defs`
/// has it. Evidence is always `module_bind:<basename of the resolved
/// file>`.
fn module_bind_tier(
    conn: &Connection,
    p: &Pending,
    module: Option<&str>,
    defs: Option<&Vec<(String, String)>>,
    project_roots: &[PathBuf],
    memo: &mut Memo,
) -> Result<Option<(String, String)>> {
    let Some(module) = module else {
        return Ok(None);
    };
    if module.is_empty() || !manifest::is_relative_module(module) {
        return Ok(None);
    }
    let Some(resolved) = resolve_module_file(&p.src_file, module, project_roots) else {
        return Ok(None);
    };
    let resolved_canon = canon_path(&resolved.to_string_lossy(), memo);
    let evidence = format!("module_bind:{}", basename(&resolved.to_string_lossy()));

    if let Some(defs) = defs {
        for (file, id) in defs {
            if canon_path(file, memo) == resolved_canon {
                return Ok(Some((id.clone(), evidence)));
            }
        }
    }

    for (file, _kind) in crate::storage::codegraph::lookup_repo_defs(conn, &p.project, &p.name)? {
        if canon_path(&file, memo) == resolved_canon {
            return Ok(Some((p.dst_id.clone(), evidence)));
        }
    }

    Ok(None)
}

/// TASK E (WCR Phase 7): internal-module-file witness fallback. An
/// `imports` edge whose module specifier is relative or an alias (see
/// `manifest::is_relative_module`) is never classified `external` by
/// `classify_tier` — correctly, since it names a file inside this project,
/// not a third-party dependency — but when no `code_nodes`/`repo_defs` def
/// exists for the bound symbol either (the caller only reaches here once
/// every bind/classify tier above — including WCR Phase 8's `module_bind_tier`
/// — has failed), the edge would otherwise fall all the way to
/// `unexplained`/`ambiguous` even though the referenced FILE is
/// disk-verifiable. Resolves the module via `resolve_module_file`; a hit
/// proves the MODULE resolves to a real file in this project — it does NOT
/// prove the specific bound symbol lives there (extraction may simply not
/// have indexed that file yet, or the def is a re-export chain this pass
/// doesn't follow — if it DOES, `module_bind_tier` already caught it before
/// this tier ever runs), so it classifies the weaker `internal_module`
/// boundary, never a real bind — see `ResolveStats::internal_module`'s doc
/// comment for why it's excluded from `internal_binding_rate`'s denominator.
///
/// Determinism note: `classify_edge` overwrites the edge's `evidence`
/// column with this tier's own `module_file:<basename>` output (the task
/// spec's evidence format keeps only the resolved file's basename, not the
/// original module specifier), so a SECOND pass over an unchanged DB can no
/// longer see the original `from:<module>` text to re-derive from. Since
/// the module doc comment's determinism invariant is scoped to "an
/// unchanged DB" — nothing about the file-existence check below could have
/// changed between passes under that precondition — a `module_file:` prefix
/// is recognized as this tier's own prior output and reaffirmed directly
/// rather than needing (impossible) re-derivation from a basename alone.
fn internal_module_tier(p: &Pending, project_roots: &[PathBuf]) -> Option<(String, String)> {
    if p.kind != "imports" {
        return None;
    }
    if p.evidence.starts_with("module_file:") {
        return Some(("internal_module".to_string(), p.evidence.clone()));
    }
    let module = p.evidence.strip_prefix("from:")?;
    if module.is_empty() || !manifest::is_relative_module(module) {
        return None;
    }
    let resolved = resolve_module_file(&p.src_file, module, project_roots)?;
    Some((
        "internal_module".to_string(),
        format!("module_file:{}", basename(&resolved.to_string_lossy())),
    ))
}

/// A write the resolve pass decided on while planning (see [`plan_resolve`]):
/// the edge it is for, the state the pass read for that edge, and the change.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedWrite {
    src_id: String,
    dst_id: String,
    kind: String,
    /// The `callee_kind`, `evidence` and `boundary` the pass read: a tier
    /// verdict is a function of them, so [`apply_planned`] writes only to a row
    /// that still holds them.
    callee_kind: String,
    evidence: String,
    boundary: String,
    change: Change,
}

#[derive(Debug, Clone, PartialEq)]
enum Change {
    Bind { new_dst: String, evidence: String },
    Classify { boundary: String, evidence: String },
    Clear,
}

impl PlannedWrite {
    fn of(p: &Pending, change: Change) -> Self {
        Self {
            src_id: p.src_id.clone(),
            dst_id: p.dst_id.clone(),
            kind: p.kind.clone(),
            callee_kind: p.callee_kind.clone(),
            evidence: p.evidence.clone(),
            boundary: p.boundary.clone(),
            change,
        }
    }
}

thread_local! {
    /// Where `bind`/`classify_edge`/`clear_edge` put their writes while a plan
    /// is being made on this thread; `None` (the default) writes through.
    static PLAN: std::cell::RefCell<Option<Vec<PlannedWrite>>> =
        const { std::cell::RefCell::new(None) };
}

/// Whether this thread is making a plan, so a write is queued, not executed.
fn planning() -> bool {
    PLAN.with(|plan| plan.borrow().is_some())
}

fn plan_push(write: PlannedWrite) {
    PLAN.with(|plan| {
        if let Some(writes) = plan.borrow_mut().as_mut() {
            writes.push(write);
        }
    });
}

/// `resolve_edges` that writes nothing: it reads the graph (hold a read
/// transaction around the call for one consistent snapshot) and returns the
/// writes it would have made, in order, with the stats it would have returned.
/// The per-edge tiers read the maps they loaded at the start, never the rows
/// they write, so replaying the list with [`apply_planned`] gives what
/// `resolve_edges` gives. Planning is what lets the project-wide pass run
/// without the database write lock: only the replay needs it.
pub fn plan_resolve(
    conn: &Connection,
    project: &str,
    file_exists: &dyn Fn(&str) -> bool,
) -> Result<(ResolveStats, Vec<PlannedWrite>)> {
    // Restores the previous (normally absent) plan even if the pass panics.
    struct Restore(Option<Vec<PlannedWrite>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            PLAN.with(|plan| *plan.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(PLAN.with(|plan| plan.borrow_mut().replace(Vec::new())));
    let stats = resolve_edges(conn, project, file_exists)?;
    let writes = PLAN
        .with(|plan| plan.borrow_mut().take())
        .unwrap_or_default();
    Ok((stats, writes))
}

/// Replay a [`plan_resolve`] list, in order, inside the caller's transaction.
/// Returns how many writes found their edge changed since the plan was made
/// (re-extracted, or classified by another writer) and left it alone; the
/// caller must not call the project settled while that is above zero.
pub fn apply_planned(conn: &Connection, writes: &[PlannedWrite]) -> Result<usize> {
    const WHERE: &str = "WHERE src_id = ? AND dst_id = ? AND kind = ? AND resolved = 0
         AND callee_kind = ? AND evidence = ? AND boundary = ?";
    let mut bind = conn.prepare(&format!(
        "UPDATE OR REPLACE code_edges SET dst_id = ?, resolved = 1, boundary = '', evidence = ?
         {WHERE}"
    ))?;
    let mut classify = conn.prepare(&format!(
        "UPDATE code_edges SET boundary = ?, evidence = ? {WHERE}"
    ))?;
    let mut clear = conn.prepare(&format!(
        "UPDATE code_edges SET boundary = '', evidence = '' {WHERE}"
    ))?;
    let mut skipped = 0usize;
    for w in writes {
        let key = (
            &w.src_id,
            &w.dst_id,
            &w.kind,
            &w.callee_kind,
            &w.evidence,
            &w.boundary,
        );
        let changed = match &w.change {
            Change::Bind { new_dst, evidence } => bind.execute(params![
                new_dst, evidence, key.0, key.1, key.2, key.3, key.4, key.5
            ])?,
            Change::Classify { boundary, evidence } => classify.execute(params![
                boundary, evidence, key.0, key.1, key.2, key.3, key.4, key.5
            ])?,
            Change::Clear => clear.execute(params![key.0, key.1, key.2, key.3, key.4, key.5])?,
        };
        if changed == 0 {
            skipped += 1;
        }
    }
    Ok(skipped)
}

/// Bind tier: repoint (or, for repo_scan evidence, leave in place) `dst_id`
/// and flip `resolved`. `UPDATE OR REPLACE` tolerates the PK collision when
/// two different placeholder names resolve onto the same real target id.
/// Always clears `boundary` — a bound edge is not a classified boundary edge,
/// even if a prior pass had classified this exact placeholder before a later
/// def appeared for it.
fn bind(conn: &Connection, p: &Pending, new_dst: &str, evidence: &str) -> Result<()> {
    if planning() {
        let change = Change::Bind {
            new_dst: new_dst.to_string(),
            evidence: evidence.to_string(),
        };
        plan_push(PlannedWrite::of(p, change));
        return Ok(());
    }
    conn.execute(
        "UPDATE OR REPLACE code_edges SET dst_id = ?1, resolved = 1, boundary = '', evidence = ?2
         WHERE src_id = ?3 AND dst_id = ?4 AND kind = ?5",
        params![new_dst, evidence, p.src_id, p.dst_id, p.kind],
    )?;
    Ok(())
}

/// Classify tier: leave `resolved = 0`, attach `boundary` + `evidence`.
fn classify_edge(conn: &Connection, p: &Pending, boundary: &str, evidence: &str) -> Result<()> {
    if planning() {
        let change = Change::Classify {
            boundary: boundary.to_string(),
            evidence: evidence.to_string(),
        };
        plan_push(PlannedWrite::of(p, change));
        return Ok(());
    }
    conn.execute(
        "UPDATE code_edges SET boundary = ?1, evidence = ?2
         WHERE src_id = ?3 AND dst_id = ?4 AND kind = ?5",
        params![boundary, evidence, p.src_id, p.dst_id, p.kind],
    )?;
    Ok(())
}

/// Reset `boundary`/`evidence` to '' for an edge that ends this pass fully
/// unexplained or ambiguous — keeps repeated resolve passes idempotent even
/// if an earlier pass (with less graph data) had classified this exact
/// placeholder differently.
fn clear_edge(conn: &Connection, p: &Pending) -> Result<()> {
    if planning() {
        plan_push(PlannedWrite::of(p, Change::Clear));
        return Ok(());
    }
    conn.execute(
        "UPDATE code_edges SET boundary = '', evidence = ''
         WHERE src_id = ?1 AND dst_id = ?2 AND kind = ?3",
        params![p.src_id, p.dst_id, p.kind],
    )?;
    Ok(())
}

/// TASK C (WCR Phase 6), the stale-file tier: a pending edge has exhausted
/// every bind/classify tier above it. If its `src_file` still exists on
/// disk (`file_exists`), fall back to the pre-existing behavior
/// (`clear_edge` — plain unexplained/ambiguous, caller decides which).
/// Otherwise the file is gone: doc-comment rationale — file absence is
/// disk-verifiable evidence, and a stale edge is provenance history whose
/// source no longer exists, not a mystery we failed to explain. Returns
/// `true` iff the edge was classified stale (caller increments its own
/// `stale` counter; when `false` the caller decides whether to also bump
/// `ambiguous_remaining`).
fn resolve_stale_or_unexplained(
    conn: &Connection,
    p: &Pending,
    file_exists: &dyn Fn(&str) -> bool,
    memo: &mut Memo,
) -> Result<bool> {
    let exists = *memo
        .src_file_exists
        .entry(p.src_file.clone())
        .or_insert_with(|| file_exists(&p.src_file));
    if exists {
        clear_edge(conn, p)?;
        Ok(false)
    } else {
        classify_edge(conn, p, "stale", "file_missing")?;
        Ok(true)
    }
}

/// Definition files for `name` within `project`, deduped (repo_defs may
/// have multiple `kind` rows — function + type — at the same file).
fn repo_candidate_files(conn: &Connection, project: &str, name: &str) -> Result<BTreeSet<String>> {
    let rows = crate::storage::codegraph::lookup_repo_defs(conn, project, name)?;
    Ok(rows.into_iter().map(|(file, _kind)| file).collect())
}

/// Canonicalize a stored file path the same way the code graph does
/// (`extraction::repo_path`), so co-edit weight comparisons against
/// `code_evolution.file_path` (which is written as an absolute path) aren't
/// thrown off by worktree-relative differences. A no-op for paths that don't
/// exist on disk (e.g. synthetic test paths) — falls back to the input.
fn canon_path(s: &str, memo: &mut Memo) -> String {
    if let Some(c) = memo.canon.get(s) {
        return c.clone();
    }
    // The warm spelling, always: the plain lookup spells the first path seen in
    // a directory with no `.git` as given and later ones resolved, so memoizing
    // it would pin whichever came first for every edge that repeats the path,
    // and pairs of paths for one file would compare unequal.
    let c = super::repo_path::canonical_repo_path_warm(Path::new(s))
        .to_string_lossy()
        .into_owned();
    memo.canon.insert(s.to_string(), c.clone());
    c
}

fn basename(s: &str) -> &str {
    Path::new(s)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(s)
}

/// Number of distinct `code_evolution.session_id`s with a row for both `f`
/// and `d` (already-canonicalized absolute paths).
///
/// One grouped query per `f` per pass instead of one self-join per candidate:
/// every file edited in a session that also edited `f`, with its session
/// count. The count for `d` is the same number the old
/// `a.file_path = f AND b.file_path = d AND a.session_id = b.session_id`
/// join returned, and 0 when `d` shares no session with `f`.
fn coedit_weight(conn: &Connection, f: &str, d: &str, memo: &mut Memo) -> Result<i64> {
    if !memo.coedit.contains_key(f) {
        let mut stmt = conn.prepare(
            "SELECT file_path, COUNT(DISTINCT session_id) FROM code_evolution
             WHERE session_id IN (SELECT session_id FROM code_evolution WHERE file_path = ?1)
             GROUP BY file_path",
        )?;
        let rows = stmt.query_map(params![f], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut by_file = BTreeMap::new();
        for r in rows {
            let (file, n) = r?;
            by_file.insert(file, n);
        }
        memo.coedit.insert(f.to_string(), by_file);
    }
    Ok(memo
        .coedit
        .get(f)
        .and_then(|by_file| by_file.get(d))
        .copied()
        .unwrap_or(0))
}

#[cfg(test)]
#[path = "resolver_tests.rs"]
mod tests;
