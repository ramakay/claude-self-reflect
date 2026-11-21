# coding-agent-life-v1 (agentmemory's corpus) — CSR bench, 2026-09-01

Reproduce:

```bash
git clone https://github.com/iii-dev/agentmemory /tmp/agentmemory-inspect   # any checkout with eval/data/coding-agent-life-v1
cd csr-engine && cargo build --release
target/release/csr-engine bench --format agentmemory --data /tmp/agentmemory-inspect/eval/data/coding-agent-life-v1 --k 5 --mode hybrid
```

| System | Dataset | Mode | P@5 | R@5 (fraction gold) | recall_any@5 | MRR@20 | NDCG@10 | query p50 | init-inclusive p50 |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| CSR `230447c` | coding-agent-life-v1 (all, 15 queries, 15 sessions) | hybrid | 0.240 | 1.000 | 1.000 | 0.867 | 0.906 | 2.6 ms | 45.7 ms |
| CSR `230447c` | same | vector | 0.240 | 1.000 | 1.000 | 0.917 | 0.937 | 2.4 ms | 45.5 ms |
| CSR `230447c` | same | fts | 0.240 | 1.000 | 1.000 | 0.897 | 0.905 | 0.3 ms | 4.6 ms |
| agentmemory `565c238` (published, different dataset) | LongMemEval-S cleaned, abstention excluded | hybrid (BM25 .4 / vector .6 / graph 0, reranker off) | — | — | 0.952 | — | — | — | — |

Receipts (from each run's `summary.json`): `build_commit=230447c54419241e4db878335bc29c396d0825a3`, `build_dirty=false` (compile-time stamp from `build.rs`, tracked files only), embedding `sentence-transformers/all-MiniLM-L6-v2` (FastEmbed, 384-d, local; not used in `fts` mode), K=5, `ranking_depth=20`, scored=15, abstention_dropped=0, inputs `sessions.json` BLAKE3 `baedd07cd75f47f783ca6df989d9ceb1767c34323b23d477893285a49ec07b2e` and `queries.json` BLAKE3 `bb7f8641bc1d70e97678b09aae9a5f5c4163f1d860b81f1728d78bb1d140dc5f`, Apple silicon, release build, run 2026-09-01T21:56Z. Each run writes `scores.ndjson` (agentmemory's `ScoreRow` shape), `summary.json` and `table.md` under `--out`.

Metric definitions: P@K = relevant retrieved sessions / K. R@K = relevant retrieved sessions / all dataset gold sessions. recall_any@K = 1 when any dataset gold session is in the top K (agentmemory's headline metric, `hit` in their ScoreRow). MRR@20 = reciprocal rank of the first gold session within the 20-deep ranked list, 0 if none. NDCG@10 = binary-gain NDCG with the ideal DCG computed over all dataset gold, not only retrieved gold. query p50 = upper median of query embedding + retrieval. init-inclusive p50 adds the one-time scratch-store build (chunking, embedding, indexing) to every query. agentmemory's `eval/runner/coding-life.ts` inits once and times only the query (their p50 is query-only); their `eval/runner/longmemeval.ts` starts the timer before `adapter.init(q.haystack)`, so on that dataset their latency includes a per-question re-init and is comparable to our init-inclusive column, not the query-only one.

What this corpus can and cannot show:

- P@5 saturates at 0.240 by construction: 12 of 15 queries have one gold session and 3 have two, so the ceiling is (12·1 + 3·2)/(15·5) = 0.240. Every mode hits it.
- R@5 = 1.000 in all three modes. The corpus is 15 fictional one-paragraph sessions about a made-up `shipctl` CLI; agentmemory's own grep baseline already scores 0.967 R@5 on it. It separates nothing between vector, keyword and fusion at K=5; only MRR@20 moves (vector 0.917 > fts 0.897 > hybrid 0.867), and on 15 queries one rank swap is 0.03 of MRR.
- The published agentmemory row is a different dataset (LongMemEval-S) and is listed only to show the metric name and its provenance: `recall_any@5` at `565c238` (2026-04-08), vector index over `text.slice(0, 512)` per session, graph weight 0, reranker off. It is not a like-for-like comparison. The like-for-like row is `csr-engine bench --format longmemeval` on the same cleaned dataset, pending (dataset download is a manual step; see the hybrid-ablation note).
- Gold session ids come from the dataset's `goldSessionIds` only; the ranker never influences the denominator.
- Retrieval goes through the production path (`reflect_gather_pass`: semantic + FTS5 + rerank, project scope `csr-bench`, baseline rerank mode) and collapses to the first hit per conversation; the candidate window doubles until K distinct conversations are found or the search is exhausted, so a session with many matching chunks cannot crowd another out of the top K.
