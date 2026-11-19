---
title: Architecture
---

## Overview

CSR is a single Rust binary of about 138 MB. Storage, search, embeddings, the MCP server, the hooks, enrichment and the companion all run from it. No database servers, no containers, no network services. Claude Code starts the MCP server and the hooks as `csr-engine` processes. The one long-running process is `csr-engine daemon`. On macOS `csr-engine setup` installs it as a launchd agent and starts it (`CSR_NO_DAEMON=1` skips that; `scripts/launchd/` holds a template for doing it by hand). Hooks and the MCP server still work without it, with less: see "The daemon" below.

![CSR architecture: JSONL transcripts are imported, embedded locally and stored in SQLite with an HNSW index; the daemon enriches, the MCP server (16 tools) serves searches, and eight hooks on seven events inject and record context](images/system-map-10.svg)

## What it reads

| Source | Read by | Stored as |
|---|---|---|
| `~/.claude/projects/*/*.jsonl` | the daemon's file watcher, and the Stop and SessionEnd hooks | conversation chunks (`source='conversation'`) |
| `~/.claude/projects/<project>/<session>/subagents/agent-*.jsonl` | the same watcher | sidechain chunks (`source='sidechain'`), scoped to the real project and linked to the parent session. In search the parent wins over the subagent. |
| `~/.claude/tasks/<session>/` | the Stop hook | the session's todos and outcome. A finished task that matches a still-open item becomes a proposal; `csr_resolve` closes it. |
| `~/.claude/plans/*.md` | the daemon, every 30 minutes | plan chunks (`source='plan'`), linked to the conversation that wrote them when the match is clear |
| `~/.codex/sessions/**/rollout-*.jsonl` | the daemon, every 30 minutes, when Codex is installed (`CSR_NO_CODEX_IMPORT=1` stops it) | Codex chunks (`source='codex_rollout'`) |
| `~/.claude/history.jsonl` | the daemon, every 10 minutes | the session registry only. Never embedded, never injected. |
| `~/.claude/projects/*/memory/*.md` | the daemon, every 30 minutes (`CSR_NO_MEMORY_REGISTRY=1` stops it) | names and metadata only. The content is never embedded or injected. |

Claude Code's paste cache is not read. Memory files and pastes stay out of the index on purpose: injecting them back would feed Claude its own notes, and pastes can hold private text.

### One sanitizer

Every source passes through the same sanitizer before it is embedded. It drops the payloads of CSR's own MCP tools and the blocks CSR's hooks injected, so Self-Reflect does not index and re-serve its own output. `csr-engine status` counts what it removed: `csr_tool_blocks_suppressed` and `csr_hook_wrappers_scrubbed`.

## The daemon

`csr-engine daemon` runs these jobs in the background:

- **Import:** watches `~/.claude/projects` for new and changed transcripts, and sweeps every 30 minutes, when idle, for files a watch event missed.
- **Plans, Codex, history and the memory registry**, on the cadences in the table above.
- **Enrichment:** V3 extraction on imported conversations, and AI narratives through the Batch API when an API key is set.
- **Dreaming:** every 6 hours by default (`CSR_DREAM_INTERVAL_SECS` changes it), it re-checks open todos and blockers against the code as it stands. `CSR_NO_DREAMING=1` turns it off.
- **Ratification:** one `claude -p` call per conversation, on your own account. At most 4 calls in any rolling hour, and only conversations active in the last 7 days (`CSR_RATIFICATION_PER_HOUR`, `CSR_RATIFICATION_DAYS`). `CSR_NO_RATIFICATION=1` or `CSR_NO_AI_NARRATIVES=1` stops it.
- **Prompt recall:** it holds the search index in memory and answers the prompt hook over a private Unix socket.
- **Dream journal:** a loopback-only web page on `127.0.0.1:7373`.

The daemon reads `CSR_*` variables from its launch agent. To change one, set it and run `csr-engine setup` again: setup carries the `CSR_*` variables of its own run into the agent.

## Hooks: the session loop

Eight hooks fire on seven events during Claude Code sessions (SessionStart runs two). Each is a `csr-engine hook <name>` process that Claude Code starts. The prompt hook asks the daemon for its context block over a private Unix socket. When the daemon cannot answer in time, the hook falls back to a smaller in-process recall (keyword and identifier lookups, re-ranked with static vectors). It never loads the full index.

