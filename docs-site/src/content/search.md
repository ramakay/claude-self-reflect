---
title: Search & Retrieval
---

## Hybrid Search

Three channels feed one ranking:

1. **Semantic** — embeds the query and finds similar chunks (a USearch generation once built, otherwise the HNSW graph) and stored reflections (HNSW)
2. **Keyword (FTS5)** — SQLite full-text over chunks and reflections for exact terms, error codes, file paths, plus an identifier list for code names
3. **Reflection Search** — stored reflections (AI-enriched summaries) take part in both channels above

## Scoring Pipeline

The steps `csr_reflect_on_past` runs by default, in order:

| Step | What happens |
|------|--------------|
| Scope | With no `project`, the search covers the current project and its aliases. It covers every project only when no current project resolves. |
| Vector candidates | Score is 1 minus cosine distance. Anything below `min_score` (default 0.3) is dropped here. |
| Project weight | Inside the scope 1.0x, outside 0.3x. With scope `all`, the current project gets 1.15x. |
| Keyword candidates | Rows found only by keyword get a flat score (0.45 for chunks). |
| Filters | Echoes of CSR's own output and synthetic rows are dropped (`CSR_NO_ECHO_FILTER=1` keeps them). |
| Fusion | Reciprocal-rank fusion of the semantic and keyword rankings (k = 60), one entry per session. Superseded statements are held out and placed after the rest. |
| Cross-encoder | A compiled-in reranker (`ettin-reranker-32m`, tuned) re-scores the top 24 and reorders the sessions. Without the model it keeps the fused order. |
| Exact prefix | A reflection whose text starts with your query goes to the top. |
| Dedupe | A plan folds into the conversation that wrote it, a subagent into its parent, and a Codex copy into the same Claude text. |
| Cut | The first `limit` results (default 5). |

### Legacy policy

`CSR_RETRIEVAL_LEGACY=1` turns the cross-encoder off and brings back the 9.x ranking: time decay, TAD and provenance re-ranking. The decay multiplier is `0.7 + 0.3 * 2^(-age_days / 90)`:

| Age | Multiplier |
|-----|-----------|
| Today | 1.0x |
| 1 week | 0.98x |
| 1 month | 0.93x |
| 3 months | 0.85x |
| 1 year | 0.72x |

Provenance re-ranking, in the legacy policy:
- **Scaffold demotion** — assistant proposals/plans rank below what was actually decided and done
- **Primacy boost** — the conversation where a thing originated outranks later mentions
- **Supersession** — chunks marked superseded by later work rank lower

TAD (Temporal Attention Decay), in the legacy policy, tracks what you've retrieved before:
- Used + session succeeded → boost
- Used + session failed → reduce
- Never retrieved → neutral

## Search Tools

| Tool | Best For |
|------|----------|
| `csr_reflect_on_past` | General semantic search |
| `csr_quick_check` | Fast yes/no check |
| `search_by_recency` | Time-bounded queries |
| `csr_search_by_file` | File-based search |
| `csr_search_by_concept` | Theme-based discovery |
| `get_recent_work` | Activity overview |
| `get_timeline` | Day-by-day activity |
| `csr_search_insights` | Aggregated patterns |
