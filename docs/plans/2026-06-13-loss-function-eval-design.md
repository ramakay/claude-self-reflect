# Loss-Function Eval for csr-engine — Design Proposal

*Status: proposal (no code yet) · Branch: `claude/agent-evals-moats-jlwkmx` · 2026-06-13*

## TL;DR

`csr-engine eval` today is a **spec-driven test suite**: finite, binary, green-means-done,
and trivially gameable. This proposal turns it into a **loss-function-driven benchmark**:
a graded, blinded, regression-tracked score over a distribution of real
`(query → relevant memory)` examples that the engine descends toward and can never fully
"finish."

The unlock: **the ground-truth distribution already exists in the database.** The
`retrieval_events` and `retrieval_stats` tables already log which memories were surfaced
and whether the session succeeded. We are sitting on the private, hard-to-reproduce eval
set the moat argument is about — we just never score against it.

> Prompts create motion. Loss functions create direction. Private evals create moats.
> CSR already collects the moat; this makes it measurable.

---

## 1. Why the current eval is spec-driven (and gameable)

Grounded in `csr-engine/src/eval/mod.rs`:

| Test | Problem | Article's term |
|------|---------|----------------|
| `test_tool_count` (L346) | asserts `12 == 12`, a constant. Can never fail, measures nothing. | metric with no instrument |
| `test_semantic_search` (L359) | returns `pass` on **empty** results *and* on non-empty results — never checks relevance against ground truth, only "did anything return." | shallow metric |
| `test_search_accuracy` (L215) | only real threshold is `top_score > 0.1` — a near-floor cosine bar; `pass` on empty data (L223). | un-fenced shortcut |
| empty-DB paths | several tests `pass` with zero data, so an empty database scores near-perfect. | leakable / no constraints |
| whole suite | 20 tests, binary, green = done. No notion of "better" vs "worse." | finite test suite, not a loss |

**Net:** a 20/20 run proves the binary boots and the index is internally consistent
(infrastructure health). It cannot tell you whether a code change made **retrieval
better or worse** — which is the one thing CSR's product thesis ("you can't optimize
what you can't see") demands it be able to see.

This is not a criticism of the tests as *smoke tests* — they're fine for that. We keep
them, recategorized as `infra`, and add a real loss on top.

---

## 2. The four-part loss function, mapped to CSR

The article decomposes a good `/goal` loss into **target, constraints, instruments,
forced entropy**. Here is each, instantiated for csr-engine.

### 2.1 Target — graded retrieval quality over a blinded distribution

Replace "did something return" with standard ranked-retrieval metrics over a labeled set
of `(query, relevant_chunk_ids)` cases:

- **Recall@k** — of the known-relevant memories, how many appear in top-k.
- **MRR** — reciprocal rank of the first relevant hit (rewards putting the right answer first).
- **nDCG@k** — graded relevance (a "success"-labeled memory worth more than a "neutral" one), discounted by rank.

These produce a continuous score in `[0,1]` that **can regress**. That single property —
the number can go *down* when a change hurts — is the entire difference between a test
suite and a loss function.

**Blinding.** The cheat the engine *can't* commit (it's pure vector similarity, no access
to labels at query time) is different from the cheat the **optimizing agent/developer**
commits: overfitting the engine to a fixture set it can see. So blinding here = a
**held-out split**:

- `dev` set — visible, used during iteration.
- `test` set — revealed only at scoring, reported separately, **rotated** periodically.
- The gap between dev and test scores *is* the overfit signal (see 2.4).

**Size.** Article's lesson: a 28-item eval gets memorized in one round; widen until
enumeration doesn't pay. Target ≥200 cases at launch, growing automatically (see 3.2).

### 2.2 Constraints — what "good" is *not* allowed to cost

A higher recall score is worthless if it comes from latency or index bloat. The loss must
price these in, not ignore them:

