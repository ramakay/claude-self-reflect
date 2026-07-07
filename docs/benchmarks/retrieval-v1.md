# Retrieval Benchmark v1 — Embedding Model Comparison

First labeled retrieval benchmark for csr-engine. Unlike `eval --full` (health
checks), this measures retrieval *quality* against ground truth: 120 synthetic
coding-session memory chunks and 60 paraphrased queries, each labeled with the
chunk(s) that answer it. Corpus lives at `csr-engine/src/eval/data/retrieval_v1.json`
and is embedded in the binary.

Run it:

```bash
csr-engine eval --benchmark --models minilm-l6,bge-small,arctic-xs
```

## Results (2026-07-07, linux x86_64, CPU)

| Model | Dim | R@1 | R@5 | MRR@10 | Doc embed (120 docs) |
|---|---|---|---|---|---|
| minilm-l6 (current default) | 384 | 0.800 | 0.967 | 0.862 | 1.5s |
| minilm-l12 | 384 | 0.817 | 0.983 | 0.885 | 2.9s |
| bge-small-en-v1.5 | 384 | 0.800 | 0.983 | 0.884 | 2.9s |
| snowflake-arctic-embed-xs | 384 | 0.767 | 1.000 | 0.869 | 1.5s |
| multilingual-e5-small | 384 | 0.667 | 0.867 | 0.753 | 4.1s |
| jina-embeddings-v2-base-code | 768 | 0.783 | 0.983 | 0.867 | 13.3s |

Per-category standout: on `error-recall` queries (paraphrased "what was that
error about X"), bge-small and jina-code hit R@1 0.800 vs minilm-l6's 0.600.

## Interpretation

- **No urgent swap is justified.** At n=60 queries, one query ≈ 1.7 points;
  the top four models are within noise of each other on aggregate metrics.
  The "MiniLM is outdated" assumption is not supported by this corpus.
- **bge-small is the strongest candidate if/when we swap**: never worse than
  minilm-l6 on any aggregate metric, best MRR@10, and a large win on
  error-recall — at ~2x embed cost (still fast). Requires the BGE query
  prefix at search time and a full re-index (same 384 dims).
- **The code-tuned jina-code model does not pay for itself here**: 8.6x slower
  embedding and 2x storage for no aggregate gain. Conversation memory is
  mostly natural-language narrative, not raw code — a code-tuned encoder
  isn't automatically better for it.
- **Prefixes matter**: BGE/E5/Arctic models expect retrieval prefixes that
  fastembed does not add automatically; the harness applies them per model
  (see `supported_models()` in `csr-engine/src/eval/benchmark.rs`).

## Next steps

1. Grow the corpus (target 500+ queries) so 2–3 point differences become
   significant; add hard-negative clusters and multi-hop queries.
2. Gate any default-model swap on a rerun of this benchmark plus embedding
   versioning + automatic re-index support in storage.
3. Use this corpus as the eval set for a future fine-tuned `csr-embed` model —
   fine-tuning without this baseline would have been unmeasurable.
