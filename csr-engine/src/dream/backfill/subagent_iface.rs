//! A-a (dream backfill pass 2): sub-session evidence visibility —
//! INTERFACE ONLY.
//!
//! This module owns the vocabulary, the path-attribution rule, and the
//! dedup/ordering INVARIANT checks that a future import body (a separate,
//! larger piece of work: walking every `~/.claude/projects/**/subagents/
//! agent-*.jsonl` file and populating new `episode_index` columns) and the
//! dream-composer's bar-audit will both call. It is deliberately NOT a
//! JSONL parser and does NOT touch the database — see the reconciliation
//! note below for why.
//!
//! INVARIANT: attribution flows transcript path -> git toplevel (A-c); a
//! missing parent link degrades to "own cluster" and NEVER drops evidence.
//!
//! # Reconciliation against the real schema (pass 2)
//!
//! `episode_index` (see `storage::migrations::run`) has none of the
//! columns the design source (`ox_turn3.md`) assumes already exist
//! (`evidence_kind`, `cluster_id`, `root_session_id`, `source_role`,
//! `source_path`, `ts_source`, `evidence_provenance`) and there is no
//! `v_episode_family` SQL view. Those are FUTURE additive columns (see the
//! `WIRING` doc block below, ported from the design source) — this pass
//! adds neither the columns nor a migration for them, because no code in
//! this pass writes to them: adding schema ahead of its first writer is
//! exactly the anti-pattern the `dreams_v1` supersession-CHECK migration's
//! own comment warns against (churn with no consumer). Consequently:
//!
//! - [`enforce_parent_dedup`] (Rule 2) is rewritten as a PURE in-memory
//!   transform over a caller-supplied `&mut [EpisodeAnchorRow]` rather
//!   than a function that runs `SELECT`/`UPDATE` against `episode_index`
//!   directly. It is fully testable today, and the future import body
//!   calls it by loading real rows into `EpisodeAnchorRow`, applying it,
//!   and persisting whichever rows it flags — without this pass having to
//!   guess at column names that may still change before that body lands.
//! - Every other function here (`attribute_transcript_path`,
//!   `check_child_never_demoted`, `assert_pair_spans_clusters`,
//!   `check_timestamp_provenance`, `DreamProvenance::draws_on_subagent_work`,
//!   `audit_bar_clause`) was already schema-independent in the design
//!   source and is ported with one real-path fix: `attribute_transcript_path`
//!   strips a trailing `.jsonl` off the session component. The design
//!   source assumed every transcript path has a `subagents/` directory
//!   segment somewhere before the file; a PARENT transcript's real path is
//!   `<proj>/<session_id>.jsonl` (the session id IS the filename stem, no
//!   directory), so without the strip the "session id" would silently
//!   include the `.jsonl` extension for every parent transcript.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, Result};

pub const EVIDENCE_PARENT_DIRECT: &str = "parent_direct";
pub const EVIDENCE_SUBAGENT_EDIT: &str = "subagent_edit";
/// Prose-eligible, GATE-INELIGIBLE: a lossy parent-side summary of
/// subagent work — quotes only, never a gate leg.
pub const EVIDENCE_SUMMARY_MENTION: &str = "summary_mention";
/// `wf_*` workflow sessions: real edits, gate-eligible.
pub const EVIDENCE_WORKFLOW: &str = "workflow";

/// Which evidence kinds may serve as `E_before`/`E_after`. `summary_mention`
/// is a lossy parent-side summary — quotes only, never a gate leg.
pub fn gate_eligible(kind: &str) -> bool {
    matches!(
        kind,
        EVIDENCE_PARENT_DIRECT | EVIDENCE_SUBAGENT_EDIT | EVIDENCE_WORKFLOW
    )
}