- **Latency budget** — p95 search must stay under target (we already measure it:
  `test_search_latency_p95`, L692, target <10ms). A recall gain that blows the p95 budget
  is penalized, not rewarded.
- **Index/memory budget** — chunk count vs resident size; flag pathological growth.
- **Score calibration** — thresholds (`min_score`) must not be tuned per-fixture; the same
  config scores dev and test.

### 2.3 Instruments — a CLI for every constraint ("a constraint without an instrument is a vibe")

Every constraint above ships with a command the loop can call to inspect itself:

```
csr-engine eval --loss              # composite loss + per-metric breakdown (dev + test)
csr-engine eval --loss --json       # machine-readable, for the outer loop
csr-engine eval history             # score over the last N runs (regression view)
csr-engine eval overfit             # dev↔test gap, per-case miss list
csr-engine eval budget              # p95 latency, index size vs caps
```

The point is that an agent (or `/goal` loop) optimizing csr-engine can *see its own
gradient* without us hand-feeding it.

### 2.4 Forced entropy — anti-overfit and regression memory

The article's sharpest, least-hyped idea: a loop walks up whatever hill it was already on.
Two instruments fight this:

- **Overfit check** — `dev_score − test_score`. If the gap widens across runs, the change
  is memorizing the visible set. Surfaced every run, not just on request.
- **Run-history regression table** — persist each run's composite + sub-scores
  (new `eval_runs` table). A change that lifts dev but regresses test, or improves recall
  while regressing p95, is flagged red. This is the "iteration log" the article prescribes,
  and CSR already has the perfect home for it (`get_session_learnings` / Ralph-loop memory).

---

## 3. Where ground truth comes from — the moat is already in the DB

Three sources, in priority order.

### 3.1 Mined from real usage (the moat) — `retrieval_events` + `retrieval_stats`

From `migrations.rs`:

```sql
retrieval_events(memory_id, memory_type, retrieved_at, hook_phase, session_outcome, session_id)
retrieval_stats(memory_id, success_count, failure_count, neutral_count, ...)
```

This is a **labeled relevance signal collected from real developer sessions** —
exactly the "edge cases your users actually trip on / ground truth you measure privately"
the article calls the durable moat. A memory with high `success_count` that was surfaced
for a given session's prompt is a positive `(query, relevant_memory)` pair. Competitors
cloning the open-source engine cannot reproduce this; it's our private distribution.

**Caveat (honest):** this signal is currently *implicit* — `session_outcome` is a
session-level label, not a per-(query,memory) judgment, and "outcome" is noisy. Phase 1
mines it conservatively (high-confidence successes only) and treats it as weak labels to
be confirmed, not gospel. Building the *labeling pipeline* that turns this telemetry into
clean pairs is itself the moat-building work.

### 3.2 Hand-labeled seed set

A curated `fixtures/eval/relevance.jsonl` of ~50 high-quality cases authored against the
real corpus (`chunks` table) to bootstrap before enough telemetry accrues. Format:

```json
{"id": "rel-001", "query": "rmcp tool params Parameters pattern",
 "relevant_chunk_ids": ["<id1>", "<id2>"], "split": "dev",
 "source": "manual", "graded": {"<id1>": 3, "<id2>": 1}}
```

`graded` = relevance grade (3=ideal, 1=related) for nDCG.

### 3.3 Synthetic-but-verified

Generate candidate queries from known chunks (e.g. take a chunk's AST-extracted function
name, build a query, expect that chunk back), then **verify** the pairing holds before
admitting it. Cheap volume to push the set past the enumeration threshold, but always
verified so it doesn't poison the target.

---

## 4. The composite loss (one descendable number)

```
loss = w_r·(1 − Recall@10)
     + w_m·(1 − MRR)
     + w_n·(1 − nDCG@10)
     + w_l·latency_penalty(p95)        # 0 under budget, ramps above
     + w_o·overfit_penalty(dev − test) # 0 when dev≈test, ramps as gap grows
```

