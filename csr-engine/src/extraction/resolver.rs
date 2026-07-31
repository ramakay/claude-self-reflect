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

    // Pass 1d (WCR truth pass, TASK 2): per (project, file) local-binding
    // name set — the X4 tier's witness table, populated only by
    // `eval::codegraph::backfill_wcr_witnesses` (see
    // `extraction::codegraph::collect_local_bindings`). Empty on every DB
    // this table hasn't been backfilled into — the X4 tier is then silent
    // by construction (an empty/missing `BTreeMap` entry never matches),
    // not an error.
    let mut local_bindings: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT project, file, name FROM local_bindings WHERE (?1 = '' OR project = ?1)",
        )?;
        let rows = stmt.query_map(params![project], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for r in rows {
            let (proj, file, name) = r?;
            local_bindings.entry((proj, file)).or_default().insert(name);
        }
    }

    // Pass 2: collect placeholder edges (calls + imports), deterministically.
    let mut pending: Vec<Pending> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT e.src_id, e.dst_id, e.kind, n.file, n.project, e.callee_kind, e.evidence, e.boundary
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
            ))
        })?;
        for r in rows {
            let (src_id, dst_id, kind, src_file, edge_project, callee_kind, evidence, boundary) =
                r?;
            let name = dst_id.strip_prefix("name:").unwrap_or(&dst_id).to_string();
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
    let mut roots_cache: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();

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
                &mut roots_cache,
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
            &project_roots_for(conn, &p.project, &mut roots_cache),
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
                if resolve_stale_or_unexplained(conn, p, file_exists)? {
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
                    &mut roots_cache,
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
        let f_canon = canon_path(&p.src_file);
        let mut scored: Vec<(&String, i64)> = Vec::with_capacity(distinct.len());
        for (file, id) in &distinct {
            let d_canon = canon_path(file);
            let weight = coedit_weight(conn, &f_canon, &d_canon)?;
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
        } else if resolve_stale_or_unexplained(conn, p, file_exists)? {
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
    roots_cache: &mut BTreeMap<String, Vec<PathBuf>>,
    file_exists: &dyn Fn(&str) -> bool,
    zero_defs_anywhere: bool,
    local_bindings: &BTreeMap<(String, String), BTreeSet<String>>,
) -> Result<Option<&'static str>> {
    if let Some((boundary, evidence)) = builtin_tier(p) {
        // X0: language-defined prelude/builtin/global — runs before X1
        // since these names are never import-evidenced.
        classify_edge(conn, p, &boundary, &evidence)?;
        return Ok(Some("external"));
    }
    if let Some((boundary, evidence)) = qualifier_tier(conn, p)? {
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
    let ns = external_ns_for(conn, &p.src_file, &p.project, roots_cache);
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
        internal_module_tier(p, &project_roots_for(conn, &p.project, roots_cache))
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
    if zero_defs_anywhere {
        if let Some(names) = local_bindings.get(&(p.project.clone(), p.src_file.clone())) {
            if names.contains(&p.name) {
                classify_edge(conn, p, "local", &format!("local_scope:{}", p.name))?;
                return Ok(Some("local"));
            }
        }
    }
    if resolve_stale_or_unexplained(conn, p, file_exists)? {
        return Ok(Some("stale"));
    }
    Ok(None)
}

/// Memoized `repo_scan::project_roots` lookup shared by `external_ns_for`
/// (X1's manifest fallback, WCR Phase 6 TASK E) and `internal_module_tier`
/// (WCR Phase 7 TASK E's alias-module resolution) — both need "this
/// project's known roots" and a resolve pass typically has many pending
/// edges sharing a project, so the DB query only runs once per project.
fn project_roots_for(
    conn: &Connection,
    project: &str,
    roots_cache: &mut BTreeMap<String, Vec<PathBuf>>,
) -> Vec<PathBuf> {
    if !roots_cache.contains_key(project) {
        let roots = repo_scan::project_roots(conn, project).unwrap_or_default();
        roots_cache.insert(project.to_string(), roots);
    }
    roots_cache.get(project).cloned().unwrap_or_default()
}

/// TASK E (WCR Phase 6): resolve the external namespaces reachable from a
/// pending edge's file, falling back to the project's other known roots
/// when the direct single-file ancestor walk finds no manifest at all.
/// `roots_cache` memoizes `repo_scan::project_roots` per project across the
/// whole resolve pass (see `project_roots_for`).
fn external_ns_for(
    conn: &Connection,
    src_file: &str,
    project: &str,
    roots_cache: &mut BTreeMap<String, Vec<PathBuf>>,
) -> manifest::ExternalNs {
    let ns = manifest::external_namespaces(Path::new(src_file));
    if !ns.rust_deps.is_empty() || !ns.js_deps.is_empty() {
        return ns;
    }
    let roots = project_roots_for(conn, project, roots_cache);
    manifest::apply_project_root_fallback(ns, &roots)
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
fn qualifier_tier(conn: &Connection, p: &Pending) -> Result<Option<(String, String)>> {
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
            if repo_has_module_named(conn, &p.project, root)? {
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
                ns.py_stdlib.contains(root) || !repo_has_python_file_named(conn, &p.project, root)?
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
fn repo_has_module_named(conn: &Connection, project: &str, root: &str) -> Result<bool> {
    let mut stmt = conn.prepare(
        "SELECT file FROM repo_defs WHERE project = ?1
         UNION
         SELECT file FROM code_nodes WHERE project = ?1",
    )?;
    let files = stmt.query_map(params![project], |row| row.get::<_, String>(0))?;
    for file in files {
        let file = file?;
        let path = Path::new(&file);
        if path.file_stem().and_then(|s| s.to_str()) == Some(root) {
            return Ok(true);
        }
        let hit = path
            .components()
            .any(|c| matches!(c, std::path::Component::Normal(n) if n.to_str() == Some(root)));
        if hit {
            return Ok(true);
        }
    }
    Ok(false)
}

/// True when `repo_defs` (for `project`) has a Python file whose basename is
/// exactly `<root>.py`. Backs `qualifier_tier`'s Python rule: the absence of
/// such a file is (conservative) positive evidence the qualifier root is not
/// a local module the repo itself defines.
fn repo_has_python_file_named(conn: &Connection, project: &str, root: &str) -> Result<bool> {
    let target = format!("{root}.py");
    let mut stmt =
        conn.prepare("SELECT DISTINCT file FROM repo_defs WHERE project = ?1 AND lang = 'python'")?;
    let files = stmt.query_map(params![project], |row| row.get::<_, String>(0))?;
    for file in files {
        let file = file?;
        if Path::new(&file).file_name().and_then(|n| n.to_str()) == Some(target.as_str()) {
            return Ok(true);
        }
    }
    Ok(false)
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
    let resolved_canon = canon_path(&resolved.to_string_lossy());
    let evidence = format!("module_bind:{}", basename(&resolved.to_string_lossy()));

    if let Some(defs) = defs {
        for (file, id) in defs {
            if canon_path(file) == resolved_canon {
                return Ok(Some((id.clone(), evidence)));
            }
        }
    }

    for (file, _kind) in crate::storage::codegraph::lookup_repo_defs(conn, &p.project, &p.name)? {
        if canon_path(&file) == resolved_canon {
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

/// Bind tier: repoint (or, for repo_scan evidence, leave in place) `dst_id`
/// and flip `resolved`. `UPDATE OR REPLACE` tolerates the PK collision when
/// two different placeholder names resolve onto the same real target id.
/// Always clears `boundary` — a bound edge is not a classified boundary edge,
/// even if a prior pass had classified this exact placeholder before a later
/// def appeared for it.
fn bind(conn: &Connection, p: &Pending, new_dst: &str, evidence: &str) -> Result<()> {
    conn.execute(
        "UPDATE OR REPLACE code_edges SET dst_id = ?1, resolved = 1, boundary = '', evidence = ?2
         WHERE src_id = ?3 AND dst_id = ?4 AND kind = ?5",
        params![new_dst, evidence, p.src_id, p.dst_id, p.kind],
    )?;
    Ok(())
}

/// Classify tier: leave `resolved = 0`, attach `boundary` + `evidence`.
fn classify_edge(conn: &Connection, p: &Pending, boundary: &str, evidence: &str) -> Result<()> {
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
) -> Result<bool> {
    if file_exists(&p.src_file) {
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
fn canon_path(s: &str) -> String {
    super::repo_path::canonical_repo_path(Path::new(s))
        .to_string_lossy()
        .into_owned()
}

fn basename(s: &str) -> &str {
    Path::new(s)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(s)
}

/// Number of distinct `code_evolution.session_id`s with a row for both `f`
/// and `d` (already-canonicalized absolute paths).
fn coedit_weight(conn: &Connection, f: &str, d: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(DISTINCT a.session_id) FROM code_evolution a
         JOIN code_evolution b ON a.session_id = b.session_id
         WHERE a.file_path = ?1 AND b.file_path = ?2",
        params![f, d],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::codegraph::{self, upsert_node, upsert_repo_defs, EdgeRow, NodeRow};
    use crate::storage::migrations;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        conn
    }

    fn def(id: &str, file: &str, name: &str) -> NodeRow {
        NodeRow {
            id: id.into(),
            repo: "r".into(),
            project: "proj".into(),
            file: file.into(),
            lang: "rust".into(),
            kind: "function".into(),
            name: name.into(),
            first_conv_id: "c".into(),
            last_conv_id: "c".into(),
            ..NodeRow::default()
        }
    }

    fn module_node(id: &str, file: &str) -> NodeRow {
        NodeRow {
            id: id.into(),
            repo: "r".into(),
            project: "proj".into(),
            file: file.into(),
            lang: "rust".into(),
            kind: "module".into(),
            name: file.into(),
            first_conv_id: "c".into(),
            last_conv_id: "c".into(),
            ..NodeRow::default()
        }
    }

    fn call_edge(src: &str, name: &str, file: &str) -> EdgeRow {
        EdgeRow {
            src_id: src.into(),
            dst_id: format!("name:{name}"),
            kind: "calls".into(),
            src_file: file.into(),
            resolved: 0,
            weight: 1.0,
            ..EdgeRow::default()
        }
    }

    fn import_edge(src: &str, name: &str, file: &str) -> EdgeRow {
        EdgeRow {
            src_id: src.into(),
            dst_id: format!("name:{name}"),
            kind: "imports".into(),
            src_file: file.into(),
            resolved: 0,
            weight: 1.0,
            ..EdgeRow::default()
        }
    }

    /// An `imports` edge carrying `from:<module>` evidence, as extraction
    /// (Phase 5 TASK 1) now writes it — feeds the X1 module-aware tier.
    fn import_edge_from(src: &str, name: &str, file: &str, module: &str) -> EdgeRow {
        EdgeRow {
            evidence: format!("from:{module}"),
            ..import_edge(src, name, file)
        }
    }

    /// A `calls` edge carrying `via:<qualifier>` evidence, as extraction
    /// (Phase 6 TASK A) now writes it — feeds the X1b qualifier tier.
    fn call_edge_via(src: &str, name: &str, file: &str, qualifier: &str) -> EdgeRow {
        EdgeRow {
            evidence: format!("via:{qualifier}"),
            ..call_edge(src, name, file)
        }
    }

    fn insert_evolution(conn: &Connection, session_id: &str, file_path: &str) {
        conn.execute(
            "INSERT INTO code_evolution (id, session_id, file_path) VALUES (?1, ?2, ?3)",
            params![format!("{session_id}:{file_path}"), session_id, file_path],
        )
        .unwrap();
    }

    #[test]
    fn resolves_same_file_def() {
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        upsert_node(&conn, &def("bar", "a.rs", "bar")).unwrap();
        codegraph::replace_file_edges(&conn, "proj", "a.rs", &[call_edge("foo", "bar", "a.rs")])
            .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.total, 1);
        assert_eq!(stats.resolved, 1);
        assert_eq!(stats.unexplained, 0);

        let callees = codegraph::query_callees(&conn, "foo", 10).unwrap();
        assert!(callees.iter().any(|n| n.id == "bar"), "foo -> bar resolved");
    }

    #[test]
    fn ambiguous_stays_unresolved() {
        let conn = mem();
        // Two defs named `bar` in different files; caller in a third file,
        // no import edge, no code_evolution — nothing to disambiguate with.
        upsert_node(&conn, &def("bar_x", "x.rs", "bar")).unwrap();
        upsert_node(&conn, &def("bar_y", "y.rs", "bar")).unwrap();
        upsert_node(&conn, &def("foo", "z.rs", "foo")).unwrap();
        codegraph::replace_file_edges(&conn, "proj", "z.rs", &[call_edge("foo", "bar", "z.rs")])
            .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.total, 1);
        assert_eq!(stats.resolved, 0, "ambiguous must not be guessed");
        assert_eq!(stats.ambiguous_remaining, 1);

        // Edge remains a placeholder.
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM code_edges WHERE resolved = 0 AND dst_id = 'name:bar'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 1);
    }

    #[test]
    fn project_unique_resolves_cross_file() {
        let conn = mem();
        upsert_node(&conn, &def("bar", "x.rs", "bar")).unwrap();
        upsert_node(&conn, &def("foo", "z.rs", "foo")).unwrap();
        codegraph::replace_file_edges(&conn, "proj", "z.rs", &[call_edge("foo", "bar", "z.rs")])
            .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.resolved, 1, "unique cross-file name resolves");

        let evidence: String = conn
            .query_row(
                "SELECT evidence FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(evidence, "unique_def");
    }

    #[test]
    fn import_bound_picks_the_imported_def_over_ambiguity() {
        let conn = mem();
        // "bar" is ambiguous in code_nodes (x.rs and y.rs both define it),
        // but repo_defs (whole-repo scan) knows the real one lives in y.rs,
        // and z.rs has a textual `import bar` — that combination must win
        // over the plain ambiguity that `unique_def`/`coedit` alone would
        // leave unresolved.
        upsert_node(&conn, &def("bar_x", "x.rs", "bar")).unwrap();
        upsert_node(&conn, &def("bar_y", "y.rs", "bar")).unwrap();
        upsert_node(&conn, &def("foo", "z.rs", "foo")).unwrap();
        upsert_node(&conn, &module_node("z_mod", "z.rs")).unwrap();
        upsert_repo_defs(
            &conn,
            "proj",
            "y.rs",
            &[(
                "bar".to_string(),
                "function".to_string(),
                "rust".to_string(),
            )],
        )
        .unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "z.rs",
            &[
                import_edge("z_mod", "bar", "z.rs"),
                call_edge("foo", "bar", "z.rs"),
            ],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            stats.total, 2,
            "the imports edge and the calls edge both target bar"
        );
        assert_eq!(stats.resolved, 2);

        let (dst, evidence): (String, String) = conn
            .query_row(
                "SELECT dst_id, evidence FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            dst, "bar_y",
            "picks the repo-defs-confirmed import target, not x.rs"
        );
        assert_eq!(evidence, "import_bound");
    }

    #[test]
    fn coedit_binds_on_strict_margin_and_refuses_without_it() {
        let conn = mem();
        // "bar": binds — x.rs shares 2 sessions with z.rs, y.rs shares 0.
        // 2 >= 2 and 2 >= 2*0 -> binds.
        upsert_node(&conn, &def("bar_x", "x.rs", "bar")).unwrap();
        upsert_node(&conn, &def("bar_y", "y.rs", "bar")).unwrap();
        upsert_node(&conn, &def("foo", "z.rs", "foo")).unwrap();
        codegraph::replace_file_edges(&conn, "proj", "z.rs", &[call_edge("foo", "bar", "z.rs")])
            .unwrap();
        for s in ["s1", "s2"] {
            insert_evolution(&conn, s, "z.rs");
            insert_evolution(&conn, s, "x.rs");
        }

        // "baz": refuses — x2.rs shares 3 sessions with z2.rs, y2.rs shares 2.
        // 3 >= 2 but 3 >= 2*2=4 is false -> must not guess.
        upsert_node(&conn, &def("baz_x", "x2.rs", "baz")).unwrap();
        upsert_node(&conn, &def("baz_y", "y2.rs", "baz")).unwrap();
        upsert_node(&conn, &def("foo2", "z2.rs", "foo2")).unwrap();
        codegraph::replace_file_edges(&conn, "proj", "z2.rs", &[call_edge("foo2", "baz", "z2.rs")])
            .unwrap();
        for s in ["t1", "t2", "t3"] {
            insert_evolution(&conn, s, "z2.rs");
            insert_evolution(&conn, s, "x2.rs");
        }
        for s in ["t1", "t2"] {
            insert_evolution(&conn, s, "y2.rs");
        }

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();

        let (bar_dst, bar_evidence): (String, String) = conn
            .query_row(
                "SELECT dst_id, evidence FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            bar_dst, "bar_x",
            "2-vs-0 margin binds to the co-edited file"
        );
        assert_eq!(bar_evidence, "coedit:2");

        let (baz_resolved, baz_dst): (i64, String) = conn
            .query_row(
                "SELECT resolved, dst_id FROM code_edges WHERE src_id = 'foo2' AND kind = 'calls'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(baz_resolved, 0, "3-vs-2 margin (< 2x) must not guess");
        assert_eq!(baz_dst, "name:baz");

        assert_eq!(stats.ambiguous_remaining, 1, "only baz stays ambiguous");
    }

    #[test]
    fn x1_rust_std_import_classified_external() {
        let conn = mem();
        // `use std;` — the bound symbol IS the literal namespace segment.
        upsert_node(&conn, &module_node("a_mod", "a.rs")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "a.rs",
            &[import_edge("a_mod", "std", "a.rs")],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.external, 1);
        assert_eq!(stats.resolved, 0);

        let (boundary, evidence, resolved): (String, String, i64) = conn
            .query_row(
                "SELECT boundary, evidence, resolved FROM code_edges WHERE src_id = 'a_mod' AND kind = 'imports'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(boundary, "external");
        assert_eq!(evidence, "import:std");
        assert_eq!(resolved, 0);
    }

    #[test]
    fn x2_method_call_with_no_def_classified() {
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        let mut e = call_edge("foo", "push", "a.rs");
        e.callee_kind = "method".into();
        codegraph::replace_file_edges(&conn, "proj", "a.rs", &[e]).unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.method, 1);
        assert_eq!(stats.resolved, 0);

        let (boundary, evidence, resolved): (String, String, i64) = conn
            .query_row(
                "SELECT boundary, evidence, resolved FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(boundary, "method");
        assert_eq!(evidence, "receiver_call");
        assert_eq!(resolved, 0);
    }

    // ─── Finding 2: receiver-aware method-call bind gating (WCR truth pass) ───

    #[test]
    fn method_call_with_unrelated_receiver_does_not_bind_to_same_file_name_collision() {
        // `receiver.save()` — a same-file, same-named `save` def exists,
        // but it's an UNRELATED free function, not evidence about what
        // `receiver` actually is. Before Finding 2, B0 bound purely on
        // bare-name identity, corrupting provenance while flattering the
        // internal-binding metric.
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        upsert_node(&conn, &def("save_a", "a.rs", "save")).unwrap();
        let mut e = call_edge_via("foo", "save", "a.rs", "receiver");
        e.callee_kind = "method".into();
        codegraph::replace_file_edges(&conn, "proj", "a.rs", &[e]).unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            stats.resolved, 0,
            "must not bind to the unrelated same-file `save`"
        );
        assert_eq!(stats.method, 1);

        let (boundary, evidence, resolved, dst_id): (String, String, i64, String) = conn
            .query_row(
                "SELECT boundary, evidence, resolved, dst_id FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(boundary, "method");
        assert_eq!(evidence, "receiver_call");
        assert_eq!(resolved, 0);
        assert_eq!(
            dst_id, "name:save",
            "dst_id stays the placeholder — never bound"
        );
    }

    #[test]
    fn method_call_with_self_receiver_binds_via_b0_same_file() {
        // `self.save()` — an instance method calling its own class's
        // method. The self/this/cls receiver IS consistent with B0's
        // same-file assumption, so this must still bind.
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        upsert_node(&conn, &def("save_a", "a.rs", "save")).unwrap();
        let mut e = call_edge_via("foo", "save", "a.rs", "self");
        e.callee_kind = "method".into();
        codegraph::replace_file_edges(&conn, "proj", "a.rs", &[e]).unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            stats.resolved, 1,
            "self.save() binds to the same-file `save` def"
        );
        assert_eq!(stats.method, 0);

        let (dst_id, resolved, evidence): (String, i64, String) = conn
            .query_row(
                "SELECT dst_id, resolved, evidence FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(dst_id, "save_a");
        assert_eq!(resolved, 1);
        assert_eq!(evidence, "same_file");
    }

    #[test]
    fn method_call_with_non_self_receiver_does_not_bind_via_unique_def_elsewhere() {
        // `obj.save()` — `save` has exactly ONE `code_nodes` def
        // project-wide, but it's in a DIFFERENT file. B2 (`unique_def`) is
        // a cross-file name-only bind tier; `obj` is not self/this/cls, so
        // it must not reach B2 at all, classifying `method` instead.
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        upsert_node(&conn, &def("save_b", "b.rs", "save")).unwrap();
        let mut e = call_edge_via("foo", "save", "a.rs", "obj");
        e.callee_kind = "method".into();
        codegraph::replace_file_edges(&conn, "proj", "a.rs", &[e]).unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            stats.resolved, 0,
            "must not bind via B2 unique_def — obj isn't self/this/cls"
        );
        assert_eq!(stats.method, 1);

        let (boundary, evidence, dst_id): (String, String, String) = conn
            .query_row(
                "SELECT boundary, evidence, dst_id FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(boundary, "method");
        assert_eq!(evidence, "receiver_call");
        assert_eq!(dst_id, "name:save");
    }

    #[test]
    fn method_call_with_no_via_evidence_does_not_bind_to_same_file_name_collision() {
        // Simulates a Rust `field_expression` receiver call
        // (`r.map_err(handler)`), which never captures `via:` qualifier
        // evidence at all (see `extraction::codegraph::call_qualifier`).
        // `method_bind_gate` treats "no via: data" the same as "receiver
        // isn't self/this/cls": skip every bind tier, including B0, even
        // though a same-file same-named def exists.
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        upsert_node(&conn, &def("handler_a", "a.rs", "handler")).unwrap();
        let mut e = call_edge("foo", "handler", "a.rs"); // no `via:` evidence
        e.callee_kind = "method".into();
        codegraph::replace_file_edges(&conn, "proj", "a.rs", &[e]).unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            stats.resolved, 0,
            "no via: evidence -> NoBind, even with a same-file name collision"
        );
        assert_eq!(stats.method, 1);
    }

    #[test]
    fn unexplained_residual_is_counted() {
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        // No def anywhere, no import edge, plain (non-method) callee_kind —
        // nothing binds or classifies it.
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "a.rs",
            &[call_edge("foo", "ghost_symbol_xyz", "a.rs")],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.total, 1);
        assert_eq!(stats.resolved, 0);
        assert_eq!(stats.external, 0);
        assert_eq!(stats.method, 0);
        assert_eq!(stats.unexplained, 1);
        assert_eq!(stats.ambiguous_remaining, 0);
    }

    #[test]
    fn resolve_is_deterministic_across_repeated_runs() {
        let conn = mem();
        upsert_node(&conn, &def("bar_x", "x.rs", "bar")).unwrap();
        upsert_node(&conn, &def("bar_y", "y.rs", "bar")).unwrap();
        upsert_node(&conn, &def("foo", "z.rs", "foo")).unwrap();
        codegraph::replace_file_edges(&conn, "proj", "z.rs", &[call_edge("foo", "bar", "z.rs")])
            .unwrap();

        let first = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        let second = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(first, second, "repeated passes over unchanged data agree");
    }

    // ─── X0 builtin/prelude/global tier (WCR Phase 5, TASK 3) ───

    #[test]
    fn x0_builtin_classifies_per_language() {
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        codegraph::replace_file_edges(&conn, "proj", "a.rs", &[call_edge("foo", "Ok", "a.rs")])
            .unwrap();
        upsert_node(&conn, &def("run", "b.ts", "run")).unwrap();
        codegraph::replace_file_edges(&conn, "proj", "b.ts", &[call_edge("run", "fetch", "b.ts")])
            .unwrap();
        upsert_node(&conn, &def("main_py", "c.py", "main_py")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "c.py",
            &[call_edge("main_py", "print", "c.py")],
        )
        .unwrap();
        upsert_node(&conn, &def("main_go", "d.go", "main_go")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "d.go",
            &[call_edge("main_go", "make", "d.go")],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.total, 4);
        assert_eq!(stats.resolved, 0);
        assert_eq!(
            stats.external, 4,
            "Ok/fetch/print/make all classify external via X0"
        );

        for (src, dst, expected_evidence) in [
            ("foo", "name:Ok", "builtin:rust"),
            ("run", "name:fetch", "builtin:js"),
            ("main_py", "name:print", "builtin:python"),
            ("main_go", "name:make", "builtin:go"),
        ] {
            let (boundary, evidence): (String, String) = conn
                .query_row(
                    "SELECT boundary, evidence FROM code_edges WHERE src_id = ?1 AND dst_id = ?2",
                    params![src, dst],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(boundary, "external", "{src}/{dst}");
            assert_eq!(evidence, expected_evidence, "{src}/{dst}");
        }
    }

    #[test]
    fn x0_builtin_does_not_fire_when_a_repo_def_exists() {
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        codegraph::replace_file_edges(&conn, "proj", "a.rs", &[call_edge("foo", "Ok", "a.rs")])
            .unwrap();
        // A whole-repo scan found a (shadowing, user-defined) `Ok` def — X0
        // must defer to the repo_scan bind tier, never fire alongside it.
        upsert_repo_defs(
            &conn,
            "proj",
            "custom_ok.rs",
            &[("Ok".to_string(), "function".to_string(), "rust".to_string())],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            stats.resolved, 1,
            "single repo_defs candidate binds via repo_scan, not X0"
        );
        assert_eq!(stats.external, 0, "X0 must not fire when a repo def exists");

        let (boundary, evidence): (String, String) = conn
            .query_row(
                "SELECT boundary, evidence FROM code_edges WHERE src_id = 'foo' AND dst_id = 'name:Ok'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(boundary, "");
        assert!(evidence.starts_with("repo_scan:"), "{evidence}");
    }

    // ─── Module-aware X1 tier (WCR Phase 5, TASK 4) ───

    #[test]
    fn x1_module_aware_relative_import_is_never_external() {
        let conn = mem();
        upsert_node(&conn, &module_node("a_mod", "a.ts")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "a.ts",
            &[import_edge_from("a_mod", "helper", "a.ts", "./util")],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            stats.external, 0,
            "relative module specifier must never classify external"
        );
        assert_eq!(stats.unexplained, 1);
    }

    #[test]
    fn x1_module_aware_bare_node_prefixed_module_matches_builtin() {
        let conn = mem();
        upsert_node(&conn, &module_node("a_mod", "a.ts")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "a.ts",
            &[import_edge_from("a_mod", "fs", "a.ts", "node:fs")],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.external, 1);
        let (boundary, evidence): (String, String) = conn
            .query_row(
                "SELECT boundary, evidence FROM code_edges WHERE src_id = 'a_mod' AND kind = 'imports'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(boundary, "external");
        assert_eq!(evidence, "import:node:fs");
    }

    #[test]
    fn x1_module_aware_calls_edge_uses_sibling_import_module() {
        let conn = mem();
        upsert_node(&conn, &module_node("a_mod", "a.ts")).unwrap();
        upsert_node(&conn, &def("foo", "a.ts", "foo")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "a.ts",
            &[
                import_edge_from("a_mod", "fs", "a.ts", "node:fs"),
                call_edge("foo", "fs", "a.ts"),
            ],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.total, 2);
        assert_eq!(
            stats.external, 2,
            "both the imports edge and the sibling calls edge classify external"
        );

        let (boundary, evidence): (String, String) = conn
            .query_row(
                "SELECT boundary, evidence FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            boundary, "external",
            "calls edge inherits the sibling imports edge's module evidence"
        );
        assert_eq!(evidence, "import:node:fs");
    }

    #[test]
    fn x1_module_present_but_unmatched_suppresses_degraded_name_fallback() {
        let conn = mem();
        upsert_node(&conn, &module_node("a_mod", "a.rs")).unwrap();
        // Bound symbol name "std" WOULD match the degraded (name-only) path,
        // but the module evidence does not match anything — once module data
        // is present it fully replaces the degraded match, it is not tried
        // as a fallback on a module-classification miss.
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "a.rs",
            &[import_edge_from(
                "a_mod",
                "std",
                "a.rs",
                "zzz_definitely_not_a_real_module_zzz",
            )],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            stats.external, 0,
            "module data present but unmatched must not fall back to degraded name match"
        );
        assert_eq!(stats.unexplained, 1);
    }

    #[test]
    fn resolve_is_deterministic_with_x0_and_module_aware_x1_tiers() {
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        codegraph::replace_file_edges(&conn, "proj", "a.rs", &[call_edge("foo", "Ok", "a.rs")])
            .unwrap();
        upsert_node(&conn, &module_node("b_mod", "b.ts")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "b.ts",
            &[import_edge_from("b_mod", "fs", "b.ts", "node:fs")],
        )
        .unwrap();

        let first = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        let second = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            first, second,
            "X0 + module-aware X1 tiers stay deterministic across repeated passes"
        );
        assert_eq!(first.external, 2);
    }

    // ─── X1b qualifier-aware tier (WCR Phase 6, TASK B) ───

    #[test]
    fn qualifier_tier_rust_path_call_classifies_external_via_stdlib_namespace() {
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "a.rs",
            &[call_edge_via("foo", "now", "a.rs", "std::time::Instant")],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            stats.external, 1,
            "std::time::Instant::now() classifies external via qualifier root"
        );
        assert_eq!(stats.resolved, 0);

        let (boundary, evidence): (String, String) = conn
            .query_row(
                "SELECT boundary, evidence FROM code_edges WHERE src_id = 'foo' AND dst_id = 'name:now'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(boundary, "external");
        assert_eq!(evidence, "import:std::time::Instant");
    }

    #[test]
    fn qualifier_tier_rust_refuses_when_root_matches_a_repo_module_name() {
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        // The repo happens to have its own module literally named `std.rs` —
        // the guard must refuse to classify `std::helper()` external just
        // because "std" also matches the Rust builtin namespace.
        upsert_node(&conn, &def("marker", "std.rs", "marker")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "a.rs",
            &[call_edge_via("foo", "helper", "a.rs", "std")],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            stats.external, 0,
            "repo-module-name collision must suppress qualifier classification"
        );
        assert_eq!(stats.unexplained, 1);
    }

    #[test]
    fn qualifier_tier_python_module_call_classifies_external_via_stdlib() {
        let conn = mem();
        upsert_node(&conn, &def("handler", "a.py", "handler")).unwrap();
        // Realistic: Python's `attribute` AST node makes `json.dumps()`
        // callee_kind = "method" same as `self.run()` — the qualifier tier
        // must take priority over X2 so this classifies `external`, not the
        // vaguer `method`/receiver_call bucket.
        let mut e = call_edge_via("handler", "dumps", "a.py", "json");
        e.callee_kind = "method".to_string();
        codegraph::replace_file_edges(&conn, "proj", "a.py", &[e]).unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            stats.external, 1,
            "json.dumps() classifies external via qualifier, ahead of X2 method"
        );
        assert_eq!(stats.method, 0);

        let (boundary, evidence): (String, String) = conn
            .query_row(
                "SELECT boundary, evidence FROM code_edges WHERE src_id = 'handler' AND dst_id = 'name:dumps'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(boundary, "external");
        assert_eq!(evidence, "import:json");
    }

    #[test]
    fn qualifier_tier_python_self_receiver_never_classifies_external() {
        let conn = mem();
        upsert_node(&conn, &def("handler", "a.py", "handler")).unwrap();
        // Realistic callee_kind: Python's `attribute` node can't
        // syntactically distinguish `self.run()` from `json.dumps()`, both
        // are "method". `self` must fall through to the generic X2 tier
        // instead of being misclassified `external`.
        let mut e = call_edge_via("handler", "run", "a.py", "self");
        e.callee_kind = "method".to_string();
        codegraph::replace_file_edges(&conn, "proj", "a.py", &[e]).unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            stats.external, 0,
            "self.run() must never classify external — self is a receiver, not a module"
        );
        assert_eq!(
            stats.method, 1,
            "falls through to the generic X2 method tier instead"
        );
        assert_eq!(stats.unexplained, 0);
    }

    #[test]
    fn qualifier_tier_python_refuses_when_repo_owns_the_module_file() {
        let conn = mem();
        upsert_node(&conn, &def("handler", "a.py", "handler")).unwrap();
        upsert_repo_defs(
            &conn,
            "proj",
            "myutil.py",
            &[(
                "real_fn".to_string(),
                "function".to_string(),
                "python".to_string(),
            )],
        )
        .unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "a.py",
            &[call_edge_via("handler", "helper", "a.py", "myutil")],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            stats.external, 0,
            "myutil.py exists in this repo — must not classify external"
        );
        assert_eq!(stats.unexplained, 1);
    }

    #[test]
    fn qualifier_tier_ignores_non_calls_edges() {
        let conn = mem();
        upsert_node(&conn, &module_node("a_mod", "a.py")).unwrap();
        // `via:` evidence only exists on `calls` edges (TASK A); an imports
        // edge that somehow carried one (shouldn't happen) must not trip
        // X1b. Bound symbol name is deliberately NOT a stdlib name itself
        // (unlike the qualifier), isolating this from the pre-existing
        // degraded X1 name-only match — this test is purely about
        // `qualifier_tier`'s own `p.kind != "calls"` guard.
        let mut e = import_edge_from("a_mod", "totally_unknown_symbol_xyz", "a.py", "os");
        e.evidence = "via:os".to_string();
        codegraph::replace_file_edges(&conn, "proj", "a.py", &[e]).unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.external, 0);
        assert_eq!(stats.unexplained, 1);
    }

    // ─── Stale-file tier (WCR Phase 6, TASK C) ───

    #[test]
    fn stale_tier_classifies_when_src_file_is_missing_on_disk() {
        const GONE: &str = "/definitely/does/not/exist/on/disk/a.rs";
        let conn = mem();
        upsert_node(&conn, &def("foo", GONE, "foo")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            GONE,
            &[call_edge("foo", "ghost_symbol_xyz", GONE)],
        )
        .unwrap();

        let stats =
            resolve_edges(&conn, "proj", &|f: &str| std::path::Path::new(f).is_file()).unwrap();
        assert_eq!(stats.total, 1);
        assert_eq!(
            stats.stale, 1,
            "missing file must classify stale, not unexplained"
        );
        assert_eq!(stats.unexplained, 0);
        assert_eq!(stats.resolved, 0);
        assert_eq!(stats.external, 0);
        assert_eq!(stats.method, 0);
        assert_eq!(
            stats.closure_rate, 1.0,
            "stale counts toward closure like external/method"
        );

        let (boundary, evidence, resolved): (String, String, i64) = conn
            .query_row(
                "SELECT boundary, evidence, resolved FROM code_edges
                 WHERE src_id = 'foo' AND dst_id = 'name:ghost_symbol_xyz'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(boundary, "stale");
        assert_eq!(evidence, "file_missing");
        assert_eq!(
            resolved, 0,
            "stale never binds — dst_id stays a placeholder"
        );
    }

    #[test]
    fn stale_tier_does_not_fire_when_file_exists_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("a.rs");
        std::fs::write(&file_path, "fn foo() {}\n").unwrap();
        let file_str = file_path.to_string_lossy().to_string();

        let conn = mem();
        upsert_node(&conn, &def("foo", &file_str, "foo")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            &file_str,
            &[call_edge("foo", "ghost_symbol_xyz", &file_str)],
        )
        .unwrap();

        let stats =
            resolve_edges(&conn, "proj", &|f: &str| std::path::Path::new(f).is_file()).unwrap();
        assert_eq!(stats.stale, 0, "existing file must not classify stale");
        assert_eq!(stats.unexplained, 1);
    }

    #[test]
    fn stale_tier_reclassifies_a_would_be_ambiguous_edge_when_file_missing() {
        const GONE: &str = "/definitely/does/not/exist/on/disk/z.rs";
        let conn = mem();
        // Two defs named `bar` in different (existing-path) files; caller's
        // own file is missing — the file-existence check runs before the
        // ambiguity is even considered, so it must win.
        upsert_node(&conn, &def("bar_x", "x.rs", "bar")).unwrap();
        upsert_node(&conn, &def("bar_y", "y.rs", "bar")).unwrap();
        upsert_node(&conn, &def("foo", GONE, "foo")).unwrap();
        codegraph::replace_file_edges(&conn, "proj", GONE, &[call_edge("foo", "bar", GONE)])
            .unwrap();

        let stats =
            resolve_edges(&conn, "proj", &|f: &str| std::path::Path::new(f).is_file()).unwrap();
        assert_eq!(
            stats.stale, 1,
            "would-be-ambiguous edge in a missing file classifies stale instead"
        );
        assert_eq!(stats.ambiguous_remaining, 0);
    }

    #[test]
    fn stale_tier_never_preempts_a_successful_bind() {
        // B0 same-file bind must still fire even when the file is reported
        // missing — staleness only applies to the leftover unexplained
        // remainder, never to edges that already bound to a real def.
        const GONE: &str = "/definitely/does/not/exist/on/disk/a.rs";
        let conn = mem();
        upsert_node(&conn, &def("foo", GONE, "foo")).unwrap();
        upsert_node(&conn, &def("bar", GONE, "bar")).unwrap();
        codegraph::replace_file_edges(&conn, "proj", GONE, &[call_edge("foo", "bar", GONE)])
            .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| false).unwrap();
        assert_eq!(stats.resolved, 1, "same-file bind unaffected by staleness");
        assert_eq!(stats.stale, 0);
    }

    #[test]
    fn resolve_is_deterministic_with_stale_and_qualifier_tiers() {
        const GONE: &str = "/definitely/does/not/exist/on/disk/a.rs";
        let conn = mem();
        upsert_node(&conn, &def("foo", GONE, "foo")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            GONE,
            &[
                call_edge("foo", "ghost_symbol_xyz", GONE),
                call_edge_via("foo", "now", GONE, "std::time::Instant"),
            ],
        )
        .unwrap();

        let checker: &dyn Fn(&str) -> bool = &|f: &str| std::path::Path::new(f).is_file();
        let first = resolve_edges(&conn, "proj", checker).unwrap();
        let second = resolve_edges(&conn, "proj", checker).unwrap();
        assert_eq!(first, second, "stale + qualifier tiers stay deterministic");
        assert_eq!(first.stale, 1);
        assert_eq!(first.external, 1);
    }

    // ─── Qualifier -> import two-hop chain (WCR Phase 7, TASK A) ───

    #[test]
    fn qualifier_import_tier_rust_binds_via_sibling_import_module() {
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        upsert_node(&conn, &module_node("a_mod", "a.rs")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "a.rs",
            &[
                import_edge_from("a_mod", "Instant", "a.rs", "std::time"),
                call_edge_via("foo", "now", "a.rs", "Instant"),
            ],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            stats.external, 2,
            "both the Instant import edge and the now() call classify external: {stats:?}"
        );

        let evidence: String = conn
            .query_row(
                "SELECT evidence FROM code_edges WHERE src_id = 'foo' AND dst_id = 'name:now'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            evidence, "import:std::time::Instant",
            "reconstructs the fully-qualified path the `use` statement elided"
        );
    }

    #[test]
    fn qualifier_import_tier_rust_bare_use_module_binding() {
        // `use std::env;` then `env::temp_dir()` — the qualifier root `env`
        // is not itself a builtin/dep name, but the sibling import edge for
        // `env` carries `from:std` evidence. (Deliberately NOT `use
        // std::fs;` + `fs::...` — `fs` coincidentally collides with
        // `manifest::NODE_BUILTINS`, so `qualifier_tier`'s own direct check
        // already classifies it without ever reaching this two-hop tier;
        // `env` has no such cross-language collision.)
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        upsert_node(&conn, &module_node("a_mod", "a.rs")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "a.rs",
            &[
                import_edge_from("a_mod", "env", "a.rs", "std"),
                call_edge_via("foo", "temp_dir", "a.rs", "env"),
            ],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.external, 2, "{stats:?}");

        let evidence: String = conn
            .query_row(
                "SELECT evidence FROM code_edges WHERE src_id = 'foo' AND dst_id = 'name:temp_dir'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(evidence, "import:std::env");
    }

    #[test]
    fn qualifier_import_tier_requires_a_same_file_import_with_module_evidence() {
        // No import edge for `Instant` in this file at all — must not guess.
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "a.rs",
            &[call_edge_via("foo", "now", "a.rs", "Instant")],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.external, 0, "{stats:?}");
        assert_eq!(stats.unexplained, 1);
    }

    #[test]
    fn qualifier_import_tier_does_not_fire_for_javascript() {
        // Deliberately out of scope (see qualifier_import_tier's doc
        // comment on replay-safety) — a JS/TS `via:` qualifier backed by a
        // same-file import must not be two-hop-joined.
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.ts", "foo")).unwrap();
        upsert_node(&conn, &module_node("a_mod", "a.ts")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "a.ts",
            &[
                import_edge_from("a_mod", "obj", "a.ts", "some-real-package"),
                call_edge_via("foo", "helper_call", "a.ts", "obj"),
            ],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            stats.external, 0,
            "JS/TS is out of scope for the two-hop chain: {stats:?}"
        );
    }

    #[test]
    fn resolve_is_deterministic_with_qualifier_import_tier() {
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        upsert_node(&conn, &module_node("a_mod", "a.rs")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "a.rs",
            &[
                import_edge_from("a_mod", "env", "a.rs", "std"),
                call_edge_via("foo", "temp_dir", "a.rs", "env"),
            ],
        )
        .unwrap();

        let first = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        let second = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            first, second,
            "qualifier-import two-hop tier stays deterministic across repeated passes"
        );
        assert_eq!(first.external, 2);

        let evidence: String = conn
            .query_row(
                "SELECT evidence FROM code_edges WHERE src_id = 'foo' AND dst_id = 'name:temp_dir'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            evidence, "import:std::env",
            "evidence must stay byte-identical across the repeated pass"
        );
    }

    // ─── Installed-package witness (WCR Phase 7, TASK B) ───

    #[test]
    fn installed_package_witness_classifies_external_via_node_modules_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("app");
        let src_dir = root.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        // `@expo/vector-icons` is deliberately NOT declared — only `expo`
        // is, matching the real-world "Feather -> @expo/vector-icons"
        // transitive/bundled dependency case.
        std::fs::write(
            root.join("package.json"),
            r#"{"dependencies": {"expo": "1.0.0"}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("node_modules/@expo/vector-icons")).unwrap();
        let src_file = src_dir.join("Icon.tsx");
        std::fs::write(&src_file, "// icon\n").unwrap();
        let src_file_str = src_file.to_string_lossy().to_string();

        let conn = mem();
        upsert_node(&conn, &module_node("a_mod", &src_file_str)).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            &src_file_str,
            &[import_edge_from(
                "a_mod",
                "Feather",
                &src_file_str,
                "@expo/vector-icons",
            )],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            stats.external, 1,
            "transitive/bundled dep must classify external via node_modules witness: {stats:?}"
        );

        let (boundary, evidence): (String, String) = conn
            .query_row(
                "SELECT boundary, evidence FROM code_edges WHERE src_id = 'a_mod' AND kind = 'imports'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(boundary, "external");
        assert_eq!(evidence, "installed:@expo/vector-icons");
    }

    #[test]
    fn installed_package_witness_does_not_fire_when_package_is_absent_everywhere() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("app");
        let src_dir = root.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(root.join("package.json"), "{}").unwrap();
        let src_file = src_dir.join("a.ts");
        std::fs::write(&src_file, "// a\n").unwrap();
        let src_file_str = src_file.to_string_lossy().to_string();

        let conn = mem();
        upsert_node(&conn, &module_node("a_mod", &src_file_str)).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            &src_file_str,
            &[import_edge_from(
                "a_mod",
                "Thing",
                &src_file_str,
                "totally-not-installed-anywhere",
            )],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.external, 0, "{stats:?}");
        assert_eq!(stats.unexplained, 1);
    }

    // ─── Internal-module-file witness fallback (WCR Phase 7, TASK E) ───

    #[test]
    fn internal_module_tier_classifies_when_relative_target_file_exists_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tmp.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let src_file = src_dir.join("a.ts");
        std::fs::write(&src_file, "// a\n").unwrap();
        // No code_nodes/repo_defs def recorded for `util` — only the FILE
        // itself is real.
        std::fs::write(src_dir.join("util.ts"), "export const x = 1;\n").unwrap();
        let src_file_str = src_file.to_string_lossy().to_string();

        let conn = mem();
        upsert_node(&conn, &module_node("a_mod", &src_file_str)).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            &src_file_str,
            &[import_edge_from("a_mod", "helper", &src_file_str, "./util")],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.internal_module, 1, "{stats:?}");
        assert_eq!(stats.unexplained, 0);
        assert_eq!(
            stats.external, 0,
            "relative module must never classify external"
        );

        let (boundary, evidence): (String, String) = conn
            .query_row(
                "SELECT boundary, evidence FROM code_edges WHERE src_id = 'a_mod' AND kind = 'imports'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(boundary, "internal_module");
        assert_eq!(evidence, "module_file:util.ts");
    }

    #[test]
    fn internal_module_tier_resolves_directory_index_file() {
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tmp.path().join("src");
        std::fs::create_dir_all(src_dir.join("widgets")).unwrap();
        let src_file = src_dir.join("a.ts");
        std::fs::write(&src_file, "// a\n").unwrap();
        std::fs::write(src_dir.join("widgets/index.ts"), "export const w = 1;\n").unwrap();
        let src_file_str = src_file.to_string_lossy().to_string();

        let conn = mem();
        upsert_node(&conn, &module_node("a_mod", &src_file_str)).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            &src_file_str,
            &[import_edge_from(
                "a_mod",
                "Widget",
                &src_file_str,
                "./widgets",
            )],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.internal_module, 1, "{stats:?}");
        let evidence: String = conn
            .query_row(
                "SELECT evidence FROM code_edges WHERE src_id = 'a_mod' AND kind = 'imports'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(evidence, "module_file:index.ts");
    }

    #[test]
    fn internal_module_tier_does_not_fire_when_target_file_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tmp.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let src_file = src_dir.join("a.ts");
        std::fs::write(&src_file, "// a\n").unwrap();
        let src_file_str = src_file.to_string_lossy().to_string();

        let conn = mem();
        upsert_node(&conn, &module_node("a_mod", &src_file_str)).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            &src_file_str,
            &[import_edge_from("a_mod", "ghost", &src_file_str, "./ghost")],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.internal_module, 0, "{stats:?}");
        assert_eq!(stats.unexplained, 1, "no file backs ./ghost anywhere");
    }

    #[test]
    fn internal_module_tier_alias_resolves_against_project_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("app");
        let src_dir = root.join("src/screens");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(root.join("package.json"), "{}").unwrap();
        // `@/components/Button` resolves project-root-relative (see
        // manifest::is_relative_module's doc comment).
        std::fs::create_dir_all(root.join("components")).unwrap();
        std::fs::write(
            root.join("components/Button.tsx"),
            "export const Button = 1;\n",
        )
        .unwrap();
        let src_file = src_dir.join("Home.tsx");
        std::fs::write(&src_file, "// home\n").unwrap();
        let src_file_str = src_file.to_string_lossy().to_string();

        let conn = mem();
        // `repo_scan::project_roots` derives roots from `code_nodes.file` —
        // seed one pointing at this project so the alias resolution has a
        // root candidate to try.
        upsert_node(
            &conn,
            &NodeRow {
                id: "root_anchor".into(),
                project: "proj".into(),
                file: src_file_str.clone(),
                kind: "function".into(),
                name: "anchor".into(),
                ..NodeRow::default()
            },
        )
        .unwrap();
        upsert_node(&conn, &module_node("a_mod", &src_file_str)).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            &src_file_str,
            &[import_edge_from(
                "a_mod",
                "Button",
                &src_file_str,
                "@/components/Button",
            )],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.internal_module, 1, "{stats:?}");
        let evidence: String = conn
            .query_row(
                "SELECT evidence FROM code_edges WHERE src_id = 'a_mod' AND kind = 'imports'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(evidence, "module_file:Button.tsx");
    }

    #[test]
    fn internal_module_tier_ignores_calls_edges() {
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tmp.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let src_file = src_dir.join("a.ts");
        std::fs::write(&src_file, "// a\n").unwrap();
        std::fs::write(src_dir.join("util.ts"), "export const x = 1;\n").unwrap();
        let src_file_str = src_file.to_string_lossy().to_string();

        let conn = mem();
        upsert_node(&conn, &def("foo", &src_file_str, "foo")).unwrap();
        let mut e = call_edge("foo", "helper", &src_file_str);
        // Defensive only: `via:`/`from:`-shaped evidence should never
        // appear on a real calls edge here, but the guard must hold
        // regardless of what the evidence column happens to contain.
        e.evidence = "from:./util".to_string();
        codegraph::replace_file_edges(&conn, "proj", &src_file_str, &[e]).unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            stats.internal_module, 0,
            "internal_module_tier is imports-only: {stats:?}"
        );
    }

    #[test]
    fn resolve_is_deterministic_with_internal_module_tier() {
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tmp.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let src_file = src_dir.join("a.ts");
        std::fs::write(&src_file, "// a\n").unwrap();
        std::fs::write(src_dir.join("util.ts"), "export const x = 1;\n").unwrap();
        let src_file_str = src_file.to_string_lossy().to_string();

        let conn = mem();
        upsert_node(&conn, &module_node("a_mod", &src_file_str)).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            &src_file_str,
            &[import_edge_from("a_mod", "helper", &src_file_str, "./util")],
        )
        .unwrap();

        let first = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        let second = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            first, second,
            "internal_module tier stays deterministic across repeated passes"
        );
        assert_eq!(first.internal_module, 1);

        let evidence: String = conn
            .query_row(
                "SELECT evidence FROM code_edges WHERE src_id = 'a_mod' AND kind = 'imports'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(evidence, "module_file:util.ts");
    }

    // ─── B1 module-path precise bind (WCR Phase 8, TASK A) ───

    #[test]
    fn module_bind_tier_precise_import_disambiguates_many_candidates() {
        // The classic per-component `styles` const collision: TWO files each
        // define a same-named `styles` symbol, but ComponentA's own
        // `import { styles } from './ComponentA.styles'` names exactly one
        // of them. Without B1 this would fall to B3 coedit (no margin, no
        // code_evolution rows here) and land in `ambiguous_remaining`.
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tmp.path().join("src/components");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("ComponentA.styles.ts"), "// a styles\n").unwrap();
        std::fs::write(src_dir.join("ComponentB.styles.ts"), "// b styles\n").unwrap();
        let a_styles_str = src_dir
            .join("ComponentA.styles.ts")
            .to_string_lossy()
            .to_string();
        let b_styles_str = src_dir
            .join("ComponentB.styles.ts")
            .to_string_lossy()
            .to_string();
        let comp_a_str = src_dir.join("ComponentA.tsx").to_string_lossy().to_string();

        let conn = mem();
        upsert_node(&conn, &def("styles_a", &a_styles_str, "styles")).unwrap();
        upsert_node(&conn, &def("styles_b", &b_styles_str, "styles")).unwrap();
        upsert_node(&conn, &module_node("a_mod", &comp_a_str)).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            &comp_a_str,
            &[import_edge_from(
                "a_mod",
                "styles",
                &comp_a_str,
                "./ComponentA.styles",
            )],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            stats.resolved, 1,
            "precise module-path bind despite two `styles` candidates: {stats:?}"
        );
        assert_eq!(
            stats.ambiguous_remaining, 0,
            "an import naming one specific file is not ambiguous"
        );

        let (dst, evidence): (String, String) = conn
            .query_row(
                "SELECT dst_id, evidence FROM code_edges WHERE src_id = 'a_mod' AND kind = 'imports'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            dst, "styles_a",
            "binds to the file the import names, not styles_b"
        );
        assert_eq!(evidence, "module_bind:ComponentA.styles.ts");
    }

    #[test]
    fn module_bind_tier_works_for_a_calls_edge_via_sibling_import() {
        // Same shape, but the pending edge is a `calls` edge for `styles`
        // (not the `imports` edge itself) — module evidence must come from
        // the SAME-FILE SIBLING `imports` edge (`module_for_pending`'s
        // calls-edge branch).
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tmp.path().join("src/components");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("ComponentA.styles.ts"), "// a styles\n").unwrap();
        std::fs::write(src_dir.join("ComponentB.styles.ts"), "// b styles\n").unwrap();
        let a_styles_str = src_dir
            .join("ComponentA.styles.ts")
            .to_string_lossy()
            .to_string();
        let b_styles_str = src_dir
            .join("ComponentB.styles.ts")
            .to_string_lossy()
            .to_string();
        let comp_a_str = src_dir.join("ComponentA.tsx").to_string_lossy().to_string();

        let conn = mem();
        upsert_node(&conn, &def("styles_a", &a_styles_str, "styles")).unwrap();
        upsert_node(&conn, &def("styles_b", &b_styles_str, "styles")).unwrap();
        upsert_node(&conn, &def("render", &comp_a_str, "render")).unwrap();
        upsert_node(&conn, &module_node("a_mod", &comp_a_str)).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            &comp_a_str,
            &[
                import_edge_from("a_mod", "styles", &comp_a_str, "./ComponentA.styles"),
                call_edge("render", "styles", &comp_a_str),
            ],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.total, 2);
        assert_eq!(stats.resolved, 2, "both edges bind: {stats:?}");
        assert_eq!(stats.ambiguous_remaining, 0);

        let dst: String = conn
            .query_row(
                "SELECT dst_id FROM code_edges WHERE src_id = 'render' AND kind = 'calls'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            dst, "styles_a",
            "calls edge inherits the sibling import's precise binding"
        );
    }

    #[test]
    fn module_bind_tier_falls_through_when_module_resolves_to_no_file() {
        // Relative import evidence is present, but the module doesn't
        // resolve to any real file on disk — module_bind_tier must no-op,
        // falling through to the old ambiguity-resolution behavior (no
        // repo_defs, no code_evolution margin -> stays ambiguous).
        let conn = mem();
        upsert_node(&conn, &def("styles_x", "x.ts", "styles")).unwrap();
        upsert_node(&conn, &def("styles_y", "y.ts", "styles")).unwrap();
        upsert_node(&conn, &module_node("z_mod", "z.ts")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "z.ts",
            &[import_edge_from(
                "z_mod",
                "styles",
                "z.ts",
                "./this_module_does_not_exist_anywhere_zzz",
            )],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            stats.resolved, 0,
            "module doesn't resolve to a real file — must never guess: {stats:?}"
        );
        assert_eq!(stats.ambiguous_remaining, 1);
    }

    #[test]
    fn module_bind_tier_no_import_evidence_multi_candidate_still_ambiguous() {
        // No `from:<module>` data at all on the imports edge (old-style,
        // pre-Phase-5 evidence shape) — module_for_pending must return
        // `None`, module_bind_tier must no-op, and multiple code_nodes
        // candidates stay genuinely ambiguous exactly as before B1 existed.
        let conn = mem();
        upsert_node(&conn, &def("styles_x", "x.ts", "styles")).unwrap();
        upsert_node(&conn, &def("styles_y", "y.ts", "styles")).unwrap();
        upsert_node(&conn, &module_node("z_mod", "z.ts")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "z.ts",
            &[import_edge("z_mod", "styles", "z.ts")],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.resolved, 0, "{stats:?}");
        assert_eq!(stats.ambiguous_remaining, 1);
    }

    #[test]
    fn resolve_is_deterministic_with_module_bind_tier() {
        // module_bind is a BIND tier: once it binds, `resolved = 1` and the
        // edge drops out of a SECOND pass's pending set entirely (unlike the
        // classify tiers' "resolve_is_deterministic_with_X_tier" pattern
        // above, which re-run on the same still-pending edge). The
        // meaningful determinism property for a bind tier is "same input on
        // fresh state always makes the same choice" — checked here across
        // two independently seeded connections, matching how B0/B1b/B2/B3
        // (which have no dedicated determinism tests of their own either,
        // for the same reason) are implicitly covered by
        // `resolve_is_deterministic_across_repeated_runs`'s AMBIGUOUS
        // (never-binding) scenario instead.
        fn seed_and_resolve() -> ResolveStats {
            let tmp = tempfile::tempdir().unwrap();
            let src_dir = tmp.path().join("src/components");
            std::fs::create_dir_all(&src_dir).unwrap();
            std::fs::write(src_dir.join("ComponentA.styles.ts"), "// a styles\n").unwrap();
            std::fs::write(src_dir.join("ComponentB.styles.ts"), "// b styles\n").unwrap();
            let a_styles_str = src_dir
                .join("ComponentA.styles.ts")
                .to_string_lossy()
                .to_string();
            let b_styles_str = src_dir
                .join("ComponentB.styles.ts")
                .to_string_lossy()
                .to_string();
            let comp_a_str = src_dir.join("ComponentA.tsx").to_string_lossy().to_string();

            let conn = mem();
            upsert_node(&conn, &def("styles_a", &a_styles_str, "styles")).unwrap();
            upsert_node(&conn, &def("styles_b", &b_styles_str, "styles")).unwrap();
            upsert_node(&conn, &module_node("a_mod", &comp_a_str)).unwrap();
            codegraph::replace_file_edges(
                &conn,
                "proj",
                &comp_a_str,
                &[import_edge_from(
                    "a_mod",
                    "styles",
                    &comp_a_str,
                    "./ComponentA.styles",
                )],
            )
            .unwrap();

            resolve_edges(&conn, "proj", &|_: &str| true).unwrap()
        }

        let first = seed_and_resolve();
        let second = seed_and_resolve();
        assert_eq!(
            first, second,
            "module_bind tier makes the same choice on fresh, identically seeded state"
        );
        assert_eq!(first.resolved, 1);
        assert_eq!(first.ambiguous_remaining, 0);
    }

    // ─── drifted tier (WCR Phase 8, TASK B) ───

    #[test]
    fn drifted_edge_is_skipped_by_all_tiers_and_counted() {
        let conn = mem();
        // A same-file def named `bar` exists — this WOULD bind via B0 if
        // reached. A pre-set `boundary = 'drifted'` must prevent that: every
        // tier, including B0, is skipped for a drifted edge.
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        upsert_node(&conn, &def("bar", "a.rs", "bar")).unwrap();
        let mut e = call_edge("foo", "bar", "a.rs");
        e.boundary = "drifted".to_string();
        e.evidence = "not_in_current_source".to_string();
        codegraph::replace_file_edges(&conn, "proj", "a.rs", &[e]).unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.total, 1);
        assert_eq!(stats.drifted, 1);
        assert_eq!(
            stats.resolved, 0,
            "drifted edge must never bind, even though bar is same-file"
        );
        assert_eq!(stats.unexplained, 0);
        assert_eq!(
            stats.closure_rate, 1.0,
            "drifted counts toward closure like stale/internal_module"
        );
        assert_eq!(
            stats.internal_binding_rate, 1.0,
            "drifted excluded from the binding denominator (0/0 -> 1.0)"
        );

        let (boundary, evidence, resolved, dst_id): (String, String, i64, String) = conn
            .query_row(
                "SELECT boundary, evidence, resolved, dst_id FROM code_edges
                 WHERE src_id = 'foo' AND kind = 'calls'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            boundary, "drifted",
            "resolve_edges must not touch a pre-set drifted boundary"
        );
        assert_eq!(evidence, "not_in_current_source");
        assert_eq!(resolved, 0);
        assert_eq!(
            dst_id, "name:bar",
            "drifted never binds — dst_id stays the placeholder"
        );
    }

    #[test]
    fn drifted_edge_excluded_from_internal_binding_denominator_with_other_edges() {
        // Mix a genuinely bound edge with a drifted one: binding rate must
        // be computed over the non-drifted denominator only (1/1 = 100%,
        // not 1/2).
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        upsert_node(&conn, &def("bar", "a.rs", "bar")).unwrap();
        upsert_node(&conn, &def("baz", "a.rs", "baz")).unwrap();
        let mut drifted_edge = call_edge("foo", "ghost", "a.rs");
        drifted_edge.boundary = "drifted".to_string();
        drifted_edge.evidence = "not_in_current_source".to_string();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "a.rs",
            &[call_edge("foo", "bar", "a.rs"), drifted_edge],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.total, 2);
        assert_eq!(stats.resolved, 1, "the real edge binds via B0");
        assert_eq!(stats.drifted, 1);
        assert_eq!(
            stats.internal_binding_rate, 1.0,
            "denominator excludes the drifted edge: 1 bound / 1 eligible"
        );
    }

    #[test]
    fn resolve_is_deterministic_with_drifted_tier() {
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        upsert_node(&conn, &def("bar", "a.rs", "bar")).unwrap();
        let mut e = call_edge("foo", "bar", "a.rs");
        e.boundary = "drifted".to_string();
        e.evidence = "not_in_current_source".to_string();
        codegraph::replace_file_edges(&conn, "proj", "a.rs", &[e]).unwrap();

        let first = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        let second = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            first, second,
            "drifted tier stays deterministic across repeated passes"
        );
        assert_eq!(first.drifted, 1);

        let (boundary, evidence): (String, String) = conn
            .query_row(
                "SELECT boundary, evidence FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            boundary, "drifted",
            "byte-identical across the repeated pass"
        );
        assert_eq!(evidence, "not_in_current_source");
    }

    // ─── X4 local-binding witness tier (WCR truth pass, TASK 2) ───

    fn insert_local_binding(conn: &Connection, project: &str, file: &str, name: &str) {
        conn.execute(
            "INSERT OR IGNORE INTO local_bindings (project, file, name) VALUES (?1, ?2, ?3)",
            params![project, file, name],
        )
        .unwrap();
    }

    #[test]
    fn x4_local_binding_classifies_when_zero_defs_anywhere_and_witnessed() {
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.ts", "foo")).unwrap();
        // No def anywhere for "playTrack" — code_nodes has no def, repo_defs
        // is empty.
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "a.ts",
            &[call_edge("foo", "playTrack", "a.ts")],
        )
        .unwrap();
        insert_local_binding(&conn, "proj", "a.ts", "playTrack");

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.local, 1);
        assert_eq!(stats.resolved, 0, "local classifies, never binds");
        assert_eq!(stats.unexplained, 0);
        assert_eq!(
            stats.closure_rate, 1.0,
            "local counts toward the closure numerator"
        );

        let (boundary, evidence, resolved, dst_id): (String, String, i64, String) = conn
            .query_row(
                "SELECT boundary, evidence, resolved, dst_id FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(boundary, "local");
        assert_eq!(evidence, "local_scope:playTrack");
        assert_eq!(resolved, 0);
        assert_eq!(dst_id, "name:playTrack", "dst_id stays the placeholder");
    }

    #[test]
    fn x4_local_binding_excluded_from_internal_binding_denominator() {
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.ts", "foo")).unwrap();
        upsert_node(&conn, &def("bar", "a.ts", "bar")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "a.ts",
            &[
                // Binds normally via B0 same_file.
                call_edge("foo", "bar", "a.ts"),
                // No def anywhere, but witnessed local.
                call_edge("foo", "playTrack", "a.ts"),
            ],
        )
        .unwrap();
        insert_local_binding(&conn, "proj", "a.ts", "playTrack");

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.total, 2);
        assert_eq!(stats.resolved, 1, "bar binds via B0");
        assert_eq!(stats.local, 1);
        assert_eq!(
            stats.internal_binding_rate, 1.0,
            "denominator excludes the local edge: 1 bound / 1 eligible"
        );
    }

    #[test]
    fn x4_never_fires_when_a_real_def_exists_elsewhere_for_the_name() {
        // Adversarial self-check: a name that is BOTH a witnessed local
        // SOMEWHERE and has a real def elsewhere in the project must NOT
        // classify local — def_file_count > 0 blocks it (condition (a)).
        // Uses the same receiver-gated (NoBind) shape as
        // `method_call_with_non_self_receiver_does_not_bind_via_unique_def_elsewhere`:
        // a method call off a non-self receiver never reaches a bind tier
        // even though `save` has exactly one code_nodes def elsewhere, so
        // the ONLY thing standing between this edge and a wrong `local`
        // classification is the explicit zero-defs-anywhere check.
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        upsert_node(&conn, &def("save_b", "b.rs", "save")).unwrap();
        let mut e = call_edge_via("foo", "save", "a.rs", "obj");
        e.callee_kind = "method".into();
        codegraph::replace_file_edges(&conn, "proj", "a.rs", &[e]).unwrap();
        // "save" also happens to be a local binding name in a.rs itself —
        // must still not classify local, because a real def of "save"
        // exists (in b.rs).
        insert_local_binding(&conn, "proj", "a.rs", "save");

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.local, 0, "a real def elsewhere must block X4");
        assert_eq!(stats.method, 1, "falls through to X2 method instead");

        let boundary: String = conn
            .query_row(
                "SELECT boundary FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(boundary, "method");
    }

    #[test]
    fn x4_never_preempts_a_successful_bind_tier() {
        // Adversarial self-check: a local_bindings witness for the SAME
        // (project, file, name) as an edge that a bind tier can already
        // resolve must never be reached at all — bind tiers run to
        // completion (and `continue`) long before `classify_only` (and
        // therefore X4) is ever called.
        let conn = mem();
        upsert_node(&conn, &def("bar", "x.rs", "bar")).unwrap();
        upsert_node(&conn, &def("foo", "z.rs", "foo")).unwrap();
        codegraph::replace_file_edges(&conn, "proj", "z.rs", &[call_edge("foo", "bar", "z.rs")])
            .unwrap();
        insert_local_binding(&conn, "proj", "z.rs", "bar");

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.local, 0);
        assert_eq!(stats.resolved, 1, "B2 unique_def still binds it");

        let (dst, evidence): (String, String) = conn
            .query_row(
                "SELECT dst_id, evidence FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(dst, "bar");
        assert_eq!(evidence, "unique_def");
    }

    #[test]
    fn x4_silent_when_local_bindings_table_is_empty() {
        // Empty local_bindings table (every resolve pass outside the WCR
        // live gate, since only `backfill_wcr_witnesses` ever populates it)
        // -> zero local classifications, tier silent not erroring; the edge
        // falls through to the pre-existing unexplained/ambiguous behavior
        // exactly as before this tier was added.
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "a.rs",
            &[call_edge("foo", "ghost_symbol_xyz", "a.rs")],
        )
        .unwrap();

        let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(stats.local, 0);
        assert_eq!(stats.unexplained, 1);
    }

    #[test]
    fn resolve_is_deterministic_with_local_tier() {
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.ts", "foo")).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            "a.ts",
            &[call_edge("foo", "playTrack", "a.ts")],
        )
        .unwrap();
        insert_local_binding(&conn, "proj", "a.ts", "playTrack");

        let first = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        let second = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
        assert_eq!(
            first, second,
            "local tier stays deterministic across repeated passes"
        );
        assert_eq!(first.local, 1);
    }
}
