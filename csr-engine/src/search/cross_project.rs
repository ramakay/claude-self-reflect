use crate::import;
use crate::storage::Storage;
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static PROJECT_DIR_EXISTS: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();

fn cached_is_dir(path: PathBuf) -> bool {
    let cache = PROJECT_DIR_EXISTS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *cache.entry(path.clone()).or_insert_with(|| path.is_dir())
}

fn is_alias_at_root(
    base: &str,
    derived: &str,
    projects_root: &Path,
    is_dir: impl Fn(PathBuf) -> bool,
) -> bool {
    derived
        .strip_prefix(base)
        .is_some_and(|suffix| suffix.starts_with('-') && suffix.len() > 1)
        && is_dir(projects_root.join(base))
        && !is_dir(projects_root.join(derived))
}

fn same_project_family_at_root_with(
    current: &str,
    candidate: &str,
    projects_root: &Path,
    is_dir: impl Fn(PathBuf) -> bool + Copy,
) -> bool {
    current == candidate
        || is_alias_at_root(current, candidate, projects_root, is_dir)
        || is_alias_at_root(candidate, current, projects_root, is_dir)
}

/// Filesystem-root seam for hermetic callers and tests.
pub(crate) fn same_project_family_at_root(
    current: &str,
    candidate: &str,
    projects_root: &Path,
) -> bool {
    same_project_family_at_root_with(current, candidate, projects_root, |path| path.is_dir())
}

/// Cached-`is_dir`, explicit-root variant: same alias semantics and the same
/// process-lifetime `cached_is_dir` memo as `same_project_family`, but the
/// projects root is caller-supplied instead of always `dirs::home_dir()`.
/// Lets a caller like `storage::queries::recent_work_family_projects` inject
/// its own resolved home directory (or `None`) so its no-resolvable-home
/// branch can be tested deterministically without depending on this
/// machine's real `$HOME`, while production keeps the exact same cached
/// filesystem-stat behavior as `same_project_family`.
pub(crate) fn same_project_family_at_cached_root(
    current: &str,
    candidate: &str,
    projects_root: &Path,
) -> bool {
    same_project_family_at_root_with(current, candidate, projects_root, cached_is_dir)
}

/// Whether two stored project namespaces identify the same on-disk project.
///
/// A hyphenated namespace is an alias only when the shorter project directory
/// exists under `~/projects` and the longer name does not. This keeps genuine
/// sibling repositories such as `anukriti-meta-campaigns` separate.
pub fn same_project_family(current: &str, candidate: &str) -> bool {
    if current == candidate {
        return true;
    }
    let Some(projects_root) = dirs::home_dir().map(|home| home.join("projects")) else {
        return false;
    };
    same_project_family_at_cached_root(current, candidate, &projects_root)
}

/// Encode a path the way Claude Code names its `~/.claude/projects` folders.
///
/// Claude Code does `path.replace(/[^a-zA-Z0-9]/g, "-")` (verified in the shipped
/// binary, 2.1.226). That regex carries no `u` flag, so it runs over UTF-16 code
/// units, not scalar values: an accented BMP character costs one dash, but a
/// non-BMP one (emoji, rarer CJK) is a surrogate pair and costs *two*. Mapping per
/// `char` would emit one dash there and miss the stored folder — the same silent
/// empty-result failure this whole branch exists to fix, just narrower.
pub(crate) fn encode_project_folder(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            for _ in 0..c.len_utf16() {
                out.push('-');
            }
        }
    }
    out
}

