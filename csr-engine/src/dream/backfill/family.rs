//! Project-family resolution for the dream backfill (design ruling
//! 2026-08-26: generators run over the CROSS-PROJECT corpus — the project
//! key is attribution metadata, not a partition boundary).
//!
//! # Why this exists (measured defect, run-3 corpus)
//!
//! One logical repo fragments across several `project` keys depending on
//! where a session's cwd happened to sit: the maintainer's corpus carries
//! `claude-self-reflect` (episodes + 16k ledger rows) alongside
//! `claude-self-reflect-csr-engine` (8.1k ledger rows, 244 funerals, ZERO
//! episodes — sessions started inside the `csr-engine/` subdir), plus
//! worktree-cwd keys (`csr-theme-wt`, `anukriti-command-center-worktrees`)
//! and even case variants (`Anukriti-Campaigns` vs an `anukriti` ledger key
//! holding files under `Anukriti-Campaigns/`). Stage 2's generators joined
//! episodes to witness verdicts on the EXACT key, so every funeral recorded
//! under a sibling key was invisible.
//!
//! # A-c (2026-08-27): repo identity replaces the hyphen-prefix heuristic
//!
//! The first version of this module grouped keys by NAME: two keys merged
//! when, case-insensitively, they were equal or one was a hyphen-boundary
//! prefix of the other (`anukriti` + `anukriti-command-center`). That
//! heuristic is now known to be a **false-positive generator**: on the live
//! corpus `anukriti` and `anukriti-website` are two genuinely unrelated git
//! repositories (different toplevels, different origin remotes) that
//! happen to share a hyphen-delimited name root — the old rule silently
//! merged them, attributing one repo's funerals and relapses to the other.
//!
//! The grouping key is now **git repo identity**, resolved from files the
//! project key actually recorded (never from the key's own text):
//!
//! - primary edge: the same `git rev-parse --show-toplevel` ([`RepoIdentity::toplevel`]);
//! - secondary edge: the same normalized `origin` remote URL
//!   ([`RepoIdentity::origin`]) — this is what keeps a linked git worktree
//!   (whose own toplevel differs from the main checkout's) in the same
//!   family as its main checkout, since both share one `origin`;
//! - fallback: when a key resolves to NEITHER (the corpus was imported from
//!   a machine/checkout no longer present), it only merges with another
//!   equally-unresolved key by exact, case-insensitive name equality —
//!   never by name PREFIX. There is no hyphen-prefix edge anywhere in this
//!   module any more.
//!
//! This correctly reunites `claude-self-reflect` with
//! `claude-self-reflect-csr-engine` (a session's cwd sitting one directory
//! deeper inside the SAME repo resolves to the SAME `git
//! rev-parse --show-toplevel`), while correctly keeping `anukriti` and
//! `anukriti-website` apart (two different toplevels, two different
//! origins, sharing nothing but a name root).
//!
//! See [`group_keys`] for the exact union-find rule and
//! [`resolve_repo_identity`] for how one key's identity is resolved.
//!
//! # File canonicalization
//!
//! A small tail of ledger rows (11 distinct files on the live corpus)
//! record worktree checkouts (`<repo>/.claude/worktrees/<wt>/rest`).
//! [`canon_file`] collapses that segment so the same symbol stamped from a
//! worktree and from the main checkout joins as one identity.

use std::collections::{BTreeMap, HashMap};
use std::process::Command;

use anyhow::Result;
use rusqlite::{params, Connection};

use crate::extraction::repo_root::repo_root_for_file;

/// One project family: a display/attribution `name` plus every raw
/// `project` key that resolves into it. `members` is sorted and never
/// empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Family {
    pub name: String,
    pub members: Vec<String>,
}

impl Family {
    /// A single-key family — the exact pre-family behavior (name = the key
    /// verbatim, member set = just it). Test fixtures and the
    /// `gate_project` compatibility wrapper use this.
    pub fn single(project: &str) -> Family {
        Family {
            name: project.to_string(),
            members: vec![project.to_string()],
        }
    }
}

