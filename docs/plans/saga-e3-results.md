# E3 Results — Controlled Contamination (run 2026-07-17)

Harness: `examples/saga_contamination.rs` (grok-lane, executed end-to-end by the lane,
diff + runs independently verified). Exact brute-force retrieval (no ANN — IV is corpus
content only). Conditions built from C0 = pre-test-episode-sweep-20260611 backup (17,134
chunks): C1 = +eval-design transcript (a4635d59, the session that wrote the queries);
Csham = +size-matched unrelated transcript (107.3% of C1's bytes, zero query-text hits,
grep-evidenced); C5 = +5 synthetic scripted re-ask cycles (verbatim query re-asks +
paraphrased answers, explicitly marked synthetic — controlled self-referential injection,
NOT natural-ecology observation). 8 eligible queries (origins pre-date C0, owner-audited
maps). 3 arms: kNN-exact, full walk (echo defense + provenance rerank), full-minus-echo.

## Headline: dose-response and repair

Echoes in top-10 under C5 (dosed re-asking), mean over 8 queries:

| Arm | echo@10 under C5 | origin-MRR under C5 |
|---|---|---|
| kNN-exact | 4.9 / 10 | 0.014 |
| full minus echo-defense | 4.9 / 10 | 0.014 |
| **full (echo defense on)** | **1.4 / 10** | **0.047** |

1. **Re-ask injection captures naive retrieval**: after 5 scripted cycles, half of kNN's
   top-10 is the injected re-asks; for Q5 the entire top-3 is synthetic cycles. The
   walk without echo defense is equally captured — hop-2 spreads from echo seeds.
2. **Echo defense is the repair, quantified**: −72% echo occupancy (4.9 → 1.4) and 3.4×
   the origin-MRR under contamination. Under C5 the defended walk even RECOVERS an origin
   (A3 enters top-10 at rank 8; absent in every other arm/condition) — demoting echoes
   clears ranked space for provenance. Complements E1's causal finding (removing echo
   defense drops below kNN) from the opposite direction: adding contamination, defense on.
3. **Sham control is null**: Csham (unrelated, size-matched, 167 chunks imported) leaves
   kNN rankings byte-identical to C0 — displacement is content-specific, not a corpus-size
   artifact. C1 (eval-design transcript) inserts ITSELF into Q5's kNN top-3 and nudges
   full-walk ranks (A8 3→4): one stored eval conversation measurably perturbs retrieval.

## Floor-effect disclosure (limits of the origin-rank measure here)

Under exact scan on the June corpus, origins sit outside top-10 for 6-7 of 8 queries in
ALL conditions (consistent with E1/E2: origin-finding unsolved). Origin-rank displacement
is therefore only observable on the 2 queries with ranked origins (A3: kNN rank 7 → gone
under C5; A8: 4→9 kNN, 3→9 no-echo). The dose-response and repair-delta rows above are the
experiment's load-bearing results; origin-rank trajectories are reported but floor-limited.

## Run integrity

- meta per condition: c0 17,134 chunks/0 imported; c1 17,399/265; csham 17,301/167;
  c5 17,150/16 (imported = chunk counts per `import_conversations` contract; file counts
  1/1/5 verified separately — spec ambiguity flagged by the lane, resolved in substance).
- 4 × 25 output lines, all parse; injected re-asks confirmed retrievable (86 echo=true
  hits under C5) — injection succeeded before any claim was scored.
- Deviation: csham source dir substituted (spec's named dirs absent/too small) — 8
  candidates in the 70-130% band all had zero query hits; closest match chosen (107.3%).
- Backups and live DB untouched; condition DBs scratchpad-only.

Artifacts: scratchpad `e3/` (spec.md, conditions/, c5_transcripts/, out/{c0,c1,csham,c5}.jsonl,
score.py). Phase-2 program complete: E2 (graded gold) → E1 (ablation) → E3 (contamination).
