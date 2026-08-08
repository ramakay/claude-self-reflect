//! Causal (ancestry-based) ordering of commits.
//!
//! Everything here is derived from the commit graph via merge-base
//! reachability — **never** from commit/author/committer timestamps. Git
//! timestamps are attacker- and rebase-controlled (`git commit --date`,
//! `git rebase --committer-date-is-author-date`, clock skew) and are
//! therefore not evidence of anything. Topology is.

use crate::Error;

/// The causal relationship between two commits `a` and `b`, established via
/// merge-base reachability, not wall-clock time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CausalOrder {
    /// `a` is a (transitive) ancestor of `b`: `b` was derived from history
    /// that includes `a`.
    AncestorOf,
    /// `a` is a (transitive) descendant of `b`.
    DescendantOf,
    /// `a` and `b` share no ancestry relationship: neither is reachable
    /// from the other. This covers both "diverged from a common ancestor"
    /// and "entirely unrelated histories".
    Incomparable,
    /// `a` and `b` are the same commit.
    Equal,
}

/// Determine the causal order of `a` relative to `b` using merge-base
/// reachability on `repo`'s commit graph.
///
/// # Errors
/// Returns [`Error::MergeBase`] only for genuine repository-access failures
/// (corrupt object database, I/O error). The "no common ancestor" case
/// (unrelated histories) is not an error — it is reported as
/// `Ok(CausalOrder::Incomparable)`.
pub fn compare(
    repo: &gix::Repository,
    a: gix::ObjectId,
    b: gix::ObjectId,
) -> Result<CausalOrder, Error> {
    if a == b {
        return Ok(CausalOrder::Equal);
    }

    match repo.merge_base(a, b) {
        Ok(base) => {
            let base = base.detach();
            if base == a {
                Ok(CausalOrder::AncestorOf)
            } else if base == b {
                Ok(CausalOrder::DescendantOf)
            } else {
                Ok(CausalOrder::Incomparable)
            }
        }
        Err(gix::repository::merge_base::Error::NotFound { .. }) => Ok(CausalOrder::Incomparable),
        Err(source) => Err(Error::MergeBase {
            one: a,
            two: b,
            source: Box::new(source),
        }),
    }
}
