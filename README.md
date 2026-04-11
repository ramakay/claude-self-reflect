# Claude Self-Reflect

<div align="center">

[![npm version](https://badge.fury.io/js/claude-self-reflect.svg)](https://www.npmjs.com/package/claude-self-reflect)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Claude Code](https://img.shields.io/badge/Claude%20Code-Compatible-6B4FBB)](https://github.com/anthropics/claude-code)
[![MCP Protocol](https://img.shields.io/badge/MCP-Enabled-FF6B6B)](https://modelcontextprotocol.io/)
[![Local First](https://img.shields.io/badge/Local%20First-Privacy-4A90E2)](https://github.com/ramakay/claude-self-reflect)

</div>

**Claude forgets everything. This fixes that.**

Single 44MB binary. No databases. No containers. No API keys required. Just install and search.

## Quick Install

```bash
# Download and install the binary
curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh

# One-shot setup: imports conversations, registers MCP, installs hooks
csr-engine setup

# Restart Claude Code. Done.
```

<details>
<summary>Alternative: npm install</summary>

```bash
npm install -g claude-self-reflect
# Then install the binary separately:
curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh
csr-engine setup
```

</details>

## Before & After

```
You: "How did we fix that CPU usage bug?"
Claude (without CSR): "I don't have access to previous conversations."

Claude (with CSR):    "Found it - we fixed the circular reference causing
                       100% CPU in the server modularization. The fix was
                       in store_reflection where dimension mismatch created
                       separate local and cloud collections."
```

```
You: "What about that memory issue last week?"
Claude (with CSR):    "The container was limited to 2GB but only using
                       266MB. Issue only happened with MAX_QUEUE_SIZE=1000
                       outside the container. With proper limits, memory
                       stayed stable at 341MB."
```

## Performance

| Metric | Value |
|--------|-------|
| **Cached startup** | 93ms |
| **Search latency (p95)** | <1ms |
| **Binary size** | 44MB |
| **Import speed** | ~20 conversations/sec |
| **Embedding** | 0.73ms/text (batch) |
| **Conversations indexed** | 900+ tested |

## How It Works

```
~/.claude/projects/**/*.jsonl
         |
    [csr-engine import]
         |
    +---------+     +----------+     +-------+
    | SQLite  | --> | FastEmbed| --> | HNSW  |
    | (chunks)|     | (384-dim)|     | (ANN) |
    +---------+     +----------+     +-------+
         |                               |
    [enrichment]                    [MCP search]
         |                               |
  Layer 1: Heuristic              reflect_on_past()
  Layer 2: V3 Extraction          search_by_concept()
  Layer 3: AI Narrative*          get_recent_work()
                                  ...12 tools total
```

Everything runs locally in a single process. No network services, no containers.

- **SQLite** stores chunks, embeddings, enrichment state
- **FastEmbed** (all-MiniLM-L6-v2) generates 384-dim vectors locally
- **HNSW** index provides sub-millisecond approximate nearest neighbor search
- **3-layer enrichment** progressively improves searchability

*\*Layer 3 (AI Narratives) is optional and requires an Anthropic API key.*

## MCP Tools

12 tools available to Claude when the MCP server is connected:

| Tool | Description |
|------|-------------|
| `csr_reflect_on_past` | Semantic search across past conversations |
| `store_reflection` | Store insights for future retrieval |
| `csr_quick_check` | Fast existence check (count + top match) |
| `search_by_recency` | Time-constrained search ("last week") |
| `get_recent_work` | "What did we work on?" with session grouping |
| `get_timeline` | Activity timeline with statistics |
| `csr_search_by_file` | Find conversations that touched a file |
| `csr_search_by_concept` | Theme-based search ("security", "testing") |
| `csr_search_insights` | Aggregated patterns from search results |
| `csr_get_more` | Paginate through additional results |
| `get_full_conversation` | Retrieve complete JSONL conversation |
| `get_session_learnings` | Iteration-level memory for Ralph loops |

## Hooks

6 Claude Code hooks for real-time intelligence:

| Hook | What it does |
|------|-------------|
| **SessionStart** | Surfaces relevant past context at conversation start |
| **SessionEnd** | Stores session narrative for future retrieval |
| **PreCompact** | Backs up state before context compaction |
| **Stop** | Stores iteration learnings, detects stuck patterns |
| **PostToolUse** | Tracks file edits with session-scoped dedup |
| **UserPromptSubmit** | Predicts and injects relevant context |

## Ralph Loop Memory

Use the [ralph-wiggum plugin](https://github.com/anthropics/claude-code-plugins/tree/main/ralph-wiggum) for long tasks? CSR gives your Ralph loops **persistent memory across sessions and projects**.

- **Automatic backup** before context compaction
- **Anti-pattern injection** — "DON'T RETRY THESE" surfaces first
- **Success pattern learning** — reuse what worked before
- **Cross-project memory** — learn from ALL your projects

```bash
# Install hooks (run once)
csr-engine hook install --apply
```

## AI Narratives (Optional)

Transform raw conversations into rich, searchable narratives with 9.3x better search quality. Requires an Anthropic API key.

```bash
# Setup with API key
csr-engine setup --anthropic-key=sk-ant-...

# Run the enrichment daemon
csr-engine daemon
```

| Metric | Without | With AI Narratives |
|--------|---------|-------------------|
| Search quality | 0.074 | 0.691 (9.3x) |
| Token compression | 100% | 18% (82% reduction) |
| Cost per conversation | - | ~$0.012 (Batch API) |

## CLI Reference

```
csr-engine                     Start MCP server (default)
csr-engine setup               One-shot setup: import + MCP + hooks
csr-engine status              System status (JSON)
csr-engine status --compact    One-line statusline output
csr-engine daemon              Background enrichment daemon
csr-engine hook install --apply Install Claude Code hooks
csr-engine eval                Quick eval (5 tests)
csr-engine eval --full         Full eval (20 tests)
csr-engine quality <file>      AST-based code quality analysis
csr-engine --import            Import conversations
csr-engine --enrich            Backfill enrichment pipeline
csr-engine --watch             Watch for new conversations
```

## Requirements

- **macOS** (Apple Silicon) or **Linux** (x86_64)
- **Claude Code** CLI
- ~50MB disk for the binary + ~100MB for database

No Docker. No Python. No API keys (unless you want AI narratives).

## Upgrading from v7.x

v8.0 replaces the Python/Docker stack with a single Rust binary.

```bash
# 1. Stop old services
docker compose down 2>/dev/null
claude mcp remove claude-self-reflect 2>/dev/null

# 2. Install the new binary
curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh

# 3. Run setup (imports existing conversations)
csr-engine setup

# 4. Restart Claude Code
```

Your conversation data (`~/.claude/projects/`) is untouched. The new engine re-imports from the same JSONL files.

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| No search results | Run `csr-engine --import --enrich` |
| MCP tools not available | Run `csr-engine setup`, restart Claude Code |
| "spawn ENOENT" in MCP | Ensure `csr-engine` is in PATH |
| Status shows 0 conversations | Run `csr-engine status` to check import progress |
| Slow first startup | Normal (~14s for index rebuild, subsequent: ~93ms) |

<details>
<summary>Uninstall</summary>

```bash
claude mcp remove claude-self-reflect
rm -rf ~/.claude-self-reflect/
rm ~/.local/bin/csr-engine
npm uninstall -g claude-self-reflect  # if installed via npm
```

</details>

## Contributors

- **[@TheGordon](https://github.com/TheGordon)** - Fixed timestamp parsing (#10)
- **[@akamalov](https://github.com/akamalov)** - Ubuntu WSL insights
- **[@kylesnowschwartz](https://github.com/kylesnowschwartz)** - Security review (#6)

---

Built with care by [ramakay](https://github.com/ramakay) for the Claude community.
