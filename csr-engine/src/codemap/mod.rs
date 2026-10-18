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
pub mod identity;
pub mod query;
pub mod scan;
pub mod status;
pub mod store;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod testutil;

pub use discover::{discover, Candidate};
pub use query::{query, CodemapHit};
pub use scan::{rescan_paths, scan_repo, view_changed, PathScanReport, RepoScanReport, ScanOpts};
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
    disabled_value(std::env::var("CSR_NO_CODEMAP").ok().as_deref())
}

fn disabled_value(value: Option<&str>) -> bool {
    value.is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
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
