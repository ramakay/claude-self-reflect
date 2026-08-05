//! Error type for `codewitness`.
//!
//! All failure modes here are *input* problems (bad path, unreadable repo,
//! missing commit) — never a verdict. A witnessed anchor that later
//! disappears from the worktree is not an [`Error`], it is
//! [`crate::Verdict::Vanished`]; see [`crate::Auditor::try_audit`].
//!
//! Every `gix` source error is boxed: `gix`'s own error enums are large
//! (they inline whole sub-error trees), and clippy's `result_large_err`
//! correctly flags an unboxed `Result<_, Error>` as an oversized value to
//! move around on the stack. Boxing keeps `Error` itself small while still
//! preserving the full `std::error::Error::source()` chain.

use std::path::PathBuf;

/// Everything that can go wrong constructing evidence, wrapping the
/// underlying `gix` error types 1:1 (boxed, see module docs) so callers
/// keep full diagnostic detail.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to open repository at {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: Box<gix::open::Error>,
    },

    #[error("failed to discover a repository starting from {path}: {source}")]
    Discover {
        path: PathBuf,
        #[source]
        source: Box<gix::discover::Error>,
    },

    #[error("repository at {0} has no working directory (bare repository); worktree-tier evidence is unavailable")]
    BareRepository(PathBuf),

    #[error("failed to read worktree file {path}: {source}")]
    WorktreeRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("HEAD could not be resolved (unborn repository with no commits yet): {0}")]
    Head(#[source] Box<gix::reference::head_id::Error>),

    #[error("commit {commit} could not be found or decoded: {source}")]
    Commit {
        commit: gix::ObjectId,
        #[source]
        source: Box<gix::object::find::existing::with_conversion::Error>,
    },

    #[error("failed to read the tree of commit {commit}: {source}")]
    Tree {
        commit: gix::ObjectId,
        #[source]
        source: Box<gix::object::commit::Error>,
    },

    #[error("failed to look up path {path:?} in commit {commit}: {source}")]
    TreeLookup {
        commit: gix::ObjectId,
        path: PathBuf,
        #[source]
        source: Box<gix::object::find::existing::Error>,
    },

    #[error("path {path:?} in commit {commit} is not a blob (it is a tree, submodule, or symlink): {source}")]
    NotABlob {
        commit: gix::ObjectId,
        path: PathBuf,
        #[source]
        source: Box<gix::object::try_into::Error>,
    },

    #[error(
        "anchor {path:?} does not exist at commit {commit:?} (commit = None means the worktree)"
    )]
    AnchorMissing {
        path: PathBuf,
        commit: Option<gix::ObjectId>,
    },

    #[error(
        "span {start}..={end} is out of range for {path:?}, which has only {available} line(s)"
    )]
    SpanOutOfRange {
        path: PathBuf,
        start: u32,
        end: u32,
        available: usize,
    },

    #[error("invalid span {start}..={end}: start must be <= end and >= 1")]
    InvalidSpan { start: u32, end: u32 },

    #[error(
        "anchor path {path:?} escapes the repository working directory (absolute path, `..` component, or a symlink resolving outside it)"
    )]
    PathEscape { path: PathBuf },

    #[error("merge-base lookup between {one} and {two} failed: {source}")]
    MergeBase {
        one: gix::ObjectId,
        two: gix::ObjectId,
        #[source]
        source: Box<gix::repository::merge_base::Error>,
    },
}

impl From<gix::reference::head_id::Error> for Error {
    fn from(source: gix::reference::head_id::Error) -> Self {
        Error::Head(Box::new(source))
    }
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;