/// Collapse a `/.claude/worktrees/<name>/` path segment so worktree and
/// main-checkout paths of the same file share one canonical identity.
/// Paths without the segment come back unchanged (the overwhelmingly common
/// case). Only the FIRST occurrence is collapsed — nested worktrees don't
/// exist in this layout.
pub fn canon_file(file: &str) -> String {
    const MARKER: &str = "/.claude/worktrees/";
    let Some(start) = file.find(MARKER) else {
        return file.to_string();
    };
    let after_marker = start + MARKER.len();
    match file[after_marker..].find('/') {
        Some(slash) => format!("{}{}", &file[..start], &file[after_marker + slash..]),
        // `<repo>/.claude/worktrees/<wt>` with no trailing path — nothing
        // meaningful to collapse onto; leave as-is.
        None => file.to_string(),
    }
}

/// One project key's resolved git-repo identity (A-c). Fields are resolved
/// from REAL files the key recorded — never from the key's own name/text,
/// which is exactly the property the hyphen-prefix heuristic lacked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RepoIdentity {
    /// Canonicalized `git rev-parse --show-toplevel` of the first recorded
    /// file that resolves to a repo on this machine. `None` when nothing
    /// recorded for this key resolves anywhere (checkout no longer
    /// present) — the [`group_keys`] equality-only fallback then applies.
    pub(super) toplevel: Option<String>,
    /// Secondary edge: that toplevel's normalized `origin` remote URL, so a
    /// linked worktree (own toplevel, same origin) still merges with its
    /// main checkout.
    pub(super) origin: Option<String>,
    /// Third edge (F4 fix, Codex review pass 1, finding #5): that
    /// toplevel's canonicalized `git rev-parse --git-common-dir` — every
    /// linked worktree of one repository shares exactly one common git
    /// dir with its main checkout, even when the repo carries no `origin`
    /// remote at all (a purely local repo, or one whose remote was never
    /// configured). `origin` alone under-merges that case; `common_dir`
    /// closes it without depending on a remote existing.
    pub(super) common_dir: Option<String>,
}

/// `git -C <repo_root> <args>` with ambient `GIT_*` env stripped — same
/// small-git-helper convention this codebase's other dream-backfill
/// modules already use (`pairs::git_at`, `verify::git_at`), duplicated
/// locally rather than shared, per that established convention.
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

/// `git config --get remote.origin.url`, normalized so `git@host:a/b.git`,
/// `ssh://git@host/a/b.git`, and `https://host/a/b` all compare equal.
/// `None` on any failure (no remote, no git, not a repo) — never a guess.
fn git_origin_url(repo_root: &str) -> Option<String> {
    let output = git_at(repo_root)
        .arg("config")
        .arg("--get")
        .arg("remote.origin.url")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8(output.stdout).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(normalize_origin(trimmed))
    }
}

/// `git rev-parse --git-common-dir`, resolved to an absolute, canonicalized
/// path: for a linked worktree this is the MAIN checkout's `.git` directory
/// (shared across every worktree of that repository), while for the main
/// checkout itself (or a non-worktree repo) it is just its own `.git`.
/// `None` on any failure — no git, not a repo, `repo_root` gone from disk.
fn git_common_dir(repo_root: &str) -> Option<String> {
    let output = git_at(repo_root)
        .arg("rev-parse")
        .arg("--git-common-dir")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8(output.stdout).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = std::path::Path::new(trimmed);
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::path::Path::new(repo_root).join(path)
    };
    Some(
        std::fs::canonicalize(&abs)
            .unwrap_or(abs)
            .to_string_lossy()
            .to_string(),
    )
}

fn normalize_origin(url: &str) -> String {
    let mut u = url.trim().to_ascii_lowercase();
    if let Some(stripped) = u.strip_suffix(".git") {
        u = stripped.to_string();
    }
    let u = u.trim_end_matches('/');
    if let Some(rest) = u.strip_prefix("git@") {
        return rest.replacen(':', "/", 1);
    }
    for scheme in ["ssh://git@", "https://", "http://", "git://"] {
        if let Some(rest) = u.strip_prefix(scheme) {
            return rest.to_string();
        }
    }
    u.to_string()
}