/// Resolve project name from a CWD path string.
/// Pure function — testable without environment variable manipulation.
pub fn resolve_project_from_cwd(cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }

    // Windows drive path ("D:\Claude", or "\\?\D:\Claude" after canonicalize):
    // stored project names are Claude Code's ~/.claude/projects folder names,
    // which encode the FULL path with every non-alphanumeric char as '-'
    // ("D:\Claude" → "D--Claude"). The last-component fallback below returned
    // "Claude", which matches no stored project and silently emptied every
    // project-scoped search. Hooks receive the project root as cwd, so no
    // component-walking is needed here.
    let stripped = cwd.strip_prefix(r"\\?\").unwrap_or(cwd);
    let trimmed = stripped.trim_end_matches(['\\', '/']);
    if trimmed.len() >= 2 && trimmed.as_bytes()[1] == b':' {
        // At a drive root the separator is the path, not trailing decoration:
        // Claude Code stores `D:\` as `D--`, so trimming down to `D:` would
        // resolve `D-` and miss. Below the root, a trailing separator is just a
        // shape the hook may deliver and is dropped.
        let at_drive_root = trimmed.len() == 2 && stripped.len() > 2;
        let to_encode = if at_drive_root {
            &stripped[..3]
        } else {
            trimmed
        };
        return Some(encode_project_folder(to_encode));
    }

    let path = std::path::Path::new(cwd);
    let dir_name = path.file_name()?.to_string_lossy().to_string();

    // If it looks like a Claude projects directory (dash-separated), normalize it
    if dir_name.starts_with('-') && dir_name.contains("projects") {
        return Some(import::normalize_project_name(&dir_name));
    }

    // Walk up path components to find one after "projects"
    // This handles subdirectories like /Users/name/projects/my-app/src/engine
    let components: Vec<&str> = cwd.split('/').filter(|s| !s.is_empty()).collect();
    for (i, comp) in components.iter().enumerate() {
        if *comp == "projects" && i + 1 < components.len() {
            // The found component can itself be a Claude dash-encoded
            // projects dir name (cwd nested under
            // `~/.claude/projects/<encoded>/memory`, where the literal
            // "projects" segment is `.claude`'s own child, not the encoded
            // project's own parent) — normalize it the same way the
            // primary branch above does. Without this, callers that write
            // storage rows keyed by this project name
            // (`hooks::post_tool_use::resolve_project_for_hook`,
            // `import::coedit_backfill::resolve_project_for_backfill`)
            // produce an unnormalized project_name that a normalized
            // project-scope filter can never match — silently zeroing the
            // reinstatement graph hop for any session whose most-touched
            // file lives under a CSR memory dir.
            return Some(import::normalize_project_name(components[i + 1]));
        }
    }

    // Fallback: last path component
    Some(dir_name)
}

/// Resolve the current project from the `MCP_CLIENT_CWD` environment variable.
///
/// Claude Code sets `MCP_CLIENT_CWD` to the user's working directory when invoking
/// MCP tools. We extract the project name using `resolve_project_from_cwd`.
///
/// Returns `None` if the env var is not set.
///
/// Kept as `MCP_CLIENT_CWD`-only on purpose: this feeds the `project="all"`
/// same-family ranking boost (`CURRENT_PROJECT_ALL_SCOPE_BOOST` in
/// `mcp::tools`), and widening its source to `CLAUDE_PROJECT_DIR` would
/// silently change ranking for every explicit `project="all"` call from
/// Claude Code. Callers that need a default scope for an *omitted* project
/// should use [`resolve_client_dir`] / [`resolve_verified_default_scope_project`]
/// instead — see their docs for why this function alone left every Claude
/// Code stdio search unscoped.
pub fn resolve_current_project() -> Option<String> {
    let cwd = std::env::var("MCP_CLIENT_CWD").ok()?;
    resolve_project_from_cwd(&cwd)
}

/// [`resolve_client_dir`] with the two env lookups passed in as a closure —
/// the seam a test uses to pin `MCP_CLIENT_CWD`/`CLAUDE_PROJECT_DIR`
/// precedence and the empty-value filter without mutating process env vars,
/// which race with every other test in this binary running in the same
/// process.
pub(crate) fn resolve_client_dir_from(get: impl Fn(&str) -> Option<String>) -> Option<String> {
    get("MCP_CLIENT_CWD")
        .filter(|v| !v.is_empty())
        .or_else(|| get("CLAUDE_PROJECT_DIR").filter(|v| !v.is_empty()))
}

