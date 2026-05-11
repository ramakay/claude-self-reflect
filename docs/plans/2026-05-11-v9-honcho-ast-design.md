# CSR v9 — HonchoAST Design (Self-Built Dreamer + AST v2)

## Decision: Build, Don't Bundle

Rejected Honcho integration (AGPL, PostgreSQL dep, second process, token budget wars).
CSR already has 90% of the pieces. Upgrade what exists.

## Architecture: Same Single Binary

```
csr-engine (MIT, single binary)
  ├── MCP server (12 tools)
  ├── Embeddings (FastEmbed, local)
  ├── Search (HNSW, <1ms)
  ├── Storage (SQLite)
  ├── AST v2 (tree-sitter incremental + ast-grep patterns)
  ├── 6 hooks (with session-aware review context)
  ├── Dreamer v1 (upgraded daemon consolidation)
  └── 3-layer enrichment (heuristic → extraction → AI narrative → consolidated facts)
```

## 3 Deliverables

### 1. Dreamer v1 — Upgraded Daemon Consolidation (~45 min)
- Upgrade `csr-engine daemon` consolidation prompts
- Turn raw sessions into durable **facts / decisions / conventions**
- Output types: `architectural_decision`, `convention`, `preference`, `bug_pattern`, `refactoring_intent`
- Store as tagged reflections searchable by type
- Reuse existing `BatchClient` trait + Anthropic Batch API
- Supersession: consolidated reflection replaces raw session reflections in search index

### 2. AST v2 — Incremental Code Evolution Tracking (~45 min)
- tree-sitter incremental parsing in PostToolUse hook
- Capture before/after AST snapshots per edit
- Diff: new functions, removed functions, renamed symbols, changed signatures
- Store diffs as `code_evolution` records in SQLite
- New MCP tool: `csr_code_evolution` — "what changed structurally across sessions?"
- Foundation for future: refactoring pattern detection, architectural drift alerts

### 3. Session-Aware Review Context (~30 min)
- Wire consolidated facts + AST diffs into UserPromptSubmit injection
- Example output: "This edit touches AuthService.validate() — you refactored this 3 sessions ago. Convention: handlers should not query DB directly."
- Combine: Dreamer conclusions + AST impact analysis + existing semantic search
- Token budget: 500 tokens max (fits existing injection framework)

## Competitive Positioning

| Feature | CSR v9 | claude-mem | Honcho |
|---------|--------|-----------|--------|
| Code-aware memory | **AST v2 + evolution** | ❌ text only | ❌ text only |
| Consolidation | **Dreamer v1 (local)** | ❌ raw storage | ✅ Dreamer (cloud/AGPL) |
| Zero dependencies | **✅ single binary** | ❌ ChromaDB | ❌ PostgreSQL |
| Search latency | **<1ms** | ~10ms | network RTT |
| License | **MIT** | MIT | AGPL-3.0 |

## Session Plan (2 hours)

| Time | Action |
|------|--------|
| 0-45 min | Dreamer v1: upgrade daemon prompts, add fact/decision/convention types |
| 45-90 min | AST v2: tree-sitter incremental diffing in PostToolUse hook |
| 90-120 min | Session-aware review context in UserPromptSubmit injection |

## Success Criteria

- [ ] `csr-engine daemon` produces consolidated facts from raw sessions
- [ ] PostToolUse captures AST before/after diffs
- [ ] UserPromptSubmit injects code-aware context referencing past decisions
- [ ] All existing tests pass + new tests for each feature
- [ ] Single binary, no new external dependencies
