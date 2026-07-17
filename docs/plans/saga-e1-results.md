# E1 Results — Channel Ablation Grid (run 2026-07-17)

Harness: `examples/saga_ablation.rs` (7 arms, one process, ONE HNSW build: 73,342 chunks,
build stamped in output meta — satisfies the ANN comparability requirement). Corpus: fresh
clone of eval-frozen-2026-07-15.db. Scoring: E2 ratification-derived graded gold
(docs/plans/saga-e2-results.md); convs outside the graded pool score 0 and are counted.

## Grid (20 queries; origin-MRR over the 12 owner-mapped queries)

| Arm | origin-MRR | nDCG@10 | R≥2@10 | ungraded@10 |
|---|---|---|---|---|
| a kNN baseline | 0.243 | 0.394 | 0.469 | 15 |
| b full reinstatement | 0.276 | 0.477 | 0.559 | 6 |
| c blend-only | 0.269 | 0.446 | 0.499 | 6 |
| **d graph-only** | **0.329** | **0.555** | **0.607** | 20 |
| e episode-only | 0.321 | 0.457 | 0.470 | 15 |
| f full minus rerank | 0.257 | 0.447 | 0.563 | 10 |
| g full minus echo-defense | 0.234 | 0.434 | 0.549 | 8 |

## Findings

1. **Graph spread is the workhorse.** d_graph_only beats the full walk on every metric
   (origin-MRR 0.329 vs 0.276, nDCG 0.555 vs 0.477) — and does so despite the scoring
   bias against it (20 ungraded convs @10 scored as 0; the E2 pool was built from the
   A/B arms + file-touch, so channels that surface novel conversations are penalized,
   not helped). Code-graph structure, not semantic blending, carries the provenance signal.
2. **Fusion dilutes.** b_full underperforms its own graph channel: max-score fusion lets
   blend-sourced semantic neighbors crowd graph-sourced provenance neighbors out of the
   final 10. Tuning direction for Phase 3: raise graph share (higher graph_boost /
   per-channel quotas), not more channels.
3. **Echo defense is validated causally.** Removing it (arm g) drops origin-MRR to 0.234 —
   below the kNN baseline. The observer-effect defense is not decoration; without it the
   walk launches from sessions that asked the question and never reaches origins.
4. **Provenance rerank earns its keep** (f: −0.019 oMRR, −0.030 nDCG vs b).
5. **Episode chain is a strong origin-finder** (e: 0.321 oMRR) but weak on graded depth
   (R≥2 0.470) — it finds the origin thread, not the full evidence set. Complementary to
   graph; candidates for a graph+episode arm without blend in Phase 3.
6. **Blend-only ≈ kNN** (0.269 vs 0.243) — the Rocchio-style hop adds little alone.

## Comparability disclosures

- Cross-experiment absolute numbers differ from E2's table (a_knn 0.243 here vs arm A
  0.193 in E2): different index build (Jul 17 vs Jul 15) + harness differences. Within-E1
  comparisons share one build and are clean; only those are claimed.
- Ungraded@10 = count of top-10 slots holding conversations absent from the E2 graded
  pool (scored 0, conservative). High for d/e — their true scores are lower bounds.
- Origin-MRR n=12 (owner-mapped, 5/5 injected maps audit-confirmed); UNRESOLVED and OOC
  strata excluded per protocol.

Artifacts: scratchpad `e1/` (spec.md, ablation.jsonl with build meta, run.log, score.py,
results.json). Binary: `cargo run --release --example saga_ablation` with
CSR_ABLATION_DB/PROJECTS/OUT env vars.
