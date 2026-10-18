//! Code map: a live, per-worktree map of every definition in the user's repos.
//!
//! The code graph (`code_nodes`) only knows symbols a conversation touched. The
//! code map reads the repos directly, so CSR can answer for code no
//! conversation ever mentioned, and say which worktree holds which version of
//! it.
//!
//! Shape of the data:
//!
//! - A **repo** is a git COMMON directory (`codemap_repos`). Every linked
//!   worktree of a checkout shares one common dir; a nested repo or a submodule
//!   has its own.
//! - A **view** is one worktree of a repo: path, branch, HEAD oid
//!   (`codemap_views`), plus the code files it holds (`codemap_view_files`:
//!   path, blob oid, and whether the worktree differs from the index).
//! - A **blob** is file content, keyed by git blob oid. It is parsed once per
//!   extractor version, however many views and paths hold it (`codemap_blobs`,
//!   `codemap_defs`). `codemap_fts` is the keyword index over
//!   (name, signature, path) for every (path, blob) pair some view holds.
//!
//! Module layout: `identity` (repo/worktree identity, git facts through gix),
//! `scan` (full scan, partial rescan, cheap change probe), `store` (all
//! writes), `discover` (which repos to map), `query` / `status` (read side),
//! `cli` (the `csr-engine codemap` subcommand).
//!
//! Safety rules that hold for every function here:
//!
//! - Nothing under `~/Desktop`, `~/Documents` or `~/Downloads` is ever stat'ed,
//!   opened or walked: macOS pops a privacy prompt for a signed binary that
//!   touches them. The check is lexical (`identity::is_tcc_path`) and runs
//!   BEFORE any filesystem call on the path.
//! - Repos are only read. No git command runs, and the index is never
//!   refreshed or written.
//! - `CSR_NO_CODEMAP=1` turns the feature off (`disabled()`).

pub mod cli;
pub mod discover;
pub mod files;
pub mod fresh;
pub mod identity;
pub mod lock;
pub mod query;
pub mod scan;
pub mod status;
pub mod store;

#[cfg(test)]
mod files_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) mod testutil;

pub use discover::{discover, Candidate};
pub use files::{
    files_for, lookup_files, lookup_files_scoped, repos_matching, CodemapFile, FileDef,
    FilesLookup, RepoScope,
};
pub use fresh::{run_loop, LoopOpts};
pub use query::{query, query_exact, query_exact_scoped, CodemapHit, ViewPresence};
pub use scan::{
    rescan_paths, rescan_paths_with, scan_repo, view_changed, PathScanReport, RepoGuard,
    RepoScanReport, ScanOpts,
};
pub use status::{status_report, CodemapStatus};

/// Version of the codemap row format. Bump when the shape of a def row changes.
const FORMAT_VERSION: &str = "codemap-v1";

/// Extractor identity stamped on every parsed blob: the codemap row format plus
/// the code-graph extractor that produced the defs. Either changing re-parses
/// every blob and garbage-collects the old rows.
pub fn extractor_version() -> &'static str {
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION.get_or_init(|| {
        format!(
            "{FORMAT_VERSION}+{}",
            crate::storage::witness_ledger::WITNESS_EXTRACTOR_VERSION
        )
    })
}

/// Kill switch: `CSR_NO_CODEMAP=1` (or `true`). The daemon job, the watcher,
/// the hook enqueue and the CLI all ask this one function.
pub fn disabled() -> bool {
    #[cfg(test)]
    if let Some(forced) = test_switch::get() {
        return forced;
    }
    disabled_value(std::env::var("CSR_NO_CODEMAP").ok().as_deref())
}