// WIRING (one line each, into existing modules — future work, not this
// pass): columns A-a will add to `episode_index` via an additive `ALTER
// TABLE` migration (NULL/default = pre-A-a row, so every existing row
// stays valid):
//   evidence_kind       TEXT NOT NULL DEFAULT 'parent_direct'
//   cluster_id          TEXT NOT NULL  -- parent session id; orphans: own session id
//   root_session_id     TEXT NOT NULL
//   source_role         TEXT           -- 'parent' | 'subagent' | 'orphan'
//   source_path         TEXT           -- transcript path (attribution receipt)
//   ts_source           TEXT NOT NULL DEFAULT 'transcript' -- 'transcript'|'import'
//   evidence_provenance TEXT           -- JSON DreamProvenance (bar-clause audit)
//
// * `death_time::DeathTime`/anchor loading gains an `origin` discriminator
//   so a `summary_mention`-origin anchor is skipped when building the
//   per-symbol funeral index (gate-invisible, still present for prose).
// * episode `ts` MUST be the transcript-native message timestamp (see
//   [`check_timestamp_provenance`]) — import time corrupts before/after
//   ordering.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Parent,
    Subagent,
    Orphan,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Parent => "parent",
            Role::Subagent => "subagent",
            Role::Orphan => "orphan",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PathAttribution {
    /// The `~/.claude/projects/<this>` component (still dash-encoded — see
    /// [`project_dir_candidates`] for decoding it back to a real path).
    pub project_dir: String,
    /// The `<session>` path component, with any `.jsonl` extension
    /// stripped (see the module doc's reconciliation note).
    pub session_id: String,
    pub role: Role,
    /// The work-cluster key: the parent session id.
    pub cluster_id: String,
    pub root_session_id: String,
}

/// Pure path parser: `~/.claude/projects/<proj>/<session>[.jsonl]` for a
/// parent transcript, or `~/.claude/projects/<proj>/<session>/subagents/
/// agent-*.jsonl` for a subagent transcript. A `subagents/` segment marks
/// a subagent of `<session>`; the session component is the cluster id EVEN
/// WHEN `chunk_provenance` is NULL (measured: 45,586 sidechain chunks, 0
/// resolvable `source_conv_id` — the path is the only parent link).
///
/// Only ever returns `Role::Parent` or `Role::Subagent`: `Role::Orphan` is
/// the LOADER's fallback when this returns `None` (path doesn't contain a
/// `projects` segment, or is too short to have both a project and session
/// component) — see the loader-side rule below.
pub fn attribute_transcript_path(path: &str) -> Option<PathAttribution> {
    let i = path.split('/').position(|c| c == "projects")?;
    let mut it = path.split('/').skip(i + 1);
    let proj = it.next()?.to_string();
    let session_raw = it.next()?.to_string();
    if session_raw.is_empty() {
        return None;
    }
    let session = session_raw
        .strip_suffix(".jsonl")
        .unwrap_or(&session_raw)
        .to_string();
    let rest: Vec<&str> = it.collect();
    let is_sub = rest.contains(&"subagents");
    let role = if is_sub { Role::Subagent } else { Role::Parent };
    Some(PathAttribution {
        project_dir: proj,
        session_id: session.clone(),
        role,
        cluster_id: session.clone(),
        root_session_id: session,
    })
}

/// `claude` encodes cwd as a dash-joined path; try raw, then dash-decoded.
/// The loader picks the first candidate whose repo-identity resolution
/// (`family::resolve_repo_identity`) yields a toplevel. This is a lossy
/// heuristic when the real directory name itself contains a hyphen (the
/// same ambiguity Claude Code's own encoding has) — never presented as
/// exact.
pub fn project_dir_candidates(proj_component: &str) -> Vec<String> {
    let mut v = vec![proj_component.to_string()];
    if let Some(rest) = proj_component.strip_prefix('-') {
        v.push(format!("/{}", rest.replace('-', "/")));
    }
    v
}

// Loader-side rule (the future import body implements the JSONL walk):
//   attribution = attribute_transcript_path(path); project/family via
//   family::compute_families + repo-identity resolution on the resolved
//   project_dir candidate. attribute_transcript_path returning None =>
//   Role::Orphan, cluster_id = own session id. Orphans pair with every
//   OTHER cluster — conservative, evidence-preserving (never dropped).

// ----- Rule 1: child anchors are never demoted -----

/// A `subagent_edit`/`workflow` episode is always gate-eligible regardless
/// of overlap with its parent. Enforced by refusing any write that sets
/// `summary_mention` on subagent-role evidence.
pub fn check_child_never_demoted(evidence_kind: &str, role: &str) -> Result<()> {
    if evidence_kind == EVIDENCE_SUMMARY_MENTION && role == "subagent" {
        bail!("invariant R1 violated: subagent evidence demoted to summary_mention");
    }
    Ok(())
}

// ----- Rule 2: parent duplicates -> summary_mention (gate-ineligible) -----

/// One anchor as the future `episode_index.anchors_json` will carry it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Anchor {
    pub file: String,
    pub symbol: String,
    pub body_hash: String,
    #[serde(default)]
    pub origin: Option<String>,
}