![Eight hooks on seven events: three inject context into Claude Code, three record the session into the store, two handle compaction](images/hook-flow-10.svg)

All hooks use catch-all error handling — they log and return OK, never blocking Claude Code.

## The companion

`csr-engine setup` installs the `csr-companion` plugin into Claude Code. The plugin is one generated file. At each session start it starts `csr-engine companion serve` and forwards Claude Code's plugin events to it over a Unix socket. Inside herdr the companion opens a pane beside Claude that runs `csr-engine companion pane --watch`. Without herdr it draws a panel inside Claude Code when you type `/csr`. See [Choose your view](#/docs/companion-views).

## Components

### JSONL Parser (`src/import/`)
Parses Claude Code conversations from `~/.claude/projects/**/*.jsonl`. Splits into searchable chunks and extracts tool context from `tool_use` blocks.

Conversations are embedded in batches.

### Embedding Engine (`src/embeddings/`)
Uses FastEmbed with `all-MiniLM-L6-v2` to generate 384-dimensional vectors locally. No API calls, no cloud service.

| Property | Value |
|----------|-------|
| Model file | Cached locally after the first download |
| Batching | Texts are embedded in batches |

### HNSW Search Index (`src/search/`)
Hierarchical Navigable Small World graph for approximate nearest neighbor search. Index lives in memory, persisted to disk for fast reload. In 10.1, chunk vectors are served from a USearch generation (`src/index_gen/`, an immutable memory-mapped f16 index) once one is built; reflections stay on the HNSW graph.

| Operation | Latency |
|-----------|---------|
| Index lookup (p95) | Timed by `csr-engine eval` on your own index; the target is under 1 ms. It covers the lookup only, not a full recall call. |
| Cached startup | Loads the saved index; the time grows with index size |
| Cold startup | Rebuilds the index from SQLite; slower than a cached load |

Staleness detection via `IndexManifest` — rebuilds only when DB has new data.

### SQLite Storage (`src/storage/`)
Single database at `~/.claude-self-reflect/csr-engine.db`. Thread-safe via `Mutex<Connection>`. WAL mode for concurrent reads.

Stores: conversation chunks, vector embeddings, reflections (stored insights), enrichment state, retrieval events (for TAD scoring), import deduplication state.

### MCP Server (`src/mcp/`)
Built on rmcp. Exposes 16 tools via Model Context Protocol. Runs as a stdio server that Claude Code starts on demand. The MCP server opens no port. The optional `csr-engine daemon` does background work and hosts the loopback-only dream journal.

### Enrichment Pipeline (`src/extraction/`)
Three-layer progressive enrichment. Each layer supersedes the previous in the search index:

| Layer | When | Cost |
|-------|------|------|
| L1 Heuristic | At import | Free |
| L2 V3 Extract | At session end, and the daemon catches up | Free |
| L3 AI Narrative | Batch daemon | Metered, optional |

### AST Code Analysis (`src/extraction/ast_analysis.rs`)
Extracts structural metadata from code found in conversations using tree-sitter grammars via ast-grep. Instead of treating code as opaque text, CSR parses it into an AST and extracts function names, type definitions, and imports — making code searchable by structure.

**Pipeline flow:**

```
source code → language detect → tree-sitter parse → AST → extract nodes → searchable metadata
```

| What's Extracted | Node Kinds |
|-----------------|------------|
| Functions | `function_item`, `function_definition`, `function_declaration` |
| Types/Structs | `struct_item`, `class_definition`, `class_declaration` |
| Imports | `use_declaration`, `import_statement` |

**Supported languages:**

| Language | Extension | Grammar |
|----------|-----------|---------|
| Rust | `.rs` | tree-sitter-rust |
| Python | `.py` | tree-sitter-python |
| TypeScript | `.ts` | tree-sitter-typescript |
| JavaScript | `.js`, `.mjs`, `.jsx` | tree-sitter-javascript |
| Go | `.go` | tree-sitter-go |
| TSX | `.tsx` | tree-sitter-tsx |

AST parsing is wrapped in `catch_unwind` for robustness — malformed code won't crash the engine.