/// Resolve one project key's repo identity from its OWN recorded files
/// (never its name): the first candidate that resolves to a real git
/// toplevel wins, and that toplevel's origin/common-dir (if any) become the
/// secondary edges. [`RepoIdentity::default`] (all `None`) when nothing
/// resolves.
///
/// F4 fix (Codex review pass 1, finding #5): a RELATIVE candidate path is
/// skipped outright, never handed to [`repo_root_for_file`]. That resolver
/// runs `git -C <dir> rev-parse --show-toplevel`, and for a relative `dir`
/// git resolves it against THIS PROCESS'S OWN current working directory —
/// not against whatever machine/session originally recorded the path. A
/// project whose anchors happen to record a relative path like `src/...`
/// can then spuriously resolve to whatever repo the backfill binary itself
/// happens to be running from (measured on this very checkout: `src/...`
/// resolves to `claude-self-reflect`'s own toplevel even for an unrelated,
/// episode-only project, purely because THIS process's cwd sits inside
/// that repo) — an unrelated project silently over-merges into whichever
/// repo the engine happens to be invoked from. Absolute paths are the only
/// candidates trustworthy enough to resolve identity from; a relative-only
/// project key falls through to [`group_keys`]'s name-equality fallback
/// instead, same as an unresolvable one.
pub(super) fn resolve_repo_identity(candidate_files: &[String]) -> RepoIdentity {
    for file in candidate_files {
        if file.is_empty() || !std::path::Path::new(file).is_absolute() {
            continue;
        }
        if let Some(toplevel) = repo_root_for_file(file) {
            let origin = git_origin_url(&toplevel);
            let common_dir = git_common_dir(&toplevel);
            return RepoIdentity {
                toplevel: Some(toplevel),
                origin,
                common_dir,
            };
        }
    }
    RepoIdentity::default()
}

/// Up to `limit` file paths recorded for `project`: `witness_ledger` first
/// (cheap, indexed, real absolute paths per its own doc convention),
/// THEN — F4 fix (Codex review pass 1, finding #5) — ALWAYS ALSO
/// `episode_index.anchors_json` candidates appended after them, up to
/// `limit` more.
///
/// The previous version only ever looked at anchors when the ledger
/// returned literally zero rows for this key. That under-samples in
/// exactly the case that matters most: a project whose `limit`-many sampled
/// ledger paths all happen to point at a checkout no longer present on this
/// machine (stale paths, `limit` too small to reach a resolvable one) never
/// even tried the anchor-derived candidates that might have resolved fine —
/// [`resolve_repo_identity`] never got the chance. Appending unconditionally
/// costs one extra (already-indexed) query per key and changes nothing for
/// the common case (ledger candidates resolve first and win, since
/// [`resolve_repo_identity`] takes the FIRST resolving candidate); it only
/// adds coverage for the case that was silently dropped before.
///
/// `pub(super)` (A-e, pass 2): [`super::intent_channel`] reuses this to
/// resolve a family's own repo-identity toplevel when attributing a raw
/// filesystem path (history.jsonl's `project` field) to a family — the
/// same repo-identity resolution [`resolve_repo_identity`] already does for
/// family grouping, just invoked a second time against one family's own
/// member keys instead of during `compute_families`'s all-keys pass.
pub(super) fn candidate_files_for_project(
    conn: &Connection,
    project: &str,
    limit: usize,
) -> Result<Vec<String>> {
    let mut out = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT file FROM witness_ledger
             WHERE project = ?1 AND file IS NOT NULL AND file != ''
             LIMIT ?2",
        )?;
        for f in stmt.query_map(params![project, limit as i64], |r| r.get::<_, String>(0))? {
            out.push(f?);
        }
    }
    {
        let mut stmt =
            conn.prepare("SELECT anchors_json FROM episode_index WHERE project = ?1 LIMIT ?2")?;
        let mut appended = 0usize;
        for aj in stmt.query_map(params![project, limit as i64], |r| r.get::<_, String>(0))? {
            let aj = aj?;
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&aj) else {
                continue;
            };
            let Some(arr) = value.as_array() else {
                continue;
            };
            for item in arr {
                if let Some(f) = item.get("file").and_then(|v| v.as_str()) {
                    if !f.is_empty() {
                        out.push(f.to_string());
                        appended += 1;
                    }
                }
            }
            if appended >= limit {
                break;
            }
        }
    }
    Ok(out)
}

/// F4 fix (Codex review pass 1, finding #5): the previous default of 8
/// sampled ledger paths meant a project whose first 8 (unordered) rows all
/// pointed at a stale/moved checkout never even tried a later, still-
/// resolvable path. Widened to a still-cheap, indexed-query sample size.
pub(super) const CANDIDATE_FILE_SAMPLE_LIMIT: usize = 64;