/// Resolve the MCP client's working directory: `MCP_CLIENT_CWD` first (what
/// a spec-following MCP client sets), falling back to `CLAUDE_PROJECT_DIR`.
///
/// Claude Code's stdio MCP transport never sets `MCP_CLIENT_CWD` — verified
/// live against running `csr-engine` server processes launched by Claude
/// Code, which had `CLAUDE_PROJECT_DIR` set and no `MCP_CLIENT_CWD` at all —
/// so [`resolve_current_project`] alone always resolved to `None` there and
/// every unscoped search ran as `project=all`. Claude Code does set
/// `CLAUDE_PROJECT_DIR` on every MCP server process it spawns. An empty
/// value from either variable (`VAR=`) is treated as absent so a blank
/// export can't silently opt a session into a bogus scope.
pub fn resolve_client_dir() -> Option<String> {
    resolve_client_dir_from(|key| std::env::var(key).ok())
}

/// Every plausible stored-project name for a client directory `dir`: the
/// `~/projects/<name>` heuristic [`resolve_project_from_cwd`] assumes, and
/// the exact name Claude Code's own importer would give it — encode the
/// full path the way Claude Code encodes its `~/.claude/projects` folder
/// names ([`encode_project_folder`]), then run the same
/// [`import::normalize_project_name`] every discovered folder goes through.
///
/// The two agree for a repo that sits directly under `~/projects`. Outside
/// that layout (`~/code/foo`, `/tmp/x`, `~/.claude-self-reflect`) or with a
/// `.`/`_`/space in the repo name, they diverge — `resolve_project_from_cwd`
/// returns the raw last path component, unnormalized, which matches no
/// stored project. Both candidates are returned so the caller can verify
/// each against what is actually in storage before picking one.
fn candidate_default_scope_names(dir: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    if let Some(heuristic) = resolve_project_from_cwd(dir) {
        candidates.push(heuristic);
    }
    let trimmed = dir.trim_end_matches('/');
    let imported = import::normalize_project_name(&encode_project_folder(trimmed));
    if !imported.is_empty() && !candidates.contains(&imported) {
        candidates.push(imported);
    }
    candidates
}

/// Shared core of [`resolve_verified_default_scope_project`] /
/// [`resolve_verified_default_scope_project_at_root`]: try each candidate
/// name in turn and accept the first one that is either a known project or
/// shares a project family with one. Neither candidate needs to be a
/// perfect match — family widening happens later in `SearchProjectScope` —
/// this only rejects a candidate certain to match nothing, which is what
/// used to leave every unscoped Claude Code search empty for a repo outside
/// `~/projects` (see [`candidate_default_scope_names`]).
fn resolve_verified_default_scope_project_via(
    dir: &str,
    known_projects: &[String],
    is_family: impl Fn(&str, &str) -> bool,
) -> Option<String> {
    candidate_default_scope_names(dir)
        .into_iter()
        .find(|candidate| {
            known_projects
                .iter()
                .any(|known| is_family(candidate, known))
        })
}

/// [`resolve_verified_default_scope_project`] with an injectable projects
/// root — the seam tests use so alias resolution never depends on this
/// machine's real `$HOME/projects` layout.
pub(crate) fn resolve_verified_default_scope_project_at_root(
    dir: &str,
    known_projects: &[String],
    projects_root: &Path,
) -> Option<String> {
    resolve_verified_default_scope_project_via(dir, known_projects, |a, b| {
        same_project_family_at_root(a, b, projects_root)
    })
}

/// The project an *omitted* `project` argument should default to, verified
/// against `known_projects` (normally `Storage::list_project_names`):
/// `None` when neither env var is set (or set to an empty string), or when
/// neither candidate name in [`candidate_default_scope_names`] matches a
/// known project or its family — the caller then falls back to
/// `project=all` rather than a scope certain to be empty.
pub fn resolve_verified_default_scope_project(
    dir: &str,
    known_projects: &[String],
) -> Option<String> {
    resolve_verified_default_scope_project_via(dir, known_projects, same_project_family)
}