/// One future `episode_index` row's identity + anchors, as the import body
/// will load it (via the not-yet-existing `cluster_id`/`source_role`
/// columns — see the module doc). Kept minimal on purpose: only what
/// [`enforce_parent_dedup`] needs.
#[derive(Debug, Clone)]
pub struct EpisodeAnchorRow {
    pub episode_id: String,
    pub cluster_id: String,
    /// `"parent"` | `"subagent"` | `"orphan"`.
    pub role: String,
    pub anchors: Vec<Anchor>,
}

/// For each cluster: any parent-role anchor whose `(file, symbol,
/// body_hash)` is also carried by a same-cluster `subagent_edit` child is
/// rewritten in place with `origin = Some(EVIDENCE_SUMMARY_MENTION)`.
/// Child rows are never touched (R1). Deterministic (input order
/// preserved). Returns the count of anchors actually demoted; the future
/// import body persists only the rows whose anchors changed.
///
/// Pure and DB-free by design — see the module doc's reconciliation note.
pub fn enforce_parent_dedup(rows: &mut [EpisodeAnchorRow]) -> usize {
    let mut child: HashMap<String, HashSet<(String, String, String)>> = HashMap::new();
    for r in rows.iter() {
        if r.role == "subagent" {
            for a in &r.anchors {
                child.entry(r.cluster_id.clone()).or_default().insert((
                    a.file.clone(),
                    a.symbol.clone(),
                    a.body_hash.clone(),
                ));
            }
        }
    }
    let mut demoted = 0usize;
    for r in rows.iter_mut() {
        if r.role != "parent" {
            continue; // orphans have no children
        }
        let Some(set) = child.get(&r.cluster_id) else {
            continue;
        };
        for a in &mut r.anchors {
            if set.contains(&(a.file.clone(), a.symbol.clone(), a.body_hash.clone()))
                && a.origin.as_deref() != Some(EVIDENCE_SUMMARY_MENTION)
            {
                a.origin = Some(EVIDENCE_SUMMARY_MENTION.to_string());
                demoted += 1;
            }
        }
    }
    demoted
}

// ----- Rule 3: pairs must span clusters -----

/// A parent and its own subagent are ONE work event: they can never be
/// `E_before`/`E_after` of themselves. `death_time` enforces this at emit;
/// the ledger/era generators must call this too once cluster ids exist.
pub fn assert_pair_spans_clusters(before_cluster: &str, after_cluster: &str) -> Result<()> {
    if before_cluster == after_cluster {
        bail!("invariant R3 violated: pair within a single work cluster ({before_cluster})");
    }
    Ok(())
}

// ----- transcript-native timestamps -----

/// Gate legs order the world by episode `ts`; an import-time `ts` poisons
/// that order. Gate-eligible episodes must be transcript-timestamped.
pub fn check_timestamp_provenance(ts_source: &str, evidence_kind: &str) -> Result<()> {
    if ts_source != "transcript" && gate_eligible(evidence_kind) {
        bail!("invariant T violated: gate-eligible episode with ts_source={ts_source}");
    }
    Ok(())
}

// ----- evidence_provenance: the bar clause, mechanically auditable -----

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DreamProvenance {
    pub parent_sessions: Vec<String>,
    /// NON-EMPTY satisfies the bar clause.
    pub subagent_sessions: Vec<String>,
    pub chunk_ids: Vec<i64>,
    pub attribution: &'static str, // "transcript_path" | "chunk_provenance" | "cwd"
}

impl DreamProvenance {
    /// "Each dream must draw on subagent work" — checked mechanically,
    /// never by quiz self-report.
    pub fn draws_on_subagent_work(&self) -> bool {
        !self.subagent_sessions.is_empty()
    }
}