/// Group every distinct `project` key present in `episode_index` OR
/// `witness_ledger` into families by GIT REPO IDENTITY (A-c) — see the
/// module doc and [`group_keys`] for the exact rule. Sorted by family name
/// for deterministic iteration order — the CLI's per-family stage lines and
/// the dry-run report depend on that stability.
pub fn compute_families(conn: &Connection) -> Result<Vec<Family>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT project FROM episode_index
         UNION
         SELECT DISTINCT project FROM witness_ledger
         ORDER BY project",
    )?;
    let keys: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut entries = Vec::with_capacity(keys.len());
    for key in keys {
        let files = candidate_files_for_project(conn, &key, CANDIDATE_FILE_SAMPLE_LIMIT)?;
        let identity = resolve_repo_identity(&files);
        entries.push((key, identity));
    }
    Ok(group_keys(entries))
}

/// Pure grouping core of [`compute_families`], separated for direct unit
/// testing without a database or real git repos.
///
/// Two entries union when ANY of: (1) their [`RepoIdentity::toplevel`]s are
/// equal, (2) their [`RepoIdentity::origin`]s are equal, or (3) their
/// [`RepoIdentity::common_dir`]s are equal (F4 fix: every linked worktree
/// of one repository shares this even with no `origin` remote configured
/// at all). An entry whose identity resolves NONE of those three ways only
/// ever merges with another equally unresolved entry by exact,
/// case-insensitive name equality — never by name prefix, which is exactly
/// the heuristic A-c removes.
///
/// Documented, intended consequence: a monorepo subdirectory that resolves
/// to the SAME toplevel as its parent (a session run from `repo/app-a` and
/// another from `repo/app-b`, both inside one git checkout) merges into one
/// family even when the two subdirectories are logically independent
/// applications. Repo identity is git's own notion of "one repository",
/// not "one deployable unit" — splitting those apart would need an
/// explicit subdirectory boundary this module has no evidence for, so the
/// coarser, git-truthful merge is the deliberate choice here.
pub(super) fn group_keys(entries: Vec<(String, RepoIdentity)>) -> Vec<Family> {
    let n = entries.len();
    let mut parent: Vec<usize> = (0..n).collect();

    fn find(parent: &mut [usize], i: usize) -> usize {
        if parent[i] != i {
            let root = find(parent, parent[i]);
            parent[i] = root;
        }
        parent[i]
    }
    fn union(parent: &mut [usize], a: usize, b: usize) {
        let (ra, rb) = (find(parent, a), find(parent, b));
        if ra != rb {
            parent[rb] = ra;
        }
    }

    let mut by_toplevel: HashMap<&str, usize> = HashMap::new();
    let mut by_origin: HashMap<&str, usize> = HashMap::new();
    let mut by_common_dir: HashMap<&str, usize> = HashMap::new();
    let mut by_name: HashMap<String, usize> = HashMap::new();
    for (i, (name, id)) in entries.iter().enumerate() {
        if let Some(t) = id.toplevel.as_deref() {
            match by_toplevel.get(t) {
                Some(&j) => union(&mut parent, i, j),
                None => {
                    by_toplevel.insert(t, i);
                }
            }
        }
        if let Some(o) = id.origin.as_deref() {
            match by_origin.get(o) {
                Some(&j) => union(&mut parent, i, j),
                None => {
                    by_origin.insert(o, i);
                }
            }
        }
        if let Some(c) = id.common_dir.as_deref() {
            match by_common_dir.get(c) {
                Some(&j) => union(&mut parent, i, j),
                None => {
                    by_common_dir.insert(c, i);
                }
            }
        }
        if id.toplevel.is_none() && id.origin.is_none() && id.common_dir.is_none() {
            let lname = name.to_lowercase();
            match by_name.get(&lname) {
                Some(&j) => union(&mut parent, i, j),
                None => {
                    by_name.insert(lname, i);
                }
            }
        }
    }

    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }

    let mut families: Vec<Family> = groups
        .into_values()
        .map(|idxs| {
            let mut members: Vec<String> = idxs.iter().map(|&i| entries[i].0.clone()).collect();
            members.sort();
            // F6 fix (Codex review pass 1 / main-thread finding): name the
            // family after the repo's own toplevel directory basename —
            // git truth — when any member resolved one, rather than the
            // shortest raw project key. The shortest-key rule named a real
            // `claude-self-reflect` family "csr-pr272" purely because a
            // stray PR-review worktree/branch checkout key happened to be
            // the shortest string in the group — an artifact of which
            // branch a session's cwd carried that day, never a repo
            // identity, and (worse) not even guaranteed to be a hyphen-
            // prefix of the family's other members the way the basename
            // usually is (real checkouts are conventionally named after
            // their repo). Deterministic: the lexicographically smallest
            // resolved toplevel path wins when more than one member
            // resolved one (e.g. a worktree's own toplevel differs from
            // its main checkout's even though both share this family via
            // `origin`/`common_dir`).
            let toplevel_name = idxs
                .iter()
                .filter_map(|&i| entries[i].1.toplevel.as_deref())
                .min()
                .and_then(|t| std::path::Path::new(t).file_name())
                .map(|n| n.to_string_lossy().to_lowercase());
            let name = toplevel_name.unwrap_or_else(|| {
                members
                    .iter()
                    .map(|m| m.to_lowercase())
                    .min_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)))
                    .expect("group is never empty")
            });
            Family { name, members }
        })
        .collect();
    families.sort_by(|a, b| a.name.cmp(&b.name));
    families
}

