# Manifesto: Ratification-Weighted Memory

*Goal document for the next agent. 2026-07-18. Successor to the Phase-2 program
(saga-phase2-experiments.md, E1/E2/E3 results, saga-paper.typ).*

## The law

**Memory strength should follow acts, not similarity.**

Everything this project has proven is an instance of that one sentence. Reinstatement
recall wins because graph edges record *acts* (this session touched that file) where
cosine records resemblance. Echo contamination happens because re-asking a question is
similarity masquerading as importance — an *anti-act*. The E2 gold worked because the
operator's dialog-acts and ship events graded conversations better than any annotator
could. We have been circling one idea from three sides. The next agent walks into the
center.

## What dies

The current paper is a better ranker measured by rank metrics. That genre is crowded and
its ceiling is "good engineering paper, single operator" — we have the review saying
exactly that. Rank metrics on retrieval compositions cannot escape the genre. Stop
optimizing them. Stop reporting them as headline claims.

## What is born

**The corpus stops labeling itself for evaluation and starts labeling itself as the
memory system.** Ratification signals — DIRECTS, ACCEPTS, REJECTS, commits, pushes,
merges, publishes, reverts — computed online, per session, become the retention and
retrieval function:

> memory_strength(conversation) = running estimate of P(ratified into the artifact)

Anderson & Schooler showed human memory tracks need-probability. Joachims learned
relevance from clicks; RLHF learns preference from human acts; Mem0/A-Mem/Generative
Agents weight consolidation by importance scores. Those are the ancestors — name them,
do not pretend otherwise (cross-vendor advisor verdict, 2026-07-18: the "no ancestor"
framing oversells and dies in review). The defensible claim is narrower and still
unpublished: **ledger-corroborated ratification** — memory strength grounded in
external ship events (commits, publishes, reverts), not self-assessed importance —
**paired with theory-restoration as the measured behavior**. Importance scores
hallucinate; ledgers do not. That pairing is the contribution. Headline it, never the
broad "P(ratified) as supervision signal" claim alone.

## Three mechanisms, three dead review objections

1. **Learned fusion.** Channel weights (blend / graph / episode) trained per-corpus by
   the online ratification signal. E1's embarrassment — graph-only beating the full
   walk — becomes the training objective instead of a limitation.
2. **Ratification-driven consolidation.** Origins that ledger-corroborate as
   load-bearing get reinforced: summarized into durable decision records, re-embedded,
   protected from their own future re-askings. Hippocampal replay where the replay
   schedule comes from git. The 5/12 unreachable origins stop being a disclosed floor
   and become the demonstration.
3. **Behavioral endpoint.** The claim class changes from "+37% origin-MRR" to
   **"agents with ratification-weighted memory stop re-litigating settled decisions."**
   Seeded maintenance tasks where the correct action depends on a past decision's
   reason: does the agent re-introduce the rejected alternative? Re-pin what was
   unpinned for cause? Nobody has measured Naur's theory-*restoration* as agent
   behavior. First paper to do it defines the benchmark; behavioral tasks are
   corpus-agnostic, so the solo-operator objection dissolves.

   **Well-posedness (advisor conditions, binding).** A blanket-refusal agent must not
   pass. Task mix is four-way, pre-registered per task before any run: settled-for-cause
   (correct = decline, citing the reason), should-change (correct = accept — kills pure
   conservatism), irrelevant-past (correct = ignore — kills over-retrieval), ambiguous
   (correct = check ledger / ask). Minimum ≥3 should-change and ≥3 settled-for-cause in
   any 10-task set. Score is reason-faithful action, not refuse-rate; grading uses a
   human reason-quality rubric for v1, never string-match alone. The two arms must be
   identical in prompt, tools, and harness except the memory index/weights — otherwise
   the experiment is a prompt A/B and the behavioral claim is void.

## The figure 1 that does not exist yet

An agent, asked to "upgrade rmcp," declines — citing the July 2026 triage session where
the pin was a decision, not an accident — while the memoryless control re-breaks the
build. One seeded task, two agents, one column of green and one of red. That figure has
never been published. It is worth more than every table we have produced.

## First 30 days (concrete, in priority order)

The data already exists — no accumulation wait. Ship ledgers (git/npm/tags) never
expire; dialog acts survive in CSR DB chunks even where 30-day JSONL cleanup purged
transcripts (E2's digest fallback proved it); the extraction pipeline is built and
validated on 204 items.

1. **Backfill first**: batch E2 extraction over the full corpus → `ratification_score`
   per conversation for the entire history, this week. Cost accounted in
   `narrative_usage`, kill-switchable, per house rules.
2. Wire the same extraction into Stop/SessionEnd hooks (forward-fill). The following
   month of live scores becomes *validation* data — does live match backfill? — not a
   prerequisite.
3. Add `ratification_score` to the fusion rerank as a shadow signal (logged, not yet
   ranked-on) — collect divergence data between similarity order and act order.
4. Author 10 seeded re-litigation tasks from this repo's own settled decisions (the E2
   sealed gold is the task source: rmcp pin, mutex choice, echo defense, chunking),
   classed and balanced per the well-posedness conditions above. Freeze a DB snapshot
   before any task dialogue — E3 is the reason.
5. Run the first two-agent comparison (arms identical except memory). The 3-of-10 bar
   is a *liveness* signal only — the paper claim waits for the balanced-mix score with
   human-rubric grading.
6. Only then: learned fusion weights and consolidation, in that order.

## Guardrails (inherited, non-negotiable)

- Frozen snapshots before any evaluation dialogue; shared index builds; echo defense on
  every retrieval surface. E3 is the reason.
- Sealed pre-registration before rank exposure. E2's protocol is the template.
- Ledgers corroborate, never mint. Extraction never assigns the top grade.
- Report the floor. The 5/12 finding earned more reviewer respect than the +37%.
- The corpus is private by construction: behavioral benchmarks and metrics-only
  telemetry are the external-validity paths; never ask operators to publish history.

## Success criterion

The next review does not say "unusually self-aware engineering paper." It says: this
measures something no one has measured, with a supervision signal no one has used, and
the benchmark is now the field's problem.

## Review record

Cross-vendor advisor consult (Grok 4.5, 2026-07-18): verdict **conditional** — proceed
through the two-agent comparison only under the arm-identity, task-mix, and
human-rubric conditions now folded into this document. Named adjacency threats
(Joachims, Mem0/A-Mem, Generative Agents, RLHF) absorbed into "What is born." Reject
trigger on record: pitching "first unsupervised memory theory" without citing the
adjacent work. Deciding risk: confounded or gameable tasks that credit policy, not
memory.

## Outcome (2026-07-20): pre-registered gate FAILED — node-level thesis dead

The program ran backfill-first and halted at Gate A′: per-conversation ratification
scores vs sealed E2 grades, Spearman **0.060 (v1) → 0.071 (v2, clean extraction,
n=123)**. Mechanism: the score is global, E2 relevance is query-conditional; in a
high-ship-rate solo corpus nearly every session is ratified at something, so
node-level act-strength cannot rank decision origins. "Memory strength follows
acts" survives, if at all, only as **edge-level** weighting — acts bound to
(conversation, artifact) pairs, joined query-conditionally — which is untested.
Stage D was not run (benchmark on a flat signal would credit policy, not memory —
the advisor's deciding risk, honored). Full record: saga-ratification-results.md.
This document stays as the goal statement it was; the next agent starts from the
edge-level open question, not from this plan.