### Continuity Engine (`src/provenance.rs`, `src/ledger/`, `src/governor/`)
Retrieval is provenance-aware. Every chunk carries speaker attribution, source conversation, and supersession state. In the prompt hook (and in MCP search under `CSR_RETRIEVAL_LEGACY=1`), the re-ranker demotes assistant scaffold text (proposals, plans) and boosts primary sources (decisions, outcomes), so recall favors what actually happened. A Derivation Ledger tracks where injected context came from, and an Injection Governor applies reuse tracking and an anti-flap budget so repeated prompts don't thrash the same context in and out.

Verified live: `csr-engine eval --continuity-live` probes the real index (not fixtures) against a grep baseline.

### Code Graph (`src/storage/codegraph.rs`)
Conversation-provenance code graph. AST anchors (function-level, body-hashed) link code symbols to the sessions that created or modified them. Queried via the `csr_code_graph` MCP tool.

### Code Map (`src/codemap/`)
A live map of definitions in supported source files in your repos, per worktree. It is keyed by git blob oid: a worktree is a view of file paths and blob oids, and a blob is parsed once per extractor version however many worktrees hold it. Three signals keep it fresh: a 60 s poll of each active repo, a file watcher with a 5 s debounce, and a queue the PostToolUse hook fills with one insert and no parse. It maps the repos your sessions are in (last 24 hours, at most 16). It only reads repos and stores the map in the same SQLite database. `csr_search_by_file` and `csr_code_graph` answer from it for code no conversation touched.

### Injection Engine (`src/injection/`)
Token-budgeted context injection for hooks (the prompt hook's budget is 500 tokens). The score adds five signals, weighted by hook phase (`src/injection/weights.rs`):

| Phase | Semantic | Recency | File overlap | Error match | Phase boost |
|-------|----------|---------|--------------|-------------|-------------|
| SessionStart | 25% | 10% | 15% | 10% | 40% |
| PromptSubmit | 30% | 25% | 20% | 10% | 15% |
| Stop | 20% | 10% | 10% | 25% | 35% |

## Time decay

MCP search in 10.1 ranks with a cross-encoder by default, and that policy turns the time decay off: the order comes from the fused vector and keyword hits, re-scored by the cross-encoder (see [Search & Retrieval](#/docs/search)). `CSR_RETRIEVAL_LEGACY=1` brings back the older policy, which time-weights results with:

```
score = raw_score * ((1 - 0.3) + 0.3 * 2^(-age_days / 90))
```

That is a 90-day half-life. Under the legacy policy, results that were retrieved and led to successful sessions also get a TAD (Temporal Attention Decay) boost.

The prompt hook has its own recency signal: `2^(-age_days / 14)`, a 14-day half-life, weighted as in the Injection Engine table above.

## Kill switches

Each one is an environment variable set to `1`. For the daemon, set it and run `csr-engine setup` again (see "The daemon" above).

| Variable | Stops |
|---|---|
| `CSR_NO_DREAMING` | dreams |
| `CSR_NO_RECAP` | the session-start recap |
| `CSR_NO_AI_NARRATIVES` | the `claude -p` calls: briefings, stories, ratification, dream commands |
| `CSR_NO_RATIFICATION` | ratification only |
| `CSR_NO_CODEX_IMPORT` | the Codex rollout import |
| `CSR_NO_MEMORY_REGISTRY` | the memory-file registry |
| `CSR_NO_CODEMAP` | the code map |
| `CSR_NO_JOURNAL_SERVER` | the dream journal web page |
| `CSR_NO_MIRROR` | the transcript mirror |
| `CSR_NO_DAEMON` | the launch agent, at setup |

More are in [Configuration](#/docs/configuration). The companion has no variable: `claude plugin disable csr-companion@claude-self-reflect` turns it off.

## Design Decisions

**Why Rust?** — Single binary distribution, no runtime deps, predictable performance, memory safety, about 138 MB in total including ONNX Runtime.

**Why SQLite over Qdrant?** — Eliminated Docker dependency entirely. Single-file database is easy to backup, move, inspect. Good enough for single-user workloads.

**Why HNSW in-memory?** — `sqlite-vec` only supports brute-force KNN. HNSW gives fast in-memory approximate nearest neighbor search.

**Why FastEmbed over Voyage AI?** — No API key required, no network dependency, no per-query cost, privacy-first. Good enough accuracy for conversation search.
