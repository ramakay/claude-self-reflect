---
title: MCP Tools Reference
---

16 tools available to Claude automatically via MCP.

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
Fast existence check — count + top match only, with measured abstention.

The score floor is derived from probing topics that were never discussed
against the full corpus. Below the floor the tool refuses rather than
fabricates:

```xml
<quick_search>
  <found>false</found>
  <count>0</count>
  <best_rejected_score>0.31</best_rejected_score>
  <floor>0.45</floor>
</quick_search>
```

Scores in the weak band (0.45–0.62) are returned but carry an explicit
warning — matches there are not distinguishable from never-discussed topics,
so the preview is the evidence, not the score:

```xml
<relevance>weak</relevance>
<warning>weak match — may be spurious. Scores in 0.45–0.62 are not
distinguishable from topics that were never discussed. Read the preview
before treating this as evidence the topic came up.</warning>
```

### search_by_recency
Time-constrained search. Supports: "today", "last week", "last 3 months", etc.

### csr_search_by_file
Find conversations that discussed or modified a file. Uses 2-component matching (parent + filename).

For code files with graph coverage, returns a per-symbol ledger with
two-channel attribution — each symbol shows *how* its origin is known,
never a guess:

```xml
<symbol kind='function' name='rerank_pool'
        attribution='transcript:bb1688ad + git:7dd95a7b'
        body_hash='a469aec4' lines='158-193'/>
<symbol kind='function' name='blend'
        attribution='git:33559079' .../>
```

`transcript:` = earliest recorded change event naming the symbol;
`git:` = the introducing commit from `git log -L` over the symbol's span.
Channels that disagree by more than 48h are shown labeled, never merged.
Symbols with no evidence in either channel render `unattributed` — they do
not inherit their file's first toucher.

In v10.1 the tool also appends a `<code_map>` section from the live code map (no new tool; there are still 16). For each matching path (an absolute path by its worktree, else an exact repo-relative path, else a suffix on a `/` boundary, at most 10) it lists the worktrees holding the file, grouped by blob (`same blob in main, a; differs in b (dirty)`), plus the definitions in the blob most worktrees share. When no conversation touched the file, it says so and the map is the answer. The section is at most 1,500 bytes and read-only. `CSR_NO_CODEMAP=1` removes it.

### csr_search_by_concept
Theme-based cross-project search. E.g., "security patterns", "error handling".

### csr_search_insights
Aggregated patterns from search results.

### csr_get_more
Pagination for additional results.

### csr_code_graph
Conversation-provenance code graph. Query which conversations touched a function or file — AST anchors link code symbols to the sessions that shaped them.

Every edge and origin claim is evidence-labeled: call/import
edges resolve only through recorded witnesses (or state why they cannot —
`external`, `method-dispatched`, `stale`, `local`), symbol origins come from
the two-channel attribution described under `csr_search_by_file`, and the
system abstains visibly (`unattributed`, `unverified:`) instead of guessing.

In v10.1 the tool also appends a `<code_map>` section with the map's definitions of the exact symbol name. When the conversation graph does not know the symbol, the answer comes from the map. The map holds definitions, not call edges, and the answer says so: callers and callees are not in it. The section is at most 1,500 bytes and read-only. `CSR_NO_CODEMAP=1` removes it.

## Activity Tools

### get_recent_work
"What did we work on?" — grouped by day and project.

### get_timeline
Day-by-day activity timeline with statistics.

### get_full_conversation
Retrieve complete JSONL conversation by ID.

### get_session_learnings
Iteration-level memory for Ralph loops.

### csr_transcript

Structured facts from one Claude Code session transcript, without a hand-rolled `jq` or Python parser. Pick a `view`: `stats`, `prompts`, `tools`, `files`, `errors`, `slice` or `grep`. Every view shows its turn numbers, so a follow-up `slice` can zoom in. Large results end with a truncation marker and a `turns` hint to resume. The `prompts` view shows only turns you typed unless you pass `include_system: true`. Read-only.

```python
csr_transcript(session="a1b2c3d4", view="grep", grep="migration")
```

## Storage Tools

### csr_why

Provenance chain — why does this code or decision exist. Reinstatement recall: seed search, blended second hop through the code graph and episode chains.

```python
csr_why("why does the rerank scaffold demotion exist")
```

### csr_resolve

Record a verified verdict about chunks surfaced in search results: `resolved`, `still_open`, or `regressed`. Append-only ledger — future searches annotate these chunks and demote resolved ones within the page; a `regressed` verdict re-opens them. Use after verifying a recalled item against the repo or real world. Pass chunk ids from the `<id>` tags in search results.

```python
csr_resolve(chunk_ids=["..."], status="resolved", evidence="shipped vc75, verified in app.json")
```

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