/// Normalize a project scope parameter.
///
/// - `None` → the client's default scope (see [`resolve_verified_default_scope_project`])
/// - `Some("all")` (any case) → `None` (search all projects)
/// - `Some(name)` → `Some(name)` (specific project)
///
/// Returns `(effective_project, scope_label)` where scope_label is for display.
pub fn normalize_project_scope(
    storage: &Storage,
    project: Option<&str>,
) -> Result<(Option<String>, String)> {
    normalize_project_scope_at_root(storage, project, None)
}

/// [`normalize_project_scope`] with an injectable projects-root — the seam a
/// test uses to exercise family-alias widening without depending on this
/// machine's real `$HOME/projects` layout.
pub fn normalize_project_scope_at_root(
    storage: &Storage,
    project: Option<&str>,
    projects_root_override: Option<&Path>,
) -> Result<(Option<String>, String)> {
    let known_projects = storage.list_project_names("", i64::MAX as usize)?;
    Ok(normalize_project_scope_with(
        project,
        resolve_client_dir().as_deref(),
        &known_projects,
        projects_root_override,
    ))
}

/// Same as [`normalize_project_scope`] with the client directory and known
/// projects passed in explicitly — the seam tests use so the result doesn't
/// depend on process env vars (which race with every other test in this
/// binary) or storage I/O.
pub fn normalize_project_scope_with(
    project: Option<&str>,
    client_dir: Option<&str>,
    known_projects: &[String],
    projects_root_override: Option<&Path>,
) -> (Option<String>, String) {
    match project {
        Some(p) if p.eq_ignore_ascii_case("all") => (None, "all".to_string()),
        Some(p) if !p.is_empty() => (Some(p.to_string()), p.to_string()),
        _ => {
            let default = client_dir.and_then(|dir| match projects_root_override {
                Some(root) => {
                    resolve_verified_default_scope_project_at_root(dir, known_projects, root)
                }
                None => resolve_verified_default_scope_project(dir, known_projects),
            });
            match default {
                Some(p) => {
                    let label = p.clone();
                    (Some(p), label)
                }
                None => (None, "all".to_string()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `resolve_client_dir_from` — W3-S finding 3: the empty-value filter and
    // MCP_CLIENT_CWD/CLAUDE_PROJECT_DIR precedence, exercised through an
    // injected closure so nothing here mutates process env vars (which race
    // with every other test in this binary).

    #[test]
    fn resolve_client_dir_from_prefers_mcp_client_cwd_when_both_are_set() {
        let dir = resolve_client_dir_from(|key| match key {
            "MCP_CLIENT_CWD" => Some("/Users/n/projects/X".to_string()),
            "CLAUDE_PROJECT_DIR" => Some("/Users/n/projects/Y".to_string()),
            _ => None,
        });
        assert_eq!(dir, Some("/Users/n/projects/X".to_string()));
    }

    /// Revert the `.filter(|v| !v.is_empty())` on the `MCP_CLIENT_CWD` branch
    /// and this fails: `MCP_CLIENT_CWD=''` would short-circuit `or_else` and
    /// return `Some("")` instead of falling through to `CLAUDE_PROJECT_DIR`.
    #[test]
    fn resolve_client_dir_from_empty_mcp_client_cwd_falls_back_to_claude_project_dir() {
        let dir = resolve_client_dir_from(|key| match key {
            "MCP_CLIENT_CWD" => Some(String::new()),
            "CLAUDE_PROJECT_DIR" => Some("/Users/n/projects/X".to_string()),
            _ => None,
        });
        assert_eq!(dir, Some("/Users/n/projects/X".to_string()));
    }

    /// Revert the `.filter(|v| !v.is_empty())` on the `CLAUDE_PROJECT_DIR`
    /// branch and this fails: it would return `Some("")` instead of `None`,
    /// which `resolve_verified_default_scope_project` would then try (and
    /// fail) to resolve as a real directory instead of falling back to
    /// `project=all`.
    #[test]
    fn resolve_client_dir_from_both_empty_is_none() {
        let dir = resolve_client_dir_from(|key| match key {
            "MCP_CLIENT_CWD" => Some(String::new()),
            "CLAUDE_PROJECT_DIR" => Some(String::new()),
            _ => None,
        });
        assert_eq!(dir, None);
    }

    #[test]
    fn resolve_client_dir_from_neither_set_is_none() {
        let dir = resolve_client_dir_from(|_| None);
        assert_eq!(dir, None);
    }

    #[test]
    fn project_family_aliases_require_only_the_base_directory_to_exist() {
        let projects = tempfile::tempdir().unwrap();

        assert!(same_project_family_at_root_with(
            "claude-self-reflect",
            "claude-self-reflect",
            projects.path(),
            |path| path.is_dir()
        ));
        assert!(!same_project_family_at_root_with(
            "claude-self-reflect",
            "claude-self-reflect-csr-engine",
            projects.path(),
            |path| path.is_dir()
        ));

        std::fs::create_dir(projects.path().join("claude-self-reflect-csr-engine")).unwrap();
        assert!(!same_project_family_at_root_with(
            "claude-self-reflect",
            "claude-self-reflect-csr-engine",
            projects.path(),
            |path| path.is_dir()
        ));
        std::fs::remove_dir(projects.path().join("claude-self-reflect-csr-engine")).unwrap();

        std::fs::create_dir(projects.path().join("claude-self-reflect")).unwrap();
        std::fs::create_dir(projects.path().join("claude-self-reflect-csr-engine")).unwrap();
        assert!(!same_project_family_at_root_with(
            "claude-self-reflect",
            "claude-self-reflect-csr-engine",
            projects.path(),
            |path| path.is_dir()
        ));

        std::fs::remove_dir(projects.path().join("claude-self-reflect-csr-engine")).unwrap();
        assert!(same_project_family_at_root_with(
            "claude-self-reflect",
            "claude-self-reflect-csr-engine",
            projects.path(),
            |path| path.is_dir()
        ));
        assert!(same_project_family_at_root_with(
            "claude-self-reflect-csr-engine",
            "claude-self-reflect",
            projects.path(),
            |path| path.is_dir()
        ));
    }

    #[test]
    fn real_hyphenated_project_is_not_swallowed_by_prefix_project() {
        let projects = tempfile::tempdir().unwrap();
        std::fs::create_dir(projects.path().join("anukriti-meta-campaigns")).unwrap();

        assert!(!same_project_family_at_root_with(
            "anukriti",
            "anukriti-meta-campaigns",
            projects.path(),
            |path| path.is_dir()
        ));

        std::fs::remove_dir(projects.path().join("anukriti-meta-campaigns")).unwrap();
        std::fs::create_dir(projects.path().join("anukriti")).unwrap();
        assert!(same_project_family_at_root_with(
            "anukriti",
            "anukriti-meta-campaigns",
            projects.path(),
            |path| path.is_dir()
        ));
    }

    #[test]
    fn test_normalize_scope_all() {
        let storage = Storage::open_memory().unwrap();
        let (project, label) = normalize_project_scope(&storage, Some("all")).unwrap();
        assert!(project.is_none());
        assert_eq!(label, "all");
    }

    #[test]
    fn test_normalize_scope_all_case_insensitive() {
        let storage = Storage::open_memory().unwrap();
        let (project, _) = normalize_project_scope(&storage, Some("ALL")).unwrap();
        assert!(project.is_none());
        let (project, _) = normalize_project_scope(&storage, Some("All")).unwrap();
        assert!(project.is_none());
    }

    #[test]
    fn test_normalize_scope_specific() {
        let storage = Storage::open_memory().unwrap();
        let (project, label) = normalize_project_scope(&storage, Some("my-project")).unwrap();
        assert_eq!(project, Some("my-project".to_string()));
        assert_eq!(label, "my-project");
    }

    #[test]
    fn test_normalize_scope_none_no_env() {
        // When MCP_CLIENT_CWD/CLAUDE_PROJECT_DIR are not set, falls back to "all"
        let storage = Storage::open_memory().unwrap();
        let (_project, label) = normalize_project_scope(&storage, None).unwrap();
        assert!(!label.is_empty());
    }

    #[test]
    fn test_normalize_scope_empty() {
        let storage = Storage::open_memory().unwrap();
        let (project, label) = normalize_project_scope(&storage, Some("")).unwrap();
        assert_eq!(
            label,
            if project.is_some() {
                project.as_deref().unwrap()
            } else {
                "all"
            }
        );
    }

    // `normalize_project_scope_with` takes the client directory, the known
    // projects, and the projects root as plain arguments instead of reading
    // `MCP_CLIENT_CWD`/`CLAUDE_PROJECT_DIR`/storage directly, so these are
    // hermetic: no `std::env::set_var`, no race with whatever order the test
    // binary happens to run in, no dependence on this machine's real
    // `$HOME/projects` layout.

    #[test]
    fn omitted_project_defaults_to_the_client_dirs_project() {
        let known = vec!["SampleApp".to_string()];
        let (project, label) = normalize_project_scope_with(
            None,
            Some("/Users/name/projects/SampleApp"),
            &known,
            None,
        );
        assert_eq!(project, Some("SampleApp".to_string()));
        assert_eq!(label, "SampleApp");
    }

    /// W3-S finding 1: a candidate that matches nothing in storage must fall
    /// back to all-scope, not a scope guaranteed to return zero results.
    #[test]
    fn omitted_project_falls_back_to_all_when_no_candidate_is_known() {
        let known = vec!["totally-unrelated-project".to_string()];
        let (project, label) = normalize_project_scope_with(
            None,
            Some("/Users/name/projects/SampleApp"),
            &known,
            None,
        );
        assert!(project.is_none());
        assert_eq!(label, "all");
    }

    /// W3-S finding 1 receipt: `CLAUDE_PROJECT_DIR=/private/tmp/csr-s1-proof`
    /// has no `projects` path component, so `resolve_project_from_cwd` alone
    /// derives `csr-s1-proof` — which matches nothing — instead of the
    /// stored `-private-tmp-csr-s1-proof` (Claude Code's own dash-encoding
    /// of the full path). Before this fix every unscoped search under a repo
    /// outside `~/projects` silently returned nothing.
    #[test]
    fn omitted_project_matches_the_importers_encoding_outside_projects_root() {
        let known = vec!["-private-tmp-csr-s1-proof".to_string()];
        let (project, label) =
            normalize_project_scope_with(None, Some("/private/tmp/csr-s1-proof"), &known, None);
        assert_eq!(project, Some("-private-tmp-csr-s1-proof".to_string()));
        assert_eq!(label, "-private-tmp-csr-s1-proof");
    }

    /// A dotted/underscored repo name that DOES sit under a `projects` root:
    /// the plain `resolve_project_from_cwd` walk-up branch returns the raw
    /// component (`my.repo_name`, dots intact), but Claude Code's own
    /// encoding turns every non-alnum character to `-`
    /// (`my-repo-name`) before importing it. Only the importer-encoded
    /// candidate matches what is actually stored.
    #[test]
    fn omitted_project_matches_the_importers_encoding_for_a_dotted_repo_name() {
        let projects_root = tempfile::tempdir().unwrap();
        let dir = projects_root
            .path()
            .join("projects")
            .join("my.repo_name")
            .to_string_lossy()
            .to_string();
        let known = vec!["my-repo-name".to_string()];
        let (project, label) =
            normalize_project_scope_with(None, Some(&dir), &known, Some(projects_root.path()));
        assert_eq!(project, Some("my-repo-name".to_string()));
        assert_eq!(label, "my-repo-name");
    }

    #[test]
    fn explicit_all_stays_global_even_with_a_client_dir() {
        let known = vec!["SampleApp".to_string()];
        let (project, label) = normalize_project_scope_with(
            Some("all"),
            Some("/Users/name/projects/SampleApp"),
            &known,
            None,
        );
        assert!(project.is_none());
        assert_eq!(label, "all");
    }

    #[test]
    fn no_client_dir_and_no_project_is_all() {
        let (project, label) = normalize_project_scope_with(None, None, &[], None);
        assert!(project.is_none());
        assert_eq!(label, "all");
    }

    #[test]
    fn empty_client_dir_string_is_treated_as_absent() {
        let (project, label) = normalize_project_scope_with(None, Some(""), &[], None);
        assert!(project.is_none());
        assert_eq!(label, "all");
    }

    #[test]
    fn explicit_project_wins_over_client_dir() {
        let known = vec!["SampleApp".to_string()];
        let (project, label) = normalize_project_scope_with(
            Some("other-project"),
            Some("/Users/name/projects/SampleApp"),
            &known,
            None,
        );
        assert_eq!(project, Some("other-project".to_string()));
        assert_eq!(label, "other-project");
    }

    // `resolve_verified_default_scope_project[_at_root]` directly — the
    // helper `normalize_project_scope_with` and `SearchProjectScope` both
    // build on.

    #[test]
    fn verified_default_scope_falls_back_to_all_when_neither_candidate_matches() {
        let known = vec!["totally-unrelated".to_string()];
        let resolved = resolve_verified_default_scope_project("/private/tmp/csr-s1-proof", &known);
        assert_eq!(resolved, None);
    }

    #[test]
    fn verified_default_scope_matches_the_importers_encoding_outside_projects_root() {
        let known = vec!["-private-tmp-csr-s1-proof".to_string()];
        let resolved = resolve_verified_default_scope_project("/private/tmp/csr-s1-proof", &known);
        assert_eq!(resolved, Some("-private-tmp-csr-s1-proof".to_string()));
    }

    #[test]
    fn verified_default_scope_at_root_matches_a_family_alias() {
        let projects_root = tempfile::tempdir().unwrap();
        std::fs::create_dir(projects_root.path().join("SampleApp")).unwrap();
        let known = vec!["SampleApp-csr-engine".to_string()];
        let resolved = resolve_verified_default_scope_project_at_root(
            "/Users/name/projects/SampleApp",
            &known,
            projects_root.path(),
        );
        assert_eq!(resolved, Some("SampleApp".to_string()));
    }

    // Pure function tests — no env var manipulation needed
    #[test]
    fn test_resolve_from_cwd_simple_project() {
        let result = resolve_project_from_cwd("/Users/name/projects/claude-self-reflect");
        assert_eq!(result, Some("claude-self-reflect".to_string()));
    }

    #[test]
    fn test_resolve_from_cwd_subdirectory() {
        let result =
            resolve_project_from_cwd("/Users/name/projects/claude-self-reflect/src/engine");
        assert_eq!(result, Some("claude-self-reflect".to_string()));
    }

    #[test]
    fn test_resolve_from_cwd_claude_dir_format() {
        let result = resolve_project_from_cwd(
            "/Users/name/.claude/projects/-Users-name-projects-claude-self-reflect",
        );
        assert_eq!(result, Some("claude-self-reflect".to_string()));
    }

    #[test]
    fn test_resolve_from_cwd_empty() {
        assert_eq!(resolve_project_from_cwd(""), None);
    }

    #[test]
    fn test_resolve_from_cwd_no_projects_segment() {
        let result = resolve_project_from_cwd("/tmp/something/mydir");
        assert_eq!(result, Some("mydir".to_string()));
    }

    /// Pins the stored project-name contract for Windows drive paths.
    ///
    /// Deliberately NOT `#[cfg(windows)]`-gated: the code path under test is pure
    /// string handling with no platform-conditional code, so gating it would only
    /// hide the regression from the CI that actually runs — which has no Windows
    /// job. A hook can deliver `cwd` raw, `\\?\`-canonicalized, with a trailing
    /// separator, or any combination of those, and every shape has to land on the
    /// same stored name or project-scoped search silently returns nothing.
    #[test]
    fn test_resolve_from_cwd_windows_drive_paths() {
        for cwd in [
            r"D:\Claude",
            r"\\?\D:\Claude",
            "D:\\Claude\\",
            "\\\\?\\D:\\Claude\\",
        ] {
            assert_eq!(
                resolve_project_from_cwd(cwd),
                Some("D--Claude".to_string()),
                "cwd {cwd:?} must encode to the stored project name"
            );
        }

        // The encoding covers the whole path, not just the last component — that
        // was the original bug: "D:\Claude" resolved to "Claude", which matches no
        // stored project.
        assert_eq!(
            resolve_project_from_cwd(r"D:\Proyectos\claude-self-reflect"),
            Some("D--Proyectos-claude-self-reflect".to_string())
        );
    }

    /// A project sitting at the drive root. The separator there is part of the
    /// path — `D:\` is stored as `D--`, not `D-` — so the trailing-separator trim
    /// must stop at the root.
    #[test]
    fn test_resolve_from_cwd_drive_root() {
        for cwd in [r"D:\", "D:/", r"\\?\D:\", r"D:\\\\"] {
            assert_eq!(
                resolve_project_from_cwd(cwd),
                Some("D--".to_string()),
                "cwd {cwd:?} is the drive root"
            );
        }

        // A bare drive letter carries no separator to preserve.
        assert_eq!(resolve_project_from_cwd("D:"), Some("D-".to_string()));
    }

    /// Claude Code's encoder is `replace(/[^a-zA-Z0-9]/g, "-")` with no `u` flag,
    /// so it counts UTF-16 code units: one dash for a BMP character, two for a
    /// surrogate pair. Non-ASCII project directories are ordinary outside English
    /// locales, so getting this wrong reproduces the original miss on exactly the
    /// paths least likely to be reported.
    #[test]
    fn test_resolve_from_cwd_non_ascii_drive_paths() {
        // Every non-ASCII character here is BMP: one code unit, one dash.
        assert_eq!(
            resolve_project_from_cwd(r"D:\Proyectos\Español"),
            Some("D--Proyectos-Espa-ol".to_string())
        );
        // ':' + '\' + 7 Cyrillic + '\' + 3 Cyrillic = 13 code units after "C".
        assert_eq!(
            resolve_project_from_cwd(r"C:\Проекты\бот"),
            Some(format!("C{}", "-".repeat(13)))
        );

        // A non-BMP character is a surrogate pair: two code units, two dashes.
        assert_eq!(
            resolve_project_from_cwd("D:\\🚀x"),
            Some("D----x".to_string())
        );
    }

    #[test]
    fn test_resolve_from_cwd_nested_under_claude_memory_dir() {
        // A CSR memory-file's own path is
        // ~/.claude/projects/<dash-encoded-project>/memory/<file>.md — its
        // immediate parent dir name is "memory" (doesn't start with '-'),
        // so this must resolve via the walk-up branch, not the primary
        // dash-encoded-dirname branch. The walk-up branch must normalize
        // the encoded component the same way, or code_evolution rows
        // written for memory-file touches (live hook + coedit backfill)
        // get a project_name that never matches a normalized project
        // scope.
        let result = resolve_project_from_cwd(
            "/Users/name/.claude/projects/-Users-name-projects-claude-self-reflect/memory",
        );
        assert_eq!(result, Some("claude-self-reflect".to_string()));
    }
}
