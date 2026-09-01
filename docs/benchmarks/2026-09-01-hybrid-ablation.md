# Hybrid retrieval ablation (CSR_SEARCH_MODE), 2026-09-01

Since `d0b97cb` FTS5 runs on every reflect call and the final order is reciprocal-rank fusion (k=60) over the reranked semantic order and the bm25 order; before it, FTS5 ran only when the top semantic score fell below 0.5. `CSR_SEARCH_MODE=hybrid|vector|fts` (default hybrid) switches the path at runtime; `csr-engine bench --mode` sets the same enum explicitly.

## Live corpus, exact-identifier queries

Maintainer corpus, 223,546 chunks, `csr_reflect_on_past` over MCP stdio, limit 5, project `all`. "content hits" = result lines containing the queried identifier (the query echo line excluded).

| Query | old binary (fallback gate) | new, hybrid | new, vector | new, fts |
|---|---:|---:|---:|---:|
| `active_forgetting_enabled_from` | 4 | 7 | 0 | 7 |
| `score_fts_candidate` | 3 | 7 | 3 | 7 |

Vector-only misses the first identifier entirely; the old gate found it only because the top cosine happened to fall under 0.5. Receipts: `/tmp/csr-b5/smoke.py` run 2026-09-01 ~20:30Z against `target/release/csr-engine` at `d0b97cb` and `/usr/local/bin/csr-engine` at `519cafb`.

## Curated evals, before and after

`csr-engine eval --full` (20 tests) and `eval --continuity` (6/6) are line-for-line identical between the pre-change installed binary and `d0b97cb`; the one failing line in both is the trained re-ranker gate (`never_run`), unrelated.

## coding-agent-life-v1, three modes

See `2026-09-01-coding-agent-life-v1.md`: R@5 = 1.000 and P@5 = 0.240 (the ceiling) in all three modes; only MRR moves (vector 0.922, fts 0.897, hybrid 0.867) on 15 queries. The corpus is too small and too keyword-friendly to rank the modes.

## Pending

`csr-engine bench --format longmemeval --data <longmemeval_s_cleaned.json> --limit 100` in all three modes, then the full 500. The 264 MB dataset (`xiaowu0162/longmemeval-cleaned` on Hugging Face) is downloaded by hand, never by the binary.
