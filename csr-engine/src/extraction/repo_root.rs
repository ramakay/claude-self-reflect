//! Git-repo-root ("repo identity") resolution for code-graph rows.
//!
//! H8 finding (WP2 Stage 1, receipt R4 in
//! `.plans/2026-07-31-codegraph-shipping-plan.md`): `code_nodes.project`
//! (and `code_evolution.project_name`) is the session's cwd tag, not a
//! repository identity — the SAME git repository checked out/opened from two
//! different working directories (e.g. `claude-self-reflect` and its
//! `csr-engine` subdirectory, each its own session cwd) gets two different
//! `project` labels for one repo. This module adds a second, git-derived
//! identity (`repo_root`) that is stable across cwd/session boundaries: the
//! absolute path git itself reports as the repository's toplevel directory.
//!
//! Fail-soft everywhere, by design: no `git` binary, not inside a repo, the
//! directory no longer exists, any I/O error — all yield `None`, never a
//! guess and never an `Err` the caller has to handle. `project` is never
//! touched by anything in this module.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

/// Cache: directory → resolved repo root (or `None` if not inside a repo /
/// git unavailable / no ancestor `.git` found). Keyed on the directory the
/// lookup ran against, so a bulk backfill touching many files in the same
/// directory only pays the `git` subprocess cost once per directory.
type RootCache = Mutex<HashMap<PathBuf, Option<String>>>;

static CACHE: OnceLock<RootCache> = OnceLock::new();

fn cache() -> &'static RootCache {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolve the git repo root for `file`'s containing directory.
///
/// Primary signal: `git -C <dir> rev-parse --show-toplevel`, cached
/// per-directory in-process. Fallback (covers backfill rows whose file is no
/// longer on disk, so `dir` itself may not exist): walk up `dir`'s ancestors
/// looking for the nearest one containing a `.git` entry (directory or
/// linked-worktree file) that still exists on disk.
///
/// `None` when neither signal resolves anything — never a guess.
pub fn repo_root_for_file(file: &str) -> Option<String> {
    if file.is_empty() {
        return None;
    }
    let path = Path::new(file);
    let dir = path.parent()?.to_path_buf();
    if dir.as_os_str().is_empty() {
        return None;
    }
    repo_root_for_dir(&dir)
}

fn repo_root_for_dir(dir: &Path) -> Option<String> {
    if let Some(hit) = cache().lock().ok().and_then(|g| g.get(dir).cloned()) {
        return hit;
    }

    let root = git_toplevel(dir).or_else(|| walk_up_for_git_dir(dir));

    if let Ok(mut guard) = cache().lock() {
        guard.insert(dir.to_path_buf(), root.clone());
    }
    root
}

/// Spawn `git -C <dir> rev-parse --show-toplevel`. `None` on any failure —
/// `git` missing, `dir` outside a work tree, `dir` doesn't exist, non-UTF8
/// output, etc. Never panics.
///
/// Ambient `GIT_*` environment is stripped: when this process itself runs
/// inside a git hook (git exports `GIT_DIR`/`GIT_INDEX_FILE`/... to hooks),
/// an inherited `GIT_DIR` would override `-C` and report the HOOK's
/// repository toplevel for any `dir` — this resolver answers for the
/// explicit path it was given, never for ambient state.
fn git_toplevel(dir: &Path) -> Option<String> {
    if !dir.is_dir() {
        return None;
    }
    let mut cmd = Command::new("git");
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&k);
        }
    }
    let output = cmd
        .arg("-C")
        .arg(dir)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Backfill fallback: walk up from `dir` looking for the nearest ancestor
/// (including `dir` itself) that contains a `.git` entry — matches
/// `extraction::repo_path::canonical_repo_path`'s ancestor walk, but returns
/// the containing directory (the repo root) rather than rewriting `path`.
/// Deliberately does NOT resolve linked-worktree `.git` files to their main
/// repo root (unlike `repo_path`'s rewrite) — the task this fallback serves
/// is "find *a* git identity for a row whose file may be long gone", not
/// worktree canonicalization; a worktree's own toplevel is still a stable,
/// truthful repo_root for that row.
fn walk_up_for_git_dir(dir: &Path) -> Option<String> {
    let mut cur = Some(dir.to_path_buf());
    while let Some(d) = cur {
        if d.join(".git").exists() {
            // Canonicalize (CodeRabbit PR #279): `git_toplevel` returns the
            // symlink-resolved spelling and `node.file` is stored resolved
            // (`repo_path::canonical_repo_path`); an unresolved root here
            // would fail `strip_prefix` in `relpath_in_repo` and count the
            // symbol as `git_no_repo` instead of attributing it.
            let resolved = std::fs::canonicalize(&d).unwrap_or(d);
            return Some(resolved.to_string_lossy().to_string());
        }
        cur = d.parent().map(|p| p.to_path_buf());
    }
    None
}

