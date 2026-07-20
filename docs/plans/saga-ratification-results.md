# Ratification-Weighted Memory — Program Results (NEGATIVE, run 2026-07-18..20)

Executed the manifesto's 30-day plan (saga-manifesto-ratification-memory.md) in
backfill-first form. **The program halted at its pre-registered gate: per-conversation
ratification scores do not correlate with sealed E2 relevance grades.** Node-level
"memory strength follows acts" is dead on this corpus. This document is the honest
record; the pre-registered halt is the result.

## What was built (all committed, saga-phase1)

| Commit | Piece |
|---|---|
| fca4133 | eval-kit released (E2/E1/E3 harness, sanitized) |
| 623218f | ratification_scores schema, daemon backfill loop, status surface, Gate A advisor tweaks (generic-basename denylist, prompt hardening) |
| ff0e51b | SessionEnd forward-fill enqueue, brace-fallback JSON parsing |
| 1aaff0c | prompt hardening against instruction-echo digests |
| ccdd2e3 | `csr-engine ratify <conv-ids>` one-off scoring subcommand |
| 05a1823 | v2 extractor: operator-turn-prioritized digest |
| 4ffb305 | shadow signal on reinstatement evidence (logged, never ranked) |

Scoring v1: acts extracted per conversation (DIRECTS/ACCEPTS/REJECTS via haiku),
`score = (directs+accepts)/(directs+accepts+rejects+2)`, git-ledger corroboration
gate (uncorroborated capped 0.6).

## Gate A′ — both runs failed

Pre-registered: Spearman between backfill scores and sealed E2 grades
(conversation-level = max grade across queries); halt if ρ≈0.

| Run | Extractor | ρ | n | Diagnosis |
|---|---|---|---|---|
| 1 | v1 | **0.060** | 121 | measurement artifact: 88% of convs extracted zero acts (digest sampled mostly assistant/tool text; echo-hardened prompt bailed to empty on instruction-like digests — which all agent digests are) |
| 2 | v2 (operator-turn digest, rebalanced prompt) | **0.071** | 123 | clean: scores have variance (grade-group means 0.31–0.47), acts genuinely extracted — still no separation. Grade-3 sealed origins mean 0.362 vs grade-0 incidental 0.422 |

## Mechanism (why the thesis fails at node level)

E2 grades are **query-conditional**: "load-bearing for THIS decision." The
ratification score is **global**: "this conversation's work (any work) was
directed/accepted." In a solo high-ship-rate corpus nearly every working session
directs, accepts, and ships something — global ratification is near-uniformly high
across real work sessions and cannot separate "origin of this decision" from
"productive session about something else." ρ≈0 is the honest measurement of that
construct mismatch, not noise.

Corollary: E2's gold worked precisely because it bound acts to a query's artifact
(acts × ledger × target). The retention-weight version of the thesis dropped the
binding and died. **Surviving open question (untested): edge-level ratification —
acts bound to (conversation, artifact) pairs weighting reinstatement graph edges,
joined query-conditionally.** That is a redesign, not a tweak, and was not run.

## Also found during the burn

- **Extractor echo-contamination**: CSR's own injected scaffold (session reminders,
  hook output) inside digests made the model treat transcripts as instructions —
  the E3 contamination class appearing in a new pipeline. Fixed by data-framing +
  operator-turn digest (1aaff0c, 05a1823).
- **Queue starvation**: newest-first enrichment ordering starved the oldest (gold)
  conversations while backlog import kept refilling the head — motivated the
  `ratify` subcommand.
- **Corroboration starvation**: `files_for_session` (code_evolution) covers only
  ~106 recent sessions → 533/534 early scores sat on the uncorroborated cap.
  Uniform cap = monotone, so Gate A′ unaffected; content-based corroboration would
  be the v3 fix if edge-level work ever proceeds.

## Cost (all recorded in narrative_usage, call_site="ratification")

4,112 haiku calls, ~2.29M output tokens, 3,956 conversations scored (v1: 3,833;
v2 rescore of E2 set: 123). Kill switches: CSR_NO_RATIFICATION / CSR_NO_AI_NARRATIVES.

## State after halt

- Ratification loop **frozen** (daemon runs with CSR_NO_RATIFICATION=1). Resuming
  live scoring is an explicit decision, not a default — the signal it accumulates
  is the one this program measured as flat.
- Shadow signal stays (harmless: logged, never ranked; versioned rows).
- 13 permanently-failing conversations marked unavailable (no retry loop).
- Stage C divergence report and Stage D benchmark **not run** — skipped per halt;
  running a benchmark on a signal proven flat would credit policy, not memory
  (the advisor's deciding risk, honored).

## Verdict for the paper

Negative result with mechanism: *global ratification-probability does not rank
decision origins in a high-ship-rate solo corpus (ρ=0.06→0.07 across two extractor
versions, n=123, pre-registered gate).* The pre-registered halt is itself the
demonstration that the E2/E1/E3 protocol discipline transfers to new hypotheses.
