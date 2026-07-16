# Saga Phase 2 — Experiment Program (paper revision 2)

Driven by external GPT-Pro review (2026-07-15) + grok-advisor design consult. Goal: convert
"impressive dogfooding report" into "defensible research paper" — isolate causality, harden GT,
characterize contamination.

## Ordering (advisor ruling: metric before mechanism before polish)

1. **E2 — Graded provenance gold** (file-touch GT is the paper's soft underbelly)
2. **E1 — Factored ablation grid** (mechanism isolation meaningful only once metric believable)
3. **E3 — Controlled contamination** (already documented qualitatively; controlled dose = polish)

## E2 — Graded provenance gold (FIRST)

- Pool: TREC-style — file-touch history ∪ both arms' top-10s, per query (20 queries). Disclose
  pool incompleteness.
- **Freeze pool before grading.** Present items arm-blind (session text only, no arm labels).
- Rubric (fixed before grading): 3 = originating decision/acceptance conversation; 2 = direct
  implementation or root-cause evidence; 1 = later retrospective discussion; 0 = re-asking,
  eval echo, unrelated touch.
- Single annotator (corpus owner) → publishable ONLY as "owner-graded pilot", not gold.
  Mandatory disclosures: owner lived-history recall bias (annotator remembers decisions beyond
  what's on screen) — THE credibility killer if undisclosed. Delayed self-re-grade of ~20%
  items → report self-κ.
- Metrics: origin-MRR (first grade-3), nDCG@10 (grade weights), Recall@10 at grade ≥2.
  File-touch demoted to candidate-set construction only.

## E1 — Ablation grid (SECOND)

All arms share: provenance reranker, dedup, min_score, k=10 budget, ONE shared HNSW build per
grid run. Candidate generation varies:

| Arm | Generation |
|---|---|
| a | one-shot kNN |
| b | kNN + echo demotion |
| c | dense centroid PRF (Rocchio: one re-query from mean of top-3 seeds, 0.65/0.35) |
| d | blend-only per-seed walk |
| e | graph-only expansion |
| f | full walk |

- Advisor: NO RM3/BM25 lexical arm — claim lives in embedding space; dense Rocchio (c) is the
  right classic-PRF foil. Lexical bake-off = different paper. (Entity-Collision already shows
  RM3 null on intent queries — cite, don't rerun.)
- N=3 index rebuilds ONLY if arm deltas fall inside documented ±1 ANN noise band.
- Key comparisons: (c) vs (d) = per-seed vs collapsed-centroid (the PRF differentiation claim);
  (b) vs (f) = does the walk earn its cost over echo-defended kNN; (d)+(e) vs (f) = channel
  additivity.
- Review-2 advisor check (2026-07-16): arms (d)/(e)/(f) ARE the three-trace channel ablation
  reviewer's major 1 demands — grid answers it; not just defense/reranker variants. Arm (b) is
  verbatim the "kNN + echo demotion + same reranker" baseline review-2 predicts reviewers will
  request.
- Risk flagged: scope creep into multi-modality bake-off that never ships. Grid is 6 arms ×
  20 queries, one evening.

## E3 — Controlled contamination (THIRD)

- C0 = session-zero snapshot: `~/.claude-self-reflect/backups/pre-test-episode-sweep-20260611`
  (pre-dates all eval dialogue). **Drop/subset queries whose target decisions post-date C0.**
- C1 = C0 + only the eval-design transcript.
- Csham = C0 + unrelated transcript matched for length + tool activity (essential control).
- C5 = C0 + five scripted re-asking cycles — frame ONLY as "controlled self-referential
  injection" (dose-response), never as natural-ecology observation. Risk: proving "we wrote
  echo into the corpus" instead of reproducing real dynamics — mitigate by comparing C1
  (natural) vs C5 (dosed) trajectories.
- Retrieval: exact brute-force scan for this experiment (kills ANN variance; legitimate since
  IV is corpus content, not index algorithm; disclose production uses HNSW).
- Measures: origin-session rank, echo-count in top-10, displacement events, repair delta from
  chunk-level and conversation-level defenses.

## Out of scope for these three (reviewer's next ask, requires outside help)

- Multi-operator / held-out human labels; external corpora. None of E1–E3 fixes
  generalization or statistical power (n≈20, single operator, owner-labeled). Scope all claims
  within-operator until then. Bootstrap CIs meaningful only after E2 grades exist.

## Infrastructure notes

- Frozen probe harness proven: scratchpad probe.py pattern (MCP stdio JSON-RPC, --db-path
  clone, empty --projects-dir blocks import contamination). Judge packets + kappa.py reusable.
- Grid arms (c)/(d)/(e) need ReinstateConfig switches or a spike-style side binary — check
  whether config flags suffice before writing new code.
- conv_ prefix regex gotcha: \b fails after underscore — strip prefixes before UUID matching.