/// Cache: directory → resolved repo identity (or `None`). Separate from
/// [`CACHE`] above — that one keys on `--show-toplevel` (a repo's own
/// checkout path, which DIFFERS between a main checkout and each of its
/// linked worktrees); this one keys on `--git-common-dir` (shared by all of
/// them), so the two must never be merged into one cache.
type IdentityCache = Mutex<HashMap<PathBuf, Option<String>>>;

static IDENTITY_CACHE: OnceLock<IdentityCache> = OnceLock::new();

fn identity_cache() -> &'static IdentityCache {
    IDENTITY_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolve a stable identity for the repository (if any) `dir` belongs to:
/// the absolute git COMMON directory. This is one identity for a repository's
/// main checkout, every subdirectory beneath it, AND every one of its linked
/// worktrees — a linked worktree's own `.git` is a FILE pointing at
/// `<main>/.git/worktrees/<name>`, and `--git-common-dir` resolves through
/// that back to the one shared `.git` every worktree points at. Two unrelated
/// repositories that happen to share a leaf directory name (e.g.
/// `/opt/customer-a/app` and `/srv/customer-b/app`) get two different
/// identities, unlike a bare leaf-name or `resolve_project_from_cwd` label.
///
/// `dir` must exist and be a directory — unlike [`repo_root_for_file`]'s
/// deleted-file fallback, this answers for a `cwd` a live session is claiming
/// to have run from right now, not a historical row whose file may be gone.
///
/// Fail-soft everywhere: no `git` binary, `dir` outside any work tree, `dir`
/// missing, any I/O error — all `None`, never a guess. If the `git`
/// subprocess itself cannot run (binary missing), falls back to walking up
/// for the nearest ancestor containing a `.git` DIRECTORY and returns its
/// canonical path; a `.git` FILE (a linked worktree, without `git` available
/// to resolve it back to the shared common dir) yields `None` rather than
/// being treated as its own separate identity.
pub fn repo_identity_for_dir(dir: &Path) -> Option<String> {
    if !dir.is_dir() {
        return None;
    }
    if let Some(hit) = identity_cache()
        .lock()
        .ok()
        .and_then(|g| g.get(dir).cloned())
    {
        return hit;
    }

    let identity = git_common_dir(dir).or_else(|| walk_up_for_git_directory(dir));

    if let Ok(mut guard) = identity_cache().lock() {
        guard.insert(dir.to_path_buf(), identity.clone());
    }
    identity
}

/// Spawn `git -C <dir> rev-parse --path-format=absolute --git-common-dir`.
/// `None` on any failure. Ambient `GIT_*` env is stripped for the same reason
/// as [`git_toplevel`] above — this resolver answers for the explicit `dir`
/// it was given, never for a hook's ambient repository.
fn git_common_dir(dir: &Path) -> Option<String> {
    if !dir.is_dir() {
        return None;
    }
    let mut cmd = Command::new("git");
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&k);
        }
    }
    let output = cmd
        .arg("-C")
        .arg(dir)
        .arg("rev-parse")
        .arg("--path-format=absolute")
        .arg("--git-common-dir")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(canonicalize_or_as_is(trimmed))
}

/// Walk up from `dir` for the nearest ancestor (including `dir` itself) with
/// a `.git` DIRECTORY — never a FILE (a linked worktree's `.git` is a file,
/// and without `git` available there is no way to resolve it back to the
/// shared common dir, so it must answer `None`, not invent a separate
/// identity for it).
fn walk_up_for_git_directory(dir: &Path) -> Option<String> {
    let mut cur = Some(dir.to_path_buf());
    while let Some(d) = cur {
        let candidate = d.join(".git");
        if candidate.is_dir() {
            return Some(canonicalize_or_as_is(&candidate.to_string_lossy()));
        }
        cur = d.parent().map(|p| p.to_path_buf());
    }
    None
}

