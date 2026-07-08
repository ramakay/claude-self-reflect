---
title: Search & Retrieval
---

## Hybrid Search

Three search strategies merged:

1. **Semantic (HNSW)** — Embeds query, finds conceptually similar content
2. **Keyword (FTS5)** — SQLite full-text for exact terms, error codes, file paths
3. **Reflection Search** — Searches stored reflections (AI-enriched summaries)

## Scoring Pipeline

![Search scoring pipeline — five stages from raw HNSW similarity through decay, TAD boost, cross-project penalty to final ranked results](/claude-self-reflect/images/search-scoring-pipeline.png)

### Decay Over Time

| Age | Multiplier |
|-----|-----------|
| Today | 1.0x |
| 1 week | 0.98x |
| 1 month | 0.93x |
| 3 months | 0.85x |
| 1 year | 0.68x |

### Provenance Re-Ranking (v9.2)

After decay and TAD, results are re-ranked by provenance:
- **Scaffold demotion** — assistant proposals/plans rank below what was actually decided and done
- **Primacy boost** — the conversation where a thing originated outranks later mentions
- **Supersession** — chunks marked superseded by later work rank lower

### TAD (Temporal Attention Decay)

Tracks what you've retrieved before:
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