/// Compose-stage audit, paired with
/// `claim_resolution::assert_verified_quotes`: no dream is written to
/// `dreams_v1` without provenance satisfying the clause.
pub fn audit_bar_clause(p: &Option<DreamProvenance>) -> Result<()> {
    let Some(p) = p else {
        bail!("dream missing evidence_provenance")
    };
    if !p.draws_on_subagent_work() {
        bail!("dream cites no subagent evidence — bar clause unmet");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // Required test 2: attribute_transcript_path on a real subagents path.
    // -----------------------------------------------------------------

    #[test]
    fn attribute_transcript_path_parses_a_real_subagent_path() {
        let path = "/Users/rama/.claude/projects/-Users-rama-projects-anukriti-website/\
                    41c4b58d-8480-4dec-ba05-05d119d66c66/subagents/agent-explore-1.jsonl";
        let a = attribute_transcript_path(path).expect("must parse");
        assert_eq!(a.project_dir, "-Users-rama-projects-anukriti-website");
        assert_eq!(a.session_id, "41c4b58d-8480-4dec-ba05-05d119d66c66");
        assert_eq!(a.cluster_id, "41c4b58d-8480-4dec-ba05-05d119d66c66");
        assert_eq!(a.role, Role::Subagent);
    }

    #[test]
    fn attribute_transcript_path_parses_a_real_parent_path_stripping_jsonl() {
        // Parent transcripts have NO subagents/ segment at all: the file
        // IS <session>.jsonl directly under the project dir. Without the
        // .jsonl strip this would wrongly report a cluster id ending in
        // ".jsonl".
        let path = "/Users/rama/.claude/projects/-Users-rama-projects-csr/\
                    8f07c97c-2184-44eb-a315-5e89c394c7ac.jsonl";
        let a = attribute_transcript_path(path).expect("must parse");
        assert_eq!(a.session_id, "8f07c97c-2184-44eb-a315-5e89c394c7ac");
        assert_eq!(a.cluster_id, "8f07c97c-2184-44eb-a315-5e89c394c7ac");
        assert_eq!(a.role, Role::Parent);
    }

    #[test]
    fn attribute_transcript_path_none_without_a_projects_segment() {
        assert!(attribute_transcript_path("/nowhere/relevant/file.jsonl").is_none());
    }

    // -----------------------------------------------------------------
    // Required test 3: assert_pair_spans_clusters rejects same-cluster.
    // -----------------------------------------------------------------

    #[test]
    fn assert_pair_spans_clusters_rejects_same_cluster() {
        assert!(assert_pair_spans_clusters("sess-1", "sess-1").is_err());
        assert!(assert_pair_spans_clusters("sess-1", "sess-2").is_ok());
    }

    #[test]
    fn check_child_never_demoted_rejects_subagent_summary_mention() {
        assert!(check_child_never_demoted(EVIDENCE_SUMMARY_MENTION, "subagent").is_err());
        assert!(check_child_never_demoted(EVIDENCE_SUMMARY_MENTION, "parent").is_ok());
        assert!(check_child_never_demoted(EVIDENCE_SUBAGENT_EDIT, "subagent").is_ok());
    }

    #[test]
    fn check_timestamp_provenance_rejects_import_time_on_gate_eligible_evidence() {
        assert!(check_timestamp_provenance("import", EVIDENCE_SUBAGENT_EDIT).is_err());
        assert!(check_timestamp_provenance("transcript", EVIDENCE_SUBAGENT_EDIT).is_ok());
        assert!(check_timestamp_provenance("import", EVIDENCE_SUMMARY_MENTION).is_ok());
    }

    #[test]
    fn enforce_parent_dedup_demotes_only_parent_side_overlap() {
        let mut rows = vec![
            EpisodeAnchorRow {
                episode_id: "parent-1".into(),
                cluster_id: "cluster-a".into(),
                role: "parent".into(),
                anchors: vec![Anchor {
                    file: "src/x.rs".into(),
                    symbol: "foo".into(),
                    body_hash: "h1".into(),
                    origin: None,
                }],
            },
            EpisodeAnchorRow {
                episode_id: "child-1".into(),
                cluster_id: "cluster-a".into(),
                role: "subagent".into(),
                anchors: vec![Anchor {
                    file: "src/x.rs".into(),
                    symbol: "foo".into(),
                    body_hash: "h1".into(),
                    origin: None,
                }],
            },
        ];
        let demoted = enforce_parent_dedup(&mut rows);
        assert_eq!(demoted, 1);
        assert_eq!(
            rows[0].anchors[0].origin.as_deref(),
            Some(EVIDENCE_SUMMARY_MENTION)
        );
        // R1: the child's own anchor is NEVER demoted.
        assert_eq!(rows[1].anchors[0].origin, None);
    }

    #[test]
    fn audit_bar_clause_requires_nonempty_subagent_sessions() {
        assert!(audit_bar_clause(&None).is_err());
        assert!(audit_bar_clause(&Some(DreamProvenance {
            parent_sessions: vec!["p1".into()],
            subagent_sessions: vec![],
            chunk_ids: vec![],
            attribution: "transcript_path",
        }))
        .is_err());
        assert!(audit_bar_clause(&Some(DreamProvenance {
            parent_sessions: vec!["p1".into()],
            subagent_sessions: vec!["s1".into()],
            chunk_ids: vec![],
            attribution: "transcript_path",
        }))
        .is_ok());
    }
}
