---
title: MCP Tools Reference
---

13 tools available to Claude automatically via MCP.

## Search Tools

### csr_reflect_on_past
Semantic search across all past conversations.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| query | string | required | Search query |
| limit | int | 5 | Max results (1-50) |
| project | string | auto | Project filter. "all" for cross-project |
| min_score | float | 0.3 | Minimum similarity (0-1) |

### csr_quick_check
Fast existence check — count + top match only.

### search_by_recency
Time-constrained search. Supports: "today", "last week", "last 3 months", etc.

### csr_search_by_file
Find conversations that discussed or modified a file. Uses 2-component matching (parent + filename).

### csr_search_by_concept
Theme-based cross-project search. E.g., "security patterns", "error handling".

### csr_search_insights
Aggregated patterns from search results.

### csr_get_more
Pagination for additional results.

### csr_code_graph
Conversation-provenance code graph. Query which conversations touched a function or file — AST anchors link code symbols to the sessions that shaped them.

## Activity Tools

### get_recent_work
"What did we work on?" — grouped by day and project.

### get_timeline
Day-by-day activity timeline with statistics.

### get_full_conversation
Retrieve complete JSONL conversation by ID.

### get_session_learnings
Iteration-level memory for Ralph loops.

## Storage Tools

### store_reflection
Store insights for future retrieval. Embedded and indexed immediately.

## Tool Selection Guide

| I want to... | Use |
|--------------|-----|
| Find past conversations | `csr_reflect_on_past` |
| Check if we discussed X | `csr_quick_check` |
| Recent activity | `get_recent_work` |
| File-specific search | `csr_search_by_file` |
| Cross-project concepts | `csr_search_by_concept` |
| Which sessions shaped this function | `csr_code_graph` |
| Save a decision | `store_reflection` |
