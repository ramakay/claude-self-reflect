# Trained re-ranker — current state (2026-08-25)

Branch: `feat/memory-registry-spine`. All reranker work in one `feat(rerank)` commit; the
journal jinja edits (p6-theme) and repo-root CLAUDE.md edit are deliberately excluded and
remain uncommitted.

## What this is

The grader half of dreaming: "sleep produces priors for tomorrow, then tomorrow grades them."
CSR records which memories it showed (exposure log), transcribes how the user actually reacted
(no-LLM labeler), trains a bounded residual re-ranker on those labels (pointwise logistic SGD),
and only deploys a model that survives a chronological gate plus a curated veto. Deterministic
everywhere; no LLM in any labeling/grading/gating seat.

## Architecture (as committed)

- **Exposure**: `hooks/exposure.rs` — schema-v2 impressions/items per injection surface;
  best-effort, zero busy-timeout, never blocks hooks.
- **Labeler**: `hooks/reaction.rs` + harvester in `daemon/trained_rerank.rs` — exemplar-over-
  embeddings classifier (Acceptance/Correction/Reask/Redirect, abstain persisted). Versioned
  reaction-turn filters (image payloads, skill preambles, slash commands, interrupts,
  `[queued]` = causality exclusion) baked into `classifier_hash`; pickup rule requires a
  question-like prior turn; 4h reaction bound on the full shown_at→next_user_ts edge; 1/n
  weight across impressions sharing one reaction turn.
- **Model**: `search/trained_rerank.rs` — residual `clamp(2p-1,-1,1)*0.25` on top of the
  deterministic baseline; poison/scaffold/mechanic candidates never gain positive residual on
  either surface; nuisance features fitted then neutralized at deploy; fail-closed load
  (gate passed + schema v2 + classifier-hash match + compile-time epsilon re-check).
- **Gate**: chronological 80/20, NDCG@5 strict mean win AND cluster wins>losses with floors
  (≥5 candidates/cluster, ≥10 valid clusters spanning ≥5 distinct sessions, ≥2 sessions per
  cluster) AND corpus-local curated veto (cases from the user's own code_evolution, ≥5 cases,
  else `insufficient_data`). Receipts persisted per cluster; attempt+cadence written
  atomically; `model_age_days` in status. Opt-in `CSR_TRAINED_RERANK=1`.
- Feature schema v2: reaction-prior features removed entirely (popularity-replay fix);
  reactions are supervision only.

## Review arc (all findings line-verified before acceptance)

1. Codex xhigh designed + implemented v1 (`.plans/trained-reranker-design.md` (local planning notes, untracked)).
2. Ox Alpha (stealth 1M-ctx model, OpenRouter free preview) whole-context review #1, ~207k
   tokens one prompt: 14 findings, 2 blockers (curated veto dev-machine-only; gate winnable by
   popularity replay), 4 majors. All top-5 confirmed in code.
3. Codex fix round → Ox review #2: 1 CLOSED, 5 NARROWED, 14 new findings; verified subset
   (R2-1..6) fixed in round 3 (epsilon-from-constant, full-edge 4h bound, classifier-hash
   binding, session floors, atomic cadence write; 3 research-grade items documented in design
   doc §13 "Known limitations (v1)").
4. Round 4 (labeler hygiene) after first-contact evidence — see below.

Fix briefs: `.plans/oxalpha-reranker-fix-brief.md` (local planning notes, untracked).

## End-to-end proof (copy DB, 3.1GB, never the live DB)

- Full daemon cycle ran → **first real gate verdict**: `insufficient_data`, "need at least 250
  labeled impressions; found 0" — correct (no schema-v2 exposures exist yet).
- Fail-closed proven live: `CSR_TRAINED_RERANK=1` with no passed model → hook output
  byte-identical to flag-off.
- First harvest exposed a mute labeler (6,192/6,767 abstain, 0 acceptance): abstains were
  harness noise + "continue" at 0.7001 vs 0.72 floor. After round 4:
  **95 acceptance / 20 correction / 4 reask / 5,759 abstain** from 25 distinct human reaction
  turns (corrections concentrated on one turn; 1/n weighting covers it). Verified by direct
  DB query, classifier hash `35e794b3…`.

## Verification (Claude-run, matching Codex's reports)

`cargo fmt --check` clean; `clippy -D warnings` clean; lib **1,855 passed / 4 failed
(pre-existing journal-template failures from uncommitted p6-theme jinja edits, isolated by
stash test) / 2 ignored**; hooks_integration **47/47**; integration **63/63**.

## What is NOT yet proven

- No `passed` verdict exists and none can until real usage accrues ≥250 labeled schema-v2
  impressions (weeks). Any faster path would be fabricated exposures.
- Label signal is real but thin (119 non-abstain labels / 25 turns on the historical corpus).
- Known limitations (design doc §13): nuisance-feature projection skew, curated MRR
  saturation, self-attested receipt store.

## Next

1. Install the binary → schema-v2 exposures start accruing from live hooks.
2. First genuine train attempt fires automatically at 250+ labeled impressions (nightly).
3. Revisit exemplar coverage once audit shows more near-misses from live data.