fn disabled_value(value: Option<&str>) -> bool {
    value.is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Test-only override of the kill switch. `cargo test --lib` runs every module in
/// one process, so a test that set `CSR_NO_CODEMAP` with `set_var` turned the map
/// off for every scanner test running at the same moment. The override is
/// thread-local: it is consulted first by `disabled()` on the thread that holds
/// the guard, and no other test sees it.
#[cfg(test)]
pub(crate) mod test_switch {
    use std::cell::Cell;

    thread_local! {
        static FORCED: Cell<Option<bool>> = const { Cell::new(None) };
    }

    pub(super) fn get() -> Option<bool> {
        FORCED.with(Cell::get)
    }

    /// Restores the previous override when dropped.
    pub(crate) struct Guard(Option<bool>);

    impl Drop for Guard {
        fn drop(&mut self) {
            FORCED.with(|f| f.set(self.0));
        }
    }

    /// Force `disabled()` to `value` on this thread until the guard drops.
    pub(crate) fn force(value: bool) -> Guard {
        Guard(FORCED.with(|f| f.replace(Some(value))))
    }
}

/// Nanoseconds since the epoch: the `codemap_queue.enqueued_at` unit.
pub(crate) fn now_nanos() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// Queue an absolute path for the daemon to rescan. A bare insert: no parse, no
/// git, no filesystem call. `should_enqueue` is the gate callers apply first.
pub fn enqueue(conn: &rusqlite::Connection, path: &std::path::Path) -> anyhow::Result<()> {
    let abs = identity::normalize_lexical(path);
    store::queue_push(conn, &abs.to_string_lossy(), now_nanos())
}

/// Should a hook enqueue this path? Absolute, a code extension, outside the
/// protected folders (lexical check, no filesystem call), and the code map is on.
pub fn should_enqueue(path: &std::path::Path, home: Option<&std::path::Path>) -> bool {
    should_enqueue_with(path, home, disabled())
}

fn should_enqueue_with(
    path: &std::path::Path,
    home: Option<&std::path::Path>,
    disabled: bool,
) -> bool {
    !disabled
        && path.is_absolute()
        && crate::extraction::repo_scan::has_code_extension(path)
        && !identity::is_tcc_path(path, home)
}

pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests_mod {
    use super::*;

    #[test]
    fn kill_switch_reads_one_or_true_only() {
        assert!(disabled_value(Some("1")));
        assert!(disabled_value(Some("true")));
        assert!(disabled_value(Some("TRUE")));
        assert!(!disabled_value(Some("0")));
        assert!(!disabled_value(Some("")));
        assert!(!disabled_value(None));
    }

    #[test]
    fn the_test_override_is_thread_local_and_restores_on_drop() {
        let outer = test_switch::force(true);
        assert!(disabled(), "forced on for this thread");
        {
            let _inner = test_switch::force(false);
            assert!(!disabled(), "an inner guard wins");
        }
        assert!(disabled(), "the outer override is back");
        let other = std::thread::spawn(test_switch::get).join().unwrap();
        assert_eq!(other, None, "another thread sees no override");
        drop(outer);
        assert_eq!(test_switch::get(), None);
    }

    #[test]
    fn hook_enqueue_gate_needs_the_switch_on_an_absolute_unprotected_code_path() {
        use std::path::Path;
        let home = Path::new("/Users/nobody");
        assert!(should_enqueue_with(
            Path::new("/w/r/a.rs"),
            Some(home),
            false
        ));
        assert!(
            !should_enqueue_with(Path::new("/w/r/a.rs"), Some(home), true),
            "CSR_NO_CODEMAP=1: nothing is enqueued"
        );
        assert!(!should_enqueue_with(Path::new("a.rs"), Some(home), false));
        assert!(!should_enqueue_with(
            Path::new("/w/r/a.md"),
            Some(home),
            false
        ));
        assert!(!should_enqueue_with(
            Path::new("/Users/nobody/Desktop/p/a.rs"),
            Some(home),
            false
        ));
        assert!(!should_enqueue_with(
            Path::new("/Volumes/x/Downloads/a.rs"),
            Some(Path::new("/Volumes/x")),
            false
        ));
    }

    #[test]
    fn extractor_version_names_the_codegraph_extractor() {
        assert_eq!(
            extractor_version(),
            format!(
                "codemap-v1+{}",
                crate::storage::witness_ledger::WITNESS_EXTRACTOR_VERSION
            )
        );
    }
}
