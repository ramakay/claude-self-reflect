---
title: Architecture
---

## Overview

CSR is a single 44MB Rust binary. Everything — storage, search, embeddings, MCP server, hooks, and enrichment — runs in one process. No daemons, no database servers, no containers, no network services.

![CSR Architecture Diagram](/claude-self-reflect/images/arch-diagram.png)

## Hooks: The Active Memory Loop

Six hooks fire at strategic moments during Claude Code sessions. They run inline as CLI subcommands — no background daemon.

![Hooks Lifecycle Diagram](/claude-self-reflect/images/hooks-diagram.png)

All hooks use catch-all error handling — they log and return OK, never blocking Claude Code.

## Components

### JSONL Parser (`src/import/`)
Parses Claude Code conversations from `~/.claude/projects/**/*.jsonl`. Splits into searchable chunks and extracts tool context from `tool_use` blocks. Since ~74% of Claude Code sessions are tool interactions, this extraction produces 2.8x more searchable content.

**Speed**: ~20 conversations/second using batch embedding.

### Embedding Engine (`src/embeddings/`)
Uses FastEmbed with `all-MiniLM-L6-v2` to generate 384-dimensional vectors locally. No API calls, no cloud service.

| Operation | Latency |
|-----------|---------|
| Single embed | 2.5ms |
| Batch (10 texts) | 7.3ms (0.73ms each) |
| Model size | ~23MB (cached locally) |

### HNSW Search Index (`src/search/`)
Hierarchical Navigable Small World graph for approximate nearest neighbor search. Index lives in memory, persisted to disk for fast reload.

| Operation | Latency |
|-----------|---------|
| Search (p95) | < 1ms |
| Cached startup | ~150ms (p50, 54K-chunk index) |
| Cold startup | ~14s (rebuild from SQLite) |

Staleness detection via `IndexManifest` — rebuilds only when DB has new data.

### SQLite Storage (`src/storage/`)
Single database at `~/.claude-self-reflect/csr-engine.db`. Thread-safe via `Mutex<Connection>`. WAL mode for concurrent reads.

Stores: conversation chunks, vector embeddings, reflections (stored insights), enrichment state, retrieval events (for TAD scoring), import deduplication state.

### MCP Server (`src/mcp/`)
Built on rmcp. Exposes 15 tools via Model Context Protocol. Runs as stdio server — Claude Code starts it on demand. No HTTP, no ports, no long-running daemon.

### Enrichment Pipeline (`src/extraction/`)
Three-layer progressive enrichment. Each layer supersedes the previous in the search index:

| Layer | When | Cost | Quality Score |
|-------|------|------|---------------|
| L1 Heuristic | At import | Free | 0.074 |
| L2 V3 Extract | At session end | Free | 0.345 |
| L3 AI Narrative | Batch daemon | $0.012/conv | 0.691 |

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
Provenance-aware retrieval added in v9.2. Every chunk carries speaker attribution, source conversation, and supersession state. The re-ranker demotes assistant scaffold text (proposals, plans) and boosts primary sources (decisions, outcomes), so recall favors what actually happened. A Derivation Ledger tracks where injected context came from, and an Injection Governor applies reuse tracking and an anti-flap budget so repeated prompts don't thrash the same context in and out.

Verified live: `csr-engine eval --continuity-live` probes the real index (not fixtures) against a grep baseline.

### Code Graph (`src/storage/codegraph.rs`)
Conversation-provenance code graph. AST anchors (function-level, body-hashed) link code symbols to the sessions that created or modified them. Queried via the `csr_code_graph` MCP tool.

### Injection Engine (`src/injection/`)
Token-budgeted context injection for hooks. Multi-signal scoring model:

| Signal | Weight |
|--------|--------|
| Semantic similarity | 50% |
| Recency | 20% |
| File overlap | 20% |
| Error pattern match | 10% |

## Memory Decay

Search results are time-weighted using:

```
score = raw_score * ((1 - 0.3) + 0.3 * 2^(-age_days / 90))
```

90-day half-life. Recent results score higher, but old results with strong semantic match still surface. Results that were retrieved and led to successful sessions get a TAD (Temporal Attention Decay) boost.

## Design Decisions

**Why Rust?** — Single binary distribution, no runtime deps, predictable performance, memory safety, 44MB total including ONNX Runtime.

**Why SQLite over Qdrant?** — Eliminated Docker dependency entirely. Single-file database is easy to backup, move, inspect. Good enough for single-user workloads.

**Why HNSW in-memory?** — `sqlite-vec` only supports brute-force KNN. HNSW gives sub-millisecond approximate nearest neighbor search at 15K+ vectors.

**Why FastEmbed over Voyage AI?** — No API key required, no network dependency, no per-query cost, privacy-first. Good enough accuracy for conversation search.