fn canonicalize_or_as_is(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn resolves_toplevel_for_a_file_inside_a_real_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).unwrap();
        // Strip ambient GIT_* env (present when the test suite runs under a
        // git hook, e.g. pre-commit): an inherited GIT_DIR would make this
        // `git init` target the REAL repository's gitdir, not the temp repo.
        let mut init = Command::new("git");
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                init.env_remove(&k);
            }
        }
        let status = init.arg("init").arg("-q").arg(&repo).status();
        if status.map(|s| !s.success()).unwrap_or(true) {
            return; // git unavailable in this environment — fail-soft test skip
        }
        let file = repo.join("src").join("a.rs");
        fs::write(&file, "fn a() {}\n").unwrap();

        let got = repo_root_for_file(&file.to_string_lossy());
        let expected = fs::canonicalize(&repo).unwrap_or(repo);
        let got_canon = got
            .as_ref()
            .map(|g| fs::canonicalize(g).unwrap_or_else(|_| PathBuf::from(g)));
        assert_eq!(got_canon, Some(expected));
    }

    #[test]
    fn non_git_directory_yields_none() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.rs");
        fs::write(&file, "fn a() {}\n").unwrap();
        assert_eq!(repo_root_for_file(&file.to_string_lossy()), None);
    }

    #[test]
    fn empty_path_yields_none() {
        assert_eq!(repo_root_for_file(""), None);
    }

    #[test]
    fn deleted_file_falls_back_to_git_dir_walk() {
        // The file itself need not exist on disk — only some ancestor
        // directory needs to still be present with a `.git` entry.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(repo.join("src")).unwrap();
        let gone = repo.join("src").join("does_not_exist.rs");

        let got = repo_root_for_file(&gone.to_string_lossy());
        let expected = fs::canonicalize(&repo).unwrap_or(repo);
        let got_canon = got
            .as_ref()
            .map(|g| fs::canonicalize(g).unwrap_or_else(|_| PathBuf::from(g)));
        assert_eq!(got_canon, Some(expected));
    }

    // --- repo_identity_for_dir ---

    /// `git init -q <repo>` with ambient `GIT_*` stripped (see the module-doc
    /// rationale above). Returns `false` — never panics — when `git` itself
    /// is unavailable, so callers can skip cleanly instead of failing.
    fn git_init(repo: &Path) -> bool {
        let mut init = Command::new("git");
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                init.env_remove(&k);
            }
        }
        init.arg("init")
            .arg("-q")
            .arg(repo)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn git(dir: &Path, args: &[&str]) -> bool {
        let mut cmd = Command::new("git");
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(&k);
            }
        }
        cmd.arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn subdir_of_a_repo_shares_the_repo_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("sub")).unwrap();
        if !git_init(&repo) {
            return; // git unavailable — skip cleanly
        }
        let root_identity = repo_identity_for_dir(&repo);
        let sub_identity = repo_identity_for_dir(&repo.join("sub"));
        assert!(root_identity.is_some());
        assert_eq!(root_identity, sub_identity);
    }

    #[test]
    fn linked_worktree_shares_the_main_checkouts_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        if !git_init(&repo) {
            return;
        }
        // A worktree needs at least one commit to branch from.
        fs::write(repo.join("f.txt"), "x").unwrap();
        if !git(&repo, &["add", "-A"]) || !git(&repo, &["commit", "-q", "-m", "init"]) {
            return; // git present but commit failed (no identity configured, etc.) — skip
        }
        let worktree = tmp.path().join("wt");
        if !git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                worktree.to_str().unwrap(),
                "-b",
                "wtbranch",
            ],
        ) {
            return; // git present but worktree add failed — skip cleanly
        }

        let main_identity = repo_identity_for_dir(&repo);
        let worktree_identity = repo_identity_for_dir(&worktree);
        assert!(main_identity.is_some());
        assert_eq!(main_identity, worktree_identity);
    }

    #[test]
    fn two_repos_with_the_same_leaf_name_have_different_identities() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("customer-a").join("app");
        let b = tmp.path().join("customer-b").join("app");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        if !git_init(&a) || !git_init(&b) {
            return;
        }

        let ia = repo_identity_for_dir(&a);
        let ib = repo_identity_for_dir(&b);
        assert!(ia.is_some());
        assert!(ib.is_some());
        assert_ne!(ia, ib);
    }

    #[test]
    fn non_git_directory_has_no_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plain");
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(repo_identity_for_dir(&dir), None);
    }

    #[test]
    fn nonexistent_directory_has_no_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("does-not-exist");
        assert_eq!(repo_identity_for_dir(&gone), None);
    }
}
