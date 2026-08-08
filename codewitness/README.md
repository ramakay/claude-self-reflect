# codewitness

Deterministic, evidence-grounded staleness detection for code-anchored
claims. Zero LLM. Zero wall-clock time.

Stamp an `Anchor` (a file, optionally a symbol/span) into a `Witness` —
either from the live worktree (`Auditor::stamp`, `Tier::Worktree`) or from a
specific commit (`Auditor::stamp_at`, `Tier::Committed`). Later,
`Auditor::try_audit` a witness against current evidence and get exactly one
of four `Verdict`s:

- **Intact** — current content still matches, even if history moved away
  and back (`A -> B -> A` reverts resurrect to Intact; content identity
  beats history).
- **Drifted** — content changed; no successor evidence was supplied.
- **Superseded(receipt)** — an explicit, *committed* successor witness
  proved this one was replaced (`Auditor::audit_against_successor`).
  `receipt.receipt()` is always a commit id from committed-tier evidence
  that was independently re-verified (re-derived stamp, causal precedence)
  before minting — a dirty worktree, a forged stamp, or a causally-earlier
  commit can never mint one.
- **Vanished { last_seen }** — the anchor no longer resolves.

`Witness` fields are private and tier is type-enforced: the public
`Witness::new` can only produce `Tier::Worktree` evidence; `Tier::Committed`
witnesses can only come from `Auditor::stamp_at` / `stamp_normalized_at`.
Likewise `Verdict::Superseded`'s payload has no public constructor — only
`Auditor::audit_against_successor`'s full verification gate can mint one.

Causal ordering (`causal::compare`) uses git merge-base ancestry only —
never author/committer timestamps, which are attacker- and
rebase-controlled and therefore not evidence of anything.

`normalized_diff_id` gives squash- and cherry-pick-equivalent patch
identity: the same edit hashes the same regardless of which commit or how
much unrelated history surrounds it.

No tokio, no async, no network. Built on [`gix`](https://docs.rs/gix).

MSRV: 1.85 (edition 2021; floor set by the `gix` 0.86 dependency).
