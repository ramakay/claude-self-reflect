use crate::Anchor;

/// The outcome of auditing a [`crate::Witness`] against current evidence.
///
/// There is no fifth "unknown" state: every audit resolves to exactly one
/// of these four, deterministically, from content and git ancestry alone.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Verdict {
    /// Current content's stamp matches the witness's stamp — including the
    /// resurrection case where intermediate commits changed the anchor and
    /// it was later reverted back (A -> B -> A). Content identity wins over
    /// history; `codewitness` never penalizes a value for having a past.
    Intact,

    /// Current content's stamp differs from the witness's stamp, and no
    /// successor witness proved this is a supersession. This is also the
    /// verdict for "near-identical" edits (whitespace tweak, rename of a
    /// local, comment change) — anything short of an explicit, *verified*
    /// successor stays `Drifted`, never escalates to `Superseded` on its
    /// own.
    Drifted,

    /// A specific, later, **committed** witness is known to have replaced
    /// this one. Only [`crate::Auditor::audit_against_successor`] can
    /// produce this variant — see [`SupersededReceipt`] for why its
    /// payload can't be hand-constructed.
    Superseded(SupersededReceipt),

    /// The anchor no longer resolves — the path does not exist (in the
    /// worktree, or at the commit being checked). `last_seen` is the `at`
    /// of the witness that last confirmed its presence.
    Vanished { last_seen: gix::ObjectId },
}

impl Verdict {
    pub fn is_intact(&self) -> bool {
        matches!(self, Verdict::Intact)
    }

    pub fn is_drifted(&self) -> bool {
        matches!(self, Verdict::Drifted)
    }

    pub fn is_superseded(&self) -> bool {
        matches!(self, Verdict::Superseded(_))
    }

    pub fn is_vanished(&self) -> bool {
        matches!(self, Verdict::Vanished { .. })
    }
}

/// The evidence backing a [`Verdict::Superseded`] verdict: which anchor
/// replaced the witnessed one, and the commit (`receipt`) at which that
/// successor witness was taken — always [`crate::Tier::Committed`]
/// evidence.
///
/// The enum variant `Verdict::Superseded` stays public so callers can
/// match on it freely, but this payload's fields are private and there is
/// no public constructor: minting one requires passing
/// [`crate::Auditor::audit_against_successor`]'s full successor-validation
/// gate (tier, commit existence, re-derived stamp, causal precedence —
/// see that function's docs). A "doc'd invariant + private-field newtype"
/// was chosen over sealing the whole `Verdict` enum (e.g. behind a trait or
/// a private-only variant) because callers legitimately need to `match` on
/// `Verdict` — including `Superseded` — in ordinary control flow; only
/// *construction* of the receipt needed to be restricted, not
/// *inspection*, and this is the narrowest change that achieves that.
///
/// **Invariant**: with the `serde` feature enabled, a receipt (like a
/// [`crate::Witness`]) deserialized from storage is an *unverified claim*
/// — `Deserialize` reconstructs the private fields verbatim, so it
/// carries no more trust than the storage it came from. Fresh verdicts
/// are trustworthy because [`crate::Auditor::audit_against_successor`]
/// re-derives the successor's stamp from the object database before
/// minting one; a forged deserialized witness cannot mint a `Superseded`
/// verdict (the serde-gated forgery tests in `tests/adversarial.rs`
/// assert this). Treat a *stored* receipt as a hint to re-audit, not as
/// proof.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SupersededReceipt {
    by: Anchor,
    receipt: gix::ObjectId,
}

impl SupersededReceipt {
    pub(crate) fn new(by: Anchor, receipt: gix::ObjectId) -> Self {
        Self { by, receipt }
    }

    /// The anchor that replaced the witnessed one.
    pub fn by(&self) -> &Anchor {
        &self.by
    }

    /// The commit at which the successor witness was taken.
    pub fn receipt(&self) -> gix::ObjectId {
        self.receipt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object_id() -> gix::ObjectId {
        gix::ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567").unwrap()
    }

    #[test]
    fn verdict_predicates_identify_only_their_matching_variant() {
        let verdicts = [
            Verdict::Intact,
            Verdict::Drifted,
            Verdict::Superseded(SupersededReceipt::new(
                Anchor::new("replacement.rs"),
                object_id(),
            )),
            Verdict::Vanished {
                last_seen: object_id(),
            },
        ];

        assert_eq!(
            verdicts.each_ref().map(|verdict| verdict.is_intact()),
            [true, false, false, false]
        );
        assert_eq!(
            verdicts.each_ref().map(|verdict| verdict.is_drifted()),
            [false, true, false, false]
        );
        assert_eq!(
            verdicts.each_ref().map(|verdict| verdict.is_superseded()),
            [false, false, true, false]
        );
        assert_eq!(
            verdicts.each_ref().map(|verdict| verdict.is_vanished()),
            [false, false, false, true]
        );
    }
}
