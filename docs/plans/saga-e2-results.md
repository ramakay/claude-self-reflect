# E2 Results — Ratification-Derived Graded Gold (run 2026-07-17)

Protocol: saga-phase2-experiments.md (E2, advisor-locked; internal planning doc, preserved
in repo history). Sealed origins: saga-e2-origins-precommit.md (internal; sealed at commit
4a42644, pre-ranks — the seal is verifiable in repo history). Pipeline artifacts in
session scratchpad `e2/` (pools, ledger, digests, dual extractions, grades.json).

## Setup

- 20 queries (12 CSR + 8 second-corpus), frozen Jul-15 rank lists reused as arm rankings
  (single documented index build; arms NOT re-run — no fresh contamination surface).
- Pool = arm A top-10 ∪ arm B top-10 ∪ date-filtered code-graph file-touch = 211 items;
  205 digested (6 unrecoverable: JSONL purged AND absent from DB, disclosed).
- ~50% of pool conversations had JSONLs purged by Claude Code's 30-day cleanup —
  reconstructed from CSR's own DB chunks (the memory system recovering its own eval inputs).
- Dual independent extraction: grok-4.5 (CLI lane) + Sonnet, identical digests, frozen
  extractive quote-anchored protocol, no retrieval tools. 40/40 files valid JSON.
- Grading: strict two-vendor consensus on dialog-acts, conservative on splits; grade 3 only
  = sealed+mapped origin (extraction never mints 3); 2 = edits target; 1 = discusses;
  0 = re-ask/unrelated.

## Headline metrics (12 mapped-origin queries; 7 UNRESOLVED excluded per protocol; Q2 OOC)

| Metric | kNN (A) | Reinstatement (B) | Δ |
|---|---|---|---|
| origin-MRR | 0.193 | 0.264 | +37% |
| nDCG@10 (graded) | 0.363 | 0.470 | +29% |
| Recall@10 (grade ≥2) | 0.444 | 0.553 | +25% |

B ≥ A on every mapped query for origin-MRR (ties included); B > A on nDCG in 9/12, A > B
in 2 (Q4 marginal, A6 marginal), tie 1. Direction consistent with the +47-53% session-
coverage results, now on graded provenance gold instead of file-touch proxy.

## The sobering finding (paper-grade)

**5 of 12 mapped origins were retrieved by NEITHER arm** (injected into pool per advisor
rule): Q4, Q9, A5, A6, A7. Origin-MRR = 0 for both arms on 7/12 queries. Even the winning
system misses the mapped origin conversation ~58% of the time at k=10. Origin-finding is
NOT solved; reinstatement improves it but the headroom above B is large.

**Owner audit (2026-07-17, Part 1): ALL FIVE injected-origin maps confirmed MAP-CORRECT**
(Q4, Q9, A5, A6 May-14, A7 — including the ambiguity-flagged A7). The mapping-error reading
is eliminated: these are genuine retrieval failures. Pattern: 4 of 5 confirmed-missed
origins date from May 2026 — old, early-build conversations losing to later, denser
sessions — the documented pre-v8/recency gap, now with owner-verified gold behind it.

## Silent acceptance confirmed empirically

Across 204 extracted conversation-items: 21 DIRECTS, **1 explicit ACCEPTS**, 0 REJECTS,
0 RE-ASKS. Operators direct in words but ratify by shipping — the acceptance ladder
(commit/push/tag/publish external ledgers) is not an optional enhancement, it is the only
acceptance signal that exists at scale. (Zero re-asks: pool derives from pre-eval frozen
rankings, largely predating the echo sessions.)

## Extractor agreement (dual-vendor, n=204)

| Act | raw agreement | Cohen κ |
|---|---|---|
| directs | 84.8% | 0.412 |
| accepts | 98.5% | 0.395 |
| rejects | 97.5% | 0.0 |
| reasks | 100% | n/a (no positives) |

Moderate κ on directs (prevalence-deflated; ~31 split items) — these splits route to the
owner audit sample. Known judgment divergence: Sonnet counted code-comment rationale
surfaced via Read dumps as `discusses`; grok stricter on that but more liberal on DIRECTS
(39 vs 21 positives). Quote-fidelity disclosure (grok self-check): ~5% of grok quotes not
byte-exact verbatim — turn-boundary joins across `[ts]` markers and markdown-bold stripping;
content faithful, no fabrication observed; consensus grading (conservative on splits)
absorbs the discrepancy. Item-count delta: grok 205 vs sonnet 204 (sonnet dropped one A1
section); grading treats single-vendor items as single-source, disclosed.

## Strata (disclosures)

- Mapped (origin-MRR eligible): Q4 Q5 Q6 Q8 Q9 Q10 Q12 A3 A5 A6 A7 A8 (12)
- UNRESOLVED (excluded, never soft-matched): Q1 Q3 Q7 Q11 A1 A2 A4 (7)
- OUT-OF-CORPUS: Q2 (owner-claimed pre-v8 concept; July impl = grade-2 candidate)
- Injected origins: 5 (listed above). Pool incompleteness: 6 undigestable items.
- Mapping: metadata-only (literal LIKE + git dates); confidences recorded in mapping.json.

## Pending

- Owner audit Part 1 (injected-origin maps): DONE 2026-07-17 — 5/5 MAP-CORRECT.
  Parts 2-3 (31 vendor-split items, 12 grade spot-checks → human-model κ): owner opted out;
  disclosed as unaudited — consensus grading (conservative on splits) is the mitigation.
- Ledger corroboration pass (72h window + path overlap) wired but thin explicit-ACCEPTS
  means ladder events corroborate grade-2 evidence rather than upgrade grades — per advisor,
  ladder never mints origin.
- E2b cross-persona marketing queries; E1 ablation grid next (metric now believable).
