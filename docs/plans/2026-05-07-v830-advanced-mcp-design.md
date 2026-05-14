# v8.3.0 Plan — Full MCP Advanced Capabilities + HTTP Transport

## Context

v8.2.0 shipped rmcp 1.6 with tool annotations + HNSW reconciliation. v8.3.0 completes the rmcp 1.6 migration by enabling all advanced capabilities and adding a web-accessible HTTP transport via Axum — setting the foundation for the future web dashboard (v9).

## TL;DR — User-Facing Features

1. **Tool argument autocomplete** — type a partial project name, file path, or time range and get instant suggestions
2. **Non-blocking long searches** — large queries run async; client can poll progress or cancel
3. **Interactive confirmation flows** — store_reflection asks "are you sure?" for large content
4. **HTTP transport** — CSR accessible via `http://127.0.0.1:3580` alongside stdio; enables web clients, remote access, multi-client
5. **npm 8.3.0** — `npx claude-self-reflect` installs latest with all capabilities

## Scope

| Feature | Effort | User Value | rmcp Feature |
|---------|--------|------------|--------------|
| **Completions** | Medium | HIGH | `complete()` method |
| **Tasks** | Medium | HIGH | `enqueue_task()` + OperationProcessor |
| **Elicitation (light)** | Small | MEDIUM | Form-based confirmation |
| **StreamableHttp + Axum** | High | VERY HIGH | `transport-streamable-http-server` |
| npm publish | Trivial | HIGH | — |
| CI artifact bump | Trivial | LOW | — |

## Architecture

### Completions (`src/mcp/completions.rs`)

Override `ServerHandler::complete()`. Parameters to autocomplete:

| Parameter | Source | Strategy |
|-----------|--------|----------|
| `project` | `SELECT DISTINCT project FROM conversations` | Prefix filter on DB query |
| `file_path` | `SELECT DISTINCT file_path FROM file_changes` | Prefix filter |
| `time_range` | Static list | "today", "yesterday", "last week", "last month", "last 3 months" |
| `group_by` | Static list | "conversation", "day", "session" |
| `granularity` | Static list | "hour", "day", "week", "month" |
| `session_id` | `SELECT DISTINCT session_id FROM iteration_learnings` | Prefix filter |
| `concept` | Top-k from search index | Semantic nearest neighbors |

Return `CompletionInfo` with up to 100 suggestions, `hasMore` if truncated.

### Tasks (`src/mcp/tasks.rs`)

Wrap heavy search tools in async tasks:

| Tool | Task Mode | Rationale |
|------|-----------|-----------|
| `csr_reflect_on_past` | Yes | Can be slow on 16K+ chunks |
| `csr_search_by_concept` | Yes | Semantic search + aggregation |
| `csr_search_insights` | Yes | Multi-query aggregation |
| `search_by_recency` | Yes | Time filter + semantic |
| Others | No (sync) | Fast enough (<100ms) |

Implementation:
- Use rmcp's `OperationProcessor` for lifecycle
- Enable `ServerCapabilities::tasks`
- `enqueue_task()` spawns tokio task, returns task ID
- `get_task_info()` / `get_task_result()` / `cancel_task()` poll state
- Default TTL: 30 seconds

### Elicitation (`src/mcp/elicitation.rs`)

Light implementation — confirmation flow only:

- `store_reflection` with content > 500 chars triggers elicitation
- Schema: `{ confirm: boolean, tags: optional string[] }`
- If client declines → abort store
- Graceful fallback: if client doesn't support elicitation, proceed without confirmation

### StreamableHttp Transport (`src/transport/http.rs`)

Dual-transport architecture:

```
csr-engine              → stdio transport (default, for Claude Code)
csr-engine serve        → HTTP transport on 127.0.0.1:3580 (for web clients)
csr-engine serve --port → Custom port
```

