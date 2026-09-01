# coding-agent-life-v1 (agentmemory's corpus) — CSR bench, 2026-09-01

Reproduce:

```bash
git clone https://github.com/iii-dev/agentmemory /tmp/agentmemory-inspect   # any checkout with eval/data/coding-agent-life-v1
csr-engine bench --format agentmemory --data /tmp/agentmemory-inspect/eval/data/coding-agent-life-v1 --k 5 --mode hybrid
```

| System | Dataset | Mode | P@5 | R@5 (fraction gold) | recall_any@5 | MRR | NDCG@10 | query p50 | init-inclusive p50 |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| CSR `12a9aca` | coding-agent-life-v1 (all, 15 queries, 15 sessions) | hybrid | 0.240 | 1.000 | 1.000 | 0.867 | 0.906 | 2.4 ms | 43.2 ms |
| CSR `12a9aca` | same | vector | 0.240 | 1.000 | 1.000 | 0.922 | 0.942 | 2.2 ms | 40.5 ms |
| CSR `12a9aca` | same | fts | 0.240 | 1.000 | 1.000 | 0.897 | 0.905 | 0.3 ms | 4.5 ms |
| agentmemory `565c238` (published, different dataset) | LongMemEval-S cleaned, abstention excluded | hybrid (BM25 .4 / vector .6 / graph 0, reranker off) | — | — | 0.952 | — | — | — | — |

Receipts: commit `12a9acaed5f5e7a8ec88b6a1a216ca0b97983bdd`, embedding `sentence-transformers/all-MiniLM-L6-v2` (FastEmbed, 384-d, local), K=5, scored=15, abstention_dropped=0, Apple silicon, release build, `target/csr-bench/{scores.ndjson,summary.json,table.md}`.

Metric definitions: P@K = relevant retrieved sessions / K. R@K = relevant retrieved sessions / all dataset gold sessions. recall_any@K = 1 when any dataset gold session is in the top K (agentmemory's headline metric, `hit` in their ScoreRow). MRR = reciprocal rank of the first gold session over the 20-deep ranked list. NDCG@10 = binary-gain NDCG with the ideal DCG computed over all dataset gold, not only retrieved gold. query p50 = upper median of query embedding + retrieval; init-inclusive p50 adds the one-time scratch-store indexing (agentmemory's runner also counts init).

What this corpus can and cannot show:

- P@5 saturates at 0.240 by construction: 12 of 15 queries have one gold session and 3 have two, so the ceiling is (12·1 + 3·2)/(15·5) = 0.240. Every mode hits it.
- R@5 = 1.000 in all three modes. The corpus is 15 fictional one-paragraph sessions about a made-up `shipctl` CLI; agentmemory's own grep baseline already scores 0.967 R@5 on it. It separates nothing between vector, keyword and fusion at K=5, only MRR moves (vector 0.922 > fts 0.897 > hybrid 0.867), and on 15 queries one rank swap is 0.03 of MRR.
- The published agentmemory row is a different dataset (LongMemEval-S) and is listed only to show the metric name and its provenance: `recall_any@5` at `565c238` (2026-04-08), vector index over `text.slice(0, 512)` per session, graph weight 0, reranker off. It is not a like-for-like comparison. The like-for-like row is `csr-engine bench --format longmemeval` on the same cleaned dataset, pending (dataset download is a manual step; see the hybrid-ablation note).
- Gold session ids come from the dataset's `goldSessionIds` only; the ranker never influences the denominator.
