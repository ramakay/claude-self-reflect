# Claude Self-Reflect v8.0 — Action Guide

## Architecture

Single Rust binary (`csr-engine`). No Python, no Docker, no Qdrant.

```
csr-engine (44MB)
  ├── MCP server (rmcp, 13 tools)
  ├── Embeddings (FastEmbed, 384-dim, local)
  ├── Search (HNSW, <1ms p95)
  ├── Storage (SQLite)
  ├── AST analysis (ast-grep, 6 languages)
  ├── 6 Claude Code hooks
  └── 3-layer enrichment pipeline
```

## Key Commands

```bash
csr-engine                     # Start MCP server (default)
csr-engine setup               # Import + register MCP + install hooks
csr-engine status              # System status (JSON)
csr-engine status --compact    # Statusline output
csr-engine daemon              # Background enrichment (AI narratives)
csr-engine hook install --apply # Install/update hooks
csr-engine eval                # Quick eval (5 tests, ~7ms)
csr-engine eval --full         # Full eval (20 tests, ~200ms)
csr-engine quality <file>      # AST code quality analysis
```

## MCP Tools (13 total)

```
csr_reflect_on_past   — Semantic search across past conversations
store_reflection      — Store insights for future retrieval
csr_quick_check       — Fast existence check
search_by_recency     — Time-constrained search
get_recent_work       — Session-grouped recent activity
get_timeline          — Activity timeline with stats
csr_search_by_file    — Find conversations touching a file
csr_search_by_concept — Theme-based search
csr_search_insights   — Aggregated patterns
csr_get_more          — Paginate results
get_full_conversation — Complete JSONL retrieval
get_session_learnings — Iteration memory for Ralph loops
csr_code_graph        — Which conversations shaped a function/file (AST anchors)
```

## Critical Rules

1. **PATH RULE**: Always use `/Users/username/...` never `~/...` in MCP commands
2. **TEST RULE**: Never claim success without running `cargo test`
3. **RESTART RULE**: After modifying MCP server code, restart Claude Code
4. **QUALITY GATE**: When pre-commit hook blocks, fix the issue — never use `--no-verify`

## Development

```bash
# Build
cd csr-engine && cargo build --release

# Test (52 unit + 68 hooks integration + 44 Phase 1 integration)
cargo test
cargo test --test hooks_integration
cargo test --test integration

# Format + lint
cargo fmt && cargo clippy

# Benchmarks
cargo bench --bench spike_bench
```

### Key Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| rmcp | 1.6 | MCP protocol (tool annotations, macros) |
| fastembed | 5.9 | Local embeddings |
| hnsw_rs | 0.3 | Vector search |
| rusqlite | 0.38 | SQLite storage |
| ast-grep-core | 0.40 | AST analysis |
| sonic-rs | 0.3 | Fast JSON parsing |
| schemars | 1.x | Schema gen (must be v1 for rmcp) |

### Key Patterns

- **rmcp tool params**: Use `Parameters<MyStruct>` pattern, NOT individual `#[tool(param)]`
- **rmcp tool annotations**: All 13 tools have `annotations(read_only_hint, destructive_hint, idempotent_hint)` in macro
- **rmcp 1.6 builders**: `ServerInfo::new(caps).with_instructions()`, `Implementation::new()`, `ReadResourceResult::new()`
- **fastembed**: Requires `aarch64` Rust — no x86_64-apple-darwin ONNX binaries
- **rusqlite 0.38**: No `ToSql` for `usize` — cast to `i64`
- **Storage thread safety**: Wrap `Connection` in `std::sync::Mutex`
- **EmbeddingEngine**: `embed` requires `&mut self`, wrap in `Mutex`
- **Hooks**: All use catch-all wrappers — never block Claude Code
- **System sqlite3 (macOS)**: cannot load fts5 — CLI inspection silently skips `chunks_fts` (integrity checks look ~10x faster than the bundled engine's). Use `csr-engine status --deep` or Homebrew sqlite3.
- **integrity_check**: never call raw `PRAGMA integrity_check` on the hot path — ~10s CPU on multi-GB DBs; use `Storage::integrity_check_cached` (meta-table cache, daemon refreshes)
- **AI narratives**: `claude -p` (model chain: `CSR_NARRATIVE_MODEL` → `haiku` → CLI default); usage counted in `narrative_usage` table, shown in `csr-engine status`; kill switch `CSR_NO_AI_NARRATIVES=1`.

## Hooks

6 hooks fire at strategic moments:

| Hook | When | What |
|------|------|------|
| SessionStart | Session begins | Injects past context (framed as history, not instructions) |
| UserPromptSubmit | Every prompt | Predictive context injection |
| PostToolUse | After Edit/Write | Tracks file changes |
| Stop | Every response | Stores iteration learnings |
| PreCompact | Before compaction | Backs up state |
| SessionEnd | Session ends | Stores narrative |

Hook CLI: `csr-engine hook session-start|session-end|precompact|stop|post-tool-use|prompt-submit|install`

## File Layout

| What | Where |
|------|-------|
| Engine source | `csr-engine/src/` |
| MCP tools | `csr-engine/src/mcp/tools.rs` |
| Hooks | `csr-engine/src/hooks/` |
| Tests | `csr-engine/tests/` |
| npm installer | `installer/` |
| Docs site (GH Pages) | `docs-site/` |
| Install script | `scripts/install.sh` |
| Data (user) | `~/.claude-self-reflect/` |
| Conversations | `~/.claude/projects/*/` |

## Upgrading from v7.x (Python)

v8.0 replaces the entire Python/Docker/Qdrant stack:

```bash
docker compose down 2>/dev/null   # Stop old services
curl -fsSL .../scripts/install.sh | sh  # Install v8
```

The Rust binary re-imports from the same `~/.claude/projects/` JSONL files.
`csr-engine hook install --apply` auto-replaces Python hooks with Rust hooks.

## Documentation

Primary docs: https://ramakay.github.io/claude-self-reflect/ (GitHub Pages, built from `docs-site/`)

---
*Plans and design docs: `docs/plans/`*