Stack:
- **Axum** for routing + middleware
- **rmcp StreamableHttpService** as Tower layer
- **In-memory session store** (LocalSessionManager) — sufficient for single-machine
- **SSE** for streaming responses
- Host validation: localhost only by default

New dependency: `axum = "0.8"` (or latest), `tower-http` for CORS/tracing

### CLI Changes

```bash
csr-engine                     # MCP server (stdio) — unchanged
csr-engine serve               # HTTP MCP server on :3580
csr-engine serve --port 8080   # Custom port
csr-engine serve --host 0.0.0.0  # Bind to all interfaces (opt-in)
```

## Files to Create/Modify

| File | Action | Purpose |
|------|--------|---------|
| `Cargo.toml` | Modify | Add axum, tower-http, rmcp features |
| `src/mcp/mod.rs` | Modify | Add complete(), enqueue_task(), enable capabilities |
| `src/mcp/completions.rs` | Create | Completion logic per parameter type |
| `src/mcp/tasks.rs` | Create | Task wrapping + OperationProcessor |
| `src/mcp/elicitation.rs` | Create | Confirmation flow for store_reflection |
| `src/transport/mod.rs` | Create | Transport module |
| `src/transport/http.rs` | Create | Axum server + StreamableHttpService |
| `src/main.rs` | Modify | Add `serve` subcommand to CLI |
| `src/storage/queries.rs` | Modify | Add list_project_names(), list_file_paths(), list_session_ids() |
| `src/storage/mod.rs` | Modify | Expose new query functions |
| `.github/workflows/*.yml` | Modify | download-artifact v4→v8 |
| `installer/package.json` | Modify | Bump version |

## Definition of Done (DoD)

### Quality Gates
- [ ] `cargo test` — all tests pass (338+ existing + new)
- [ ] `cargo clippy -- -D warnings` — 0 warnings
- [ ] `cargo fmt --check` — clean
- [ ] `csr-engine eval --full` — 20/20 pass
- [ ] PR passes: CodeRabbit + claude-review + CodeQL + Snyk

### Feature Acceptance
- [ ] **Completions**: `completion/complete` request with partial "clau" returns "claude-self-reflect" project
- [ ] **Tasks**: `tools/call` with `task: {}` returns task ID; `tasks/get` shows Working→Completed
- [ ] **Elicitation**: `store_reflection` with 600-char content triggers confirmation form
- [ ] **HTTP**: `curl http://127.0.0.1:3580/mcp` returns MCP initialize response
- [ ] **HTTP**: SSE stream works for long-running task results
- [ ] **npm**: `npm publish --dry-run` shows correct version + binary refs

### Regression
- [ ] Existing stdio transport unchanged (default mode)
- [ ] All 12 tool annotations preserved
- [ ] HNSW reconciliation still works
- [ ] Hook startup time unchanged (<100ms)

## Verification Plan

1. Unit tests for each new module (completions, tasks, elicitation, http)
2. Integration test: full MCP protocol round-trip over HTTP
3. Manual test: start `csr-engine serve`, use MCP Inspector to verify
4. `csr-engine eval --full` — confirm 20/20
5. Codex review before merge
6. CI green on all platforms

## Implementation Order

1. **Completions** (smallest, highest immediate value, unblocks others)
2. **Tasks** (medium, pairs with completions for async story)
3. **Elicitation** (small, depends on nothing)
4. **StreamableHttp** (largest, independent, can be parallelized)
5. **npm + CI** (trivial, do last)

## Risk Mitigation

| Risk | Mitigation |
|------|------------|
| Axum version incompatibility with rmcp's tower version | Pin to rmcp's tower version, verify in Cargo.lock |
| StreamableHttp session memory leak | Use TTL-based cleanup, test with load |
| Completions slow on large project lists | Cache project/file lists, refresh on import |
| Tasks capability not supported by Claude Code client | Graceful degradation — sync fallback |
| Elicitation not supported by client | Check client capabilities, skip if unsupported |
