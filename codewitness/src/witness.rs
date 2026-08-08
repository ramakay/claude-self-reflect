use crate::{Anchor, Stamp};

/// The evidentiary strength of a [`Witness`].
///
/// This is the axis that gates [`crate::Verdict::Superseded`]: a receipt is
/// a claim that a specific commit replaced an anchor, and only committed
/// history can support that claim. A dirty worktree cannot supersede
/// anything because it isn't part of the causal graph yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Tier {
    /// Stamped from the live filesystem. May not correspond to any commit
    /// (uncommitted changes, untracked files, mid-rebase state).
    Worktree,
    /// Stamped from a specific commit's tree. Fully reproducible from the
    /// object database alone.
    Committed,
}

/// A claim, frozen in time: "at `at`, `anchor` had content matching `stamp`."
///
/// Witnesses are inert data — constructing one makes no claim about whether
/// it is still true. [`crate::Auditor::try_audit`] is what turns a
/// `Witness` into a [`crate::Verdict`].
///
/// # Tier discipline is type-enforced
///
/// Fields are private and reached only through the getters below.
/// [`Witness::new`] can only produce [`Tier::Worktree`] evidence — it is
/// the sole public constructor. A [`Tier::Committed`] witness (the only
/// tier from which [`crate::Verdict::Superseded`] may be minted, see
/// [`crate::Auditor::audit_against_successor`]) can only be produced by
/// [`crate::Auditor::stamp_at`] / [`crate::Auditor::stamp_normalized_at`],
/// which read the commit's tree themselves and so can actually vouch for
/// the claim. There is no public way to hand-construct a `Witness` that
/// claims `Tier::Committed` without an `Auditor` having verified it.
///
/// Caveat: with the `serde` feature enabled, `Deserialize` reconstructs a
/// `Witness` field-for-field (derive macros expand inside this module, so
/// they see the private fields) — a `Witness` rehydrated from untrusted
/// storage is exactly as trustworthy as the storage it came from.
///
/// **Invariant**: any `Witness` obtained via serde deserialization is an
/// *unverified claim*, never evidence. Every field — `tier`, `stamp`,
/// `at` — may have been forged in storage. This is safe because
/// [`crate::Auditor::audit_against_successor`] never trusts a successor
/// `Witness` at face value: it re-derives the successor's stamp from the
/// claimed commit's content in the object database and compares it
/// byte-for-byte before minting [`crate::Verdict::Superseded`]. A forged
/// deserialized witness therefore cannot mint a `Superseded` verdict —
/// the serde-gated forgery tests in `tests/adversarial.rs`
/// (`successor_forgery`) assert exactly this.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Witness {
    anchor: Anchor,
    stamp: Stamp,
    /// For [`Tier::Committed`], the exact commit the content was read from
    /// — a reproducible fact. For [`Tier::Worktree`], the HEAD commit
    /// observed at stamping time: a provenance marker only. It does **not**
    /// assert that the stamped content matches that commit's committed
    /// blob — the worktree may be dirty relative to it.
    at: gix::ObjectId,
    tier: Tier,
}

impl Witness {
    /// Construct a [`Tier::Worktree`] witness directly. Prefer
    /// [`crate::Auditor::stamp`], which computes `stamp` and `at` for you;
    /// this constructor exists for callers rehydrating a previously stored
    /// *worktree-tier* witness (e.g. from a database).
    ///
    /// There is deliberately no way to construct a [`Tier::Committed`]
    /// witness through this function — see the type-level docs.
    pub fn new(anchor: Anchor, stamp: Stamp, at: gix::ObjectId) -> Self {
        Self {
            anchor,
            stamp,
            at,
            tier: Tier::Worktree,
        }
    }

    /// Construct a [`Tier::Committed`] witness. Crate-visible only:
    /// callers outside `codewitness` cannot mint committed-tier evidence
    /// directly — only [`crate::Auditor::stamp_at`] /
    /// [`crate::Auditor::stamp_normalized_at`], which read the claimed
    /// commit's tree themselves before calling this.
    pub(crate) fn new_committed(anchor: Anchor, stamp: Stamp, at: gix::ObjectId) -> Self {
        Self {
            anchor,
            stamp,
            at,
            tier: Tier::Committed,
        }
    }

    /// The anchor this witness makes a claim about.
    pub fn anchor(&self) -> &Anchor {
        &self.anchor
    }

    /// The content stamp captured at [`Self::at`].
    pub fn stamp(&self) -> &Stamp {
        &self.stamp
    }

    /// The commit this witness's claim is pinned to. For
    /// [`Tier::Committed`] this is the commit the content was read from;
    /// for [`Tier::Worktree`] it is only a provenance marker (the HEAD
    /// commit observed at stamping time) — see [`Self::tier`].
    pub fn at(&self) -> gix::ObjectId {
        self.at
    }

    /// The evidentiary strength of this witness.
    pub fn tier(&self) -> Tier {
        self.tier
    }
}
