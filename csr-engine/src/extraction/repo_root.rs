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
fn git_toplevel(dir: &Path) -> Option<String> {
    if !dir.is_dir() {
        return None;
    }
    let output = Command::new("git")
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn resolves_toplevel_for_a_file_inside_a_real_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("src")).unwrap();
        let status = Command::new("git")
            .arg("init")
            .arg("-q")
            .arg(&repo)
            .status();
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
}