/// The family whose member set (or name) contains `project`,
/// case-insensitively — the CLI's `--project <p>` resolution.
pub fn family_containing<'a>(families: &'a [Family], project: &str) -> Option<&'a Family> {
    let lp = project.to_lowercase();
    families
        .iter()
        .find(|f| f.name == lp || f.members.iter().any(|m| m.to_lowercase() == lp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn ri(toplevel: Option<&str>, origin: Option<&str>) -> RepoIdentity {
        RepoIdentity {
            toplevel: toplevel.map(String::from),
            origin: origin.map(String::from),
            common_dir: None,
        }
    }

    fn fam_names(entries: Vec<(&str, RepoIdentity)>) -> Vec<(String, Vec<String>)> {
        group_keys(
            entries
                .into_iter()
                .map(|(k, id)| (k.to_string(), id))
                .collect(),
        )
        .into_iter()
        .map(|f| (f.name, f.members))
        .collect()
    }

    #[test]
    fn canon_file_collapses_worktree_segment() {
        assert_eq!(
            canon_file("/u/projects/repo/.claude/worktrees/p6-theme/csr-engine/src/x.rs"),
            "/u/projects/repo/csr-engine/src/x.rs"
        );
    }

    #[test]
    fn canon_file_identity_without_segment() {
        assert_eq!(
            canon_file("/u/projects/repo/csr-engine/src/x.rs"),
            "/u/projects/repo/csr-engine/src/x.rs"
        );
    }

    #[test]
    fn canon_file_bare_worktree_dir_untouched() {
        assert_eq!(
            canon_file("/u/projects/repo/.claude/worktrees/p6"),
            "/u/projects/repo/.claude/worktrees/p6"
        );
    }

    // -----------------------------------------------------------------
    // A-c: repo-identity grouping (replaces the hyphen-prefix heuristic)
    // -----------------------------------------------------------------

    #[test]
    fn same_toplevel_merges_a_cwd_subdir_key_with_its_parent_repo() {
        // claude-self-reflect + claude-self-reflect-csr-engine: a session's
        // cwd sitting one directory deeper inside the SAME repo resolves to
        // the SAME toplevel — must merge.
        let fams = fam_names(vec![
            ("claude-self-reflect", ri(Some("/repo"), None)),
            ("claude-self-reflect-csr-engine", ri(Some("/repo"), None)),
        ]);
        assert_eq!(fams.len(), 1);
        assert_eq!(fams[0].1.len(), 2);
    }

    #[test]
    fn shared_hyphen_prefix_with_different_toplevel_never_merges() {
        // anukriti vs anukriti-website: two DIFFERENT real repos that merely
        // share a hyphen-delimited name root. The old heuristic merged
        // these; A-c must not.
        let fams = fam_names(vec![
            (
                "anukriti",
                ri(Some("/repo/anukriti"), Some("host/org/anukriti")),
            ),
            (
                "anukriti-website",
                ri(
                    Some("/repo/anukriti-website"),
                    Some("host/org/anukriti-website"),
                ),
            ),
        ]);
        assert_eq!(fams.len(), 2);
        assert!(fams
            .iter()
            .find(|(_, m)| m.contains(&"anukriti".to_string()))
            .is_some_and(|(_, m)| !m.contains(&"anukriti-website".to_string())));
    }

    #[test]
    fn same_origin_merges_a_worktree_despite_a_different_toplevel() {
        // A linked git worktree's own `--show-toplevel` differs from the
        // main checkout's, but both share one `origin` remote — the
        // secondary edge must still merge them.
        let fams = fam_names(vec![
            ("repo", ri(Some("/main"), Some("host/org/repo"))),
            ("repo-worktrees", ri(Some("/wt"), Some("host/org/repo"))),
        ]);
        assert_eq!(fams.len(), 1);
        assert_eq!(fams[0].1.len(), 2);
    }

    #[test]
    fn unresolved_keys_fall_back_to_exact_case_insensitive_equality_never_prefix() {
        let fams = fam_names(vec![
            ("Acme-Campaigns", ri(None, None)),
            ("acme-campaigns", ri(None, None)),
            ("acme", ri(None, None)), // must NOT merge via prefix
        ]);
        assert_eq!(fams.len(), 2, "acme must stay separate from acme-campaigns");
        let acme_campaigns = fams
            .iter()
            .find(|(n, _)| n == "acme-campaigns")
            .expect("acme-campaigns family");
        assert_eq!(acme_campaigns.1.len(), 2);
    }

    #[test]
    fn resolved_and_unresolved_keys_never_merge_by_name_alone() {
        // A resolved key and an unresolved key sharing a name root must not
        // merge just because the unresolved one has "nowhere else to go".
        let fams = fam_names(vec![
            ("acme", ri(Some("/repo/acme"), None)),
            ("acme-orphaned", ri(None, None)),
        ]);
        assert_eq!(fams.len(), 2);
    }

    #[test]
    fn family_name_uses_the_repo_toplevel_basename_not_the_shortest_member_key() {
        // F6 fix (Codex review pass 1 / main-thread finding): a real
        // "claude-self-reflect" family was DISPLAY-named "csr-pr272" purely
        // because that stray PR-review worktree key was the shortest
        // string in the group. The family name must come from git truth
        // (the repo's own toplevel directory basename) whenever any member
        // resolved one, never from raw key length.
        let fams = fam_names(vec![
            (
                "claude-self-reflect",
                ri(Some("/home/u/claude-self-reflect"), None),
            ),
            ("csr-pr272", ri(Some("/home/u/claude-self-reflect"), None)),
        ]);
        assert_eq!(fams.len(), 1);
        assert_eq!(fams[0].0, "claude-self-reflect");
        assert!(fams[0].1.contains(&"csr-pr272".to_string()));
    }

    #[test]
    fn family_name_falls_back_to_shortest_key_when_nothing_resolved() {
        // Unresolved keys only ever merge by exact case-insensitive name
        // equality (never prefix) -- these two ARE the same key modulo
        // case, so with no toplevel resolved anywhere, the old
        // shortest-string rule is still the only available naming evidence.
        let fams = fam_names(vec![
            ("Acme-Campaigns", ri(None, None)),
            ("acme-campaigns", ri(None, None)),
        ]);
        assert_eq!(fams.len(), 1);
        assert_eq!(fams[0].0, "acme-campaigns");
    }

    #[test]
    fn common_dir_merges_a_worktree_with_no_origin_configured_at_all() {
        // F4 fix: `origin` alone under-merges a purely local worktree setup
        // (no remote configured anywhere) -- `common_dir` must still unite
        // them.
        let fams = fam_names(vec![
            (
                "repo",
                RepoIdentity {
                    toplevel: Some("/main".into()),
                    origin: None,
                    common_dir: Some("/main/.git".into()),
                },
            ),
            (
                "repo-worktrees",
                RepoIdentity {
                    toplevel: Some("/wt".into()),
                    origin: None,
                    common_dir: Some("/main/.git".into()),
                },
            ),
        ]);
        assert_eq!(fams.len(), 1);
        assert_eq!(fams[0].1.len(), 2);
    }

    // -----------------------------------------------------------------
    // F4 (Codex review pass 1, finding #5): relative candidate paths must
    // never resolve identity — they are cwd-sensitive to THIS PROCESS, not
    // to whatever machine/session recorded them.
    // -----------------------------------------------------------------

    #[test]
    fn resolve_repo_identity_never_trusts_a_relative_candidate_path() {
        // Regardless of what "src/dream/backfill/mod.rs" might resolve to
        // relative to the CURRENT process's cwd (on this very checkout, it
        // would resolve to csr-engine's own repo!), a relative path must
        // never be handed to `repo_root_for_file` at all.
        let identity = resolve_repo_identity(&["src/dream/backfill/mod.rs".to_string()]);
        assert_eq!(
            identity,
            RepoIdentity::default(),
            "a relative-only candidate must resolve to nothing, never the engine's own cwd repo"
        );
    }

    #[test]
    fn resolve_repo_identity_skips_a_leading_relative_candidate_and_uses_a_later_absolute_one() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return; // git unavailable -- fail-soft skip
        }
        std::fs::write(repo.join("a.rs"), "fn a() {}\n").unwrap();
        assert!(commit_all(&repo));
        let abs = repo.join("a.rs").to_string_lossy().to_string();

        let identity = resolve_repo_identity(&["relative/path.rs".to_string(), abs]);
        let expected_top = std::fs::canonicalize(&repo).unwrap_or(repo);
        assert_eq!(
            identity
                .toplevel
                .as_deref()
                .map(|t| std::fs::canonicalize(t).unwrap_or_else(|_| t.into())),
            Some(expected_top)
        );
    }

    // -----------------------------------------------------------------
    // F4: candidate_files_for_project must fall back to anchors even when
    // the ledger returned rows, as long as none of them resolved.
    // -----------------------------------------------------------------

    #[test]
    fn candidate_files_for_project_falls_back_to_anchors_when_ledger_rows_are_all_unresolvable() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return;
        }
        std::fs::write(repo.join("a.rs"), "fn a() {}\n").unwrap();
        assert!(commit_all(&repo));
        let abs = repo.join("a.rs").to_string_lossy().to_string();

        let conn = Connection::open_in_memory().unwrap();
        crate::storage::migrations::run(&conn).unwrap();
        // A ledger row whose file no longer exists ANYWHERE on this
        // machine -- the old version's fallback never even ran once the
        // ledger returned ANY row, however unresolvable.
        seed_witness_file(&conn, "proj", Path::new("/nowhere/gone.rs"));
        let anchors_json = serde_json::json!([{ "file": abs }]).to_string();
        conn.execute(
            "INSERT INTO episode_index (episode_id, session_id, project, ts, outcome, anchors_json) \
             VALUES ('ep-1', 'sess', 'proj', '2026-01-01T00:00:00Z', 'done', ?1)",
            rusqlite::params![anchors_json],
        )
        .unwrap();

        let files =
            candidate_files_for_project(&conn, "proj", CANDIDATE_FILE_SAMPLE_LIMIT).unwrap();
        let identity = resolve_repo_identity(&files);
        let expected_top = std::fs::canonicalize(&repo).unwrap_or(repo);
        assert_eq!(
            identity
                .toplevel
                .as_deref()
                .map(|t| std::fs::canonicalize(t).unwrap_or_else(|_| t.into())),
            Some(expected_top),
            "the anchor-derived candidate must still be tried and resolve, even though the ledger returned a (stale) row first"
        );
    }

    #[test]
    fn unrelated_keys_stay_separate() {
        // Under the old hyphen-prefix heuristic "beta-sub" merged into
        // "beta" by name alone. A-c removes that edge entirely: with no
        // repo identity resolved for any of these three, only exact
        // case-insensitive equality merges -- all three stay distinct.
        let fams = fam_names(vec![
            ("alpha", ri(None, None)),
            ("beta", ri(None, None)),
            ("beta-sub", ri(None, None)),
        ]);
        assert_eq!(fams.len(), 3);
    }

    #[test]
    fn family_containing_matches_members_case_insensitively() {
        let families = group_keys(vec![
            ("Acme-Campaigns".into(), ri(None, None)),
            ("acme".into(), ri(None, None)),
        ]);
        assert_eq!(
            family_containing(&families, "ACME-campaigns").map(|f| f.name.as_str()),
            Some("acme-campaigns")
        );
        assert!(family_containing(&families, "other").is_none());
    }

    // -----------------------------------------------------------------
    // A-c integration: compute_families over a real DB + real git repos.
    // -----------------------------------------------------------------

    fn strip_git_env(cmd: &mut Command) {
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(&k);
            }
        }
    }

    /// `git init -q <dir>`. Returns `false` (fail-soft skip) if `git` is
    /// unavailable in this environment.
    fn init_repo(dir: &Path) -> bool {
        std::fs::create_dir_all(dir).unwrap();
        let mut cmd = Command::new("git");
        strip_git_env(&mut cmd);
        cmd.arg("init").arg("-q").arg(dir);
        cmd.status().map(|s| s.success()).unwrap_or(false)
    }

    fn commit_all(dir: &Path) -> bool {
        let run = |args: &[&str]| -> bool {
            let mut cmd = Command::new("git");
            strip_git_env(&mut cmd);
            cmd.arg("-C").arg(dir).args(args);
            cmd.status().map(|s| s.success()).unwrap_or(false)
        };
        run(&["add", "-A"])
            && run(&[
                "-c",
                "user.email=t@example.com",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "seed",
            ])
    }

    fn seed_witness_file(conn: &Connection, project: &str, file: &Path) {
        use crate::storage::witness_ledger::{insert_witness, WitnessLedgerRow};
        insert_witness(
            conn,
            &WitnessLedgerRow {
                id: 0,
                project: project.to_string(),
                file: file.to_string_lossy().to_string(),
                symbol: None,
                span_start: None,
                span_end: None,
                stamp: "b3:x".to_string(),
                tier: "worktree".to_string(),
                at_oid: None,
                source_kind: "backfill".to_string(),
                source_id: None,
            },
        )
        .unwrap();
    }

    #[test]
    fn compute_families_merges_by_repo_identity_not_name_prefix() {
        let tmp = tempfile::tempdir().unwrap();

        // repo_a hosts both "claude-self-reflect" and a cwd-subdir key
        // ("...-csr-engine") — same toplevel, must merge.
        let repo_a = tmp.path().join("repo_a");
        if !init_repo(&repo_a) {
            return; // git unavailable in this environment — fail-soft skip
        }
        std::fs::write(repo_a.join("main.rs"), "fn a() {}\n").unwrap();
        std::fs::create_dir_all(repo_a.join("csr-engine/src")).unwrap();
        std::fs::write(repo_a.join("csr-engine/src/x.rs"), "fn b() {}\n").unwrap();
        assert!(commit_all(&repo_a));

        // repo_b and repo_c are two UNRELATED repos that happen to share
        // the "anukriti" hyphen-prefix name root — must never merge.
        let repo_b = tmp.path().join("anukriti_checkout");
        let repo_c = tmp.path().join("anukriti_website_checkout");
        assert!(init_repo(&repo_b));
        assert!(init_repo(&repo_c));
        std::fs::write(repo_b.join("a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(repo_c.join("b.rs"), "fn b() {}\n").unwrap();
        assert!(commit_all(&repo_b));
        assert!(commit_all(&repo_c));

        let conn = Connection::open_in_memory().unwrap();
        crate::storage::migrations::run(&conn).unwrap();
        seed_witness_file(&conn, "claude-self-reflect", &repo_a.join("main.rs"));
        seed_witness_file(
            &conn,
            "claude-self-reflect-csr-engine",
            &repo_a.join("csr-engine/src/x.rs"),
        );
        seed_witness_file(&conn, "anukriti", &repo_b.join("a.rs"));
        seed_witness_file(&conn, "anukriti-website", &repo_c.join("b.rs"));

        let families = compute_families(&conn).unwrap();

        let csr = family_containing(&families, "claude-self-reflect")
            .expect("claude-self-reflect resolves to a family");
        assert!(
            csr.members
                .contains(&"claude-self-reflect-csr-engine".to_string()),
            "cwd-subdir key must merge via shared toplevel: {csr:?}"
        );

        let ak = family_containing(&families, "anukriti").expect("anukriti resolves");
        let akw =
            family_containing(&families, "anukriti-website").expect("anukriti-website resolves");
        assert_ne!(
            ak.name, akw.name,
            "two unrelated repos sharing a name root must stay distinct"
        );
        assert!(!ak.members.contains(&"anukriti-website".to_string()));
        assert!(!akw.members.contains(&"anukriti".to_string()));
    }
}
