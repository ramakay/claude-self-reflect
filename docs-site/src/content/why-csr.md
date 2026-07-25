---
title: Why Claude Self-Reflect?
---

## The Memory Problem

Claude Code has no persistent memory. Every session starts from zero. Architecture decisions, debugging breakthroughs, project conventions — gone when the session ends.

## What Makes CSR Different

### Single Binary, Not a Service Stack

Most memory tools require multiple services: vector databases, Python runtimes, Docker containers, background daemons, web UI servers.

CSR is a single 44MB binary. SQLite, HNSW search, FastEmbed embeddings, MCP server, and all 6 hooks — compiled into one executable.

| | CSR | Typical Memory Tool |
|--|-----|---------------------|
| **Install** | `curl \| sh` (one command, asks before activating) | npm + Docker + Python + DB |
| **Dependencies** | None | Docker, Python, vector DB, Node.js |
| **Processes** | 1 (on-demand) | 3-5 background services |
| **Startup** | ~150ms (cached) | Seconds to minutes |
| **Search** | <1ms (HNSW) | 10-100ms typical |

### Active Injection, Not Passive Search

Most memory tools give you a search box. CSR also **actively injects** relevant context at six strategic moments:

- **SessionStart** — Searches history and surfaces relevant past work before you ask
- **UserPromptSubmit** — Predicts context needed from your prompt using multi-signal scoring (semantic 50%, recency 20%, file overlap 20%, error patterns 10%)
- **SessionEnd** — Auto-generates searchable narratives. No manual tagging
- **Stop** — Detects stuck patterns, surfaces anti-patterns from past sessions
- **PreCompact** — Backs up state before context compaction
- **PostToolUse** — Tracks file edits with session-scoped deduplication

### Progressive Enrichment

3-layer pipeline that progressively improves search quality:

1. **Layer 1: Heuristic** (instant, free) — Extracts key patterns at import time
2. **Layer 2: V3 Extraction** (inline, free) — Structured extraction at session end
3. **Layer 3: AI Narrative** (optional, ~$0.012/conv) — Full AI narratives. **9.3x search quality improvement**

### Cross-Project Memory

Indexes ALL your Claude Code projects automatically. Solutions from one project surface in another. No per-project configuration.

### Privacy First

Everything runs on your machine by default. No cloud APIs, no accounts, no telemetry. The only optional cloud feature is AI Narratives (Layer 3).

### Consent First

Install and activation are separate steps. Installing (via `curl | sh` or npm) only downloads the SHA256-verified binary — nothing touches `~/.claude/settings.json`, no MCP server is registered, and no conversations are indexed until you explicitly approve `csr-engine setup` (interactive installs prompt; non-interactive installs never activate on their own). See [Installation](#/docs/installation) for the `CSR_AUTO_SETUP` / `CSR_SKIP_SETUP` controls.
