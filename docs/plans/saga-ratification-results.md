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
| 2 | v2 (rebalanced prompt; digest silently still head/tail — see correction) | **0.071** | 123 | scores have variance (grade-group means 0.31–0.47), acts genuinely extracted — still no separation. Grade-3 sealed origins mean 0.362 vs grade-0 incidental 0.422 |
| 3 | v3 (operator-turn digest genuinely active, post PR #246 fix) | **0.036** | 111 | flattest of all three. Grade-3 origins mean 0.447 vs grade-0 incidental 0.476 |

**Correction (2026-07-20, post-merge review):** run 2 was described at the time as
using the "operator-turn-prioritized digest." It did not. `get_chunks_by_ids`
reconstructs chunks with a hardcoded `Speaker::ToolResult` author (author lives in
`chunk_provenance`, not the `chunks` table), so `build_digest`'s operator-turn filter
matched nothing and every v2 extraction silently fell back to head/tail sampling —
v2's only real change was the rebalanced prompt. Found via CodeRabbit review of PR
#245 (surfaced by the operator), fixed in PR #246 (`get_chunks_by_ids_with_provenance`
join + regression test through the real storage path), and the gate was re-run as v3
with the digest genuinely active. ρ dropped to 0.036 — the negative result is
strengthened, not rescued, by the fix.

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
binding and died. **Surviving open question: edge-level ratification —
acts bound to (conversation, artifact) pairs weighting reinstatement graph edges,
joined query-conditionally.** That is a redesign, not a tweak, and was not run.

### Edge-level cheap pass — ruled infeasible by data audit (2026-07-21)

Post-halt, a cross-vendor advisor consult (Grok 4.5) recommended one pre-registered
Gate B on edge-conditional scores IF computable from data already on disk at zero
model spend, with the predicate frozen before any correlation was computed. A Codex
audit of the live DB then killed the cheap pass on coverage before any ρ was run:

- Strict act↔artifact binding: only 5/124 sealed-gold conversation/query pairs have
  an act evidence string containing the target path, and **none** of those also has
  the corresponding ledger row — strict edge coverage is zero pairs.
- `ledger_refs` persists only commit SHAs; the file identity used to select those
  SHAs was discarded at write time. `code_evolution` covers 75/3,957 ratified
  conversations (74/124 sealed gold; 56/124 overlap any E2 target file — 42.7%).
- The only computable join binds conversation→file, not act→file. Assigning every
  act to every touched file recreates the node-level construct mismatch at edge
  granularity — a proxy, not the hypothesis.
- Git history cannot backfill: commits carry no conversation ID; only probabilistic
  timestamp matching is possible.

Consequence: the sealed one-shot was **not** burned on an invalid predicate.
Edge-level ratification remains untested and is now known to require re-extraction
that persists act↔artifact bindings at write time (extractor generation 4) plus
content-based corroboration — real spend on a construct family 0-for-3, declined
under budget. Design lesson for any future attempt: the binding the mechanism needs
must be persisted at extraction time; it cannot be reconstructed afterwards.

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
decision origins in a high-ship-rate solo corpus (ρ=0.060→0.071→0.036 across three
extractor versions, n=111–123, pre-registered gate).* The pre-registered halt is
itself the demonstration that the E2/E1/E3 protocol discipline transfers to new
hypotheses — including surviving a post-hoc mechanism correction: when external
review exposed that v2's digest never actually prioritized operator turns, fixing
the mechanism and re-running the gate made the correlation flatter, not better.