Lower is better; the outer loop descends it. Weights live in config so they're auditable
(not buried in code). The headline number reported is `1 − loss` as a 0–100 "quality
score" for human readability, but the **gradient is the loss**.

---

## 5. Fencing the cheats — CSR-specific shortcuts to block up front

The article cheated 3 times because it left shortcuts open. Ours, pre-fenced:

1. **Memorizing fixtures** → held-out `test` split, rotated; overfit penalty in the loss.
2. **Per-fixture threshold tuning** → one config scores both splits; config is part of the run record.
3. **Inflating recall by returning everything** (his own 50× trap) → report **precision/MRR
   alongside recall**; a result set that dilutes rank is penalized by nDCG and MRR even as
   recall rises.
4. **Empty-DB free pass** → loss requires a minimum corpus; `SKIP` cannot score as `pass`.
5. **Constant-true tests** → `test_tool_count`-style asserts move to `infra`, excluded from the loss number.

---

## 6. Implementation plan (phased — none of this lands until you approve scope)

**Phase 0 — Graded harness scaffolding (no telemetry yet).**
New `eval/loss.rs`: metric implementations (recall@k, MRR, nDCG@k), `relevance.jsonl`
loader, `EvalCase`/`LossReport` types. Wire `csr-engine eval --loss`. Recategorize the
existing 20 tests as `infra`. Ship ~50 hand-labeled cases (3.2). *Self-contained; fully
testable with `cargo test`.*

**Phase 1 — Mine ground truth from telemetry (3.1).**
`storage` query that joins `retrieval_events` + `retrieval_stats` into weak labels;
conservative confidence filter; fold into the eval corpus. This is the moat-builder.

**Phase 2 — Blinding + regression memory (2.4).**
`eval_runs` table; `dev`/`test` split + rotation; `eval history` and `eval overfit`
commands; overfit penalty enters the loss.

**Phase 3 — Constraints + meta-instruments (2.2/2.3).**
Latency & index budgets in the loss; `eval budget`; `--json` for outer-loop consumption.

Each phase is independently shippable and leaves `cargo test` green.

---

## 7. Files touched (anticipated)

| File | Change |
|------|--------|
| `csr-engine/src/eval/mod.rs` | recategorize existing tests → `infra`; route `--loss` |
| `csr-engine/src/eval/loss.rs` *(new)* | metrics, loss composite, report types |
| `csr-engine/src/eval/corpus.rs` *(new)* | fixtures loader + telemetry miner |
| `csr-engine/src/storage/queries.rs` | weak-label join over `retrieval_events`/`retrieval_stats` |
| `csr-engine/src/storage/migrations.rs` | `eval_runs` history table (Phase 2) |
| `csr-engine/src/main.rs` | `eval --loss`, `eval history|overfit|budget` subcommands |
| `csr-engine/fixtures/eval/relevance.jsonl` *(new)* | seed labeled set |
| `csr-engine/tests/eval_loss.rs` *(new)* | metric unit tests (recall/MRR/nDCG correctness) |

---

## 8. Decisions for you

1. **Primary ground-truth source.** Lean on mined telemetry (3.1, max moat, noisier) vs.
   hand-labeled seed (3.2, cleaner, slower to scale) as the *launch* default? Recommend:
   ship Phase 0 on hand-labeled, make telemetry-mining (Phase 1) the headline follow-up.
2. **`--loss` separate from `--full`, or fold in?** Recommend separate command so the fast
   infra smoke test stays fast; loss is the deliberate, heavier run.
3. **Scope of this branch.** Phase 0 only (proves the shape), or Phase 0+1 (proves the moat
   thesis end-to-end by scoring against real telemetry)? Recommend Phase 0+1.

---

*Next step: on approval, implement Phase 0 behind `csr-engine eval --loss`, with metric
unit tests, leaving the existing eval untouched as `infra`.*
