# Claude Self-Reflect

<div align="center">

<img src="docs-site/public/favicon.svg" alt="Claude Self-Reflect" width="80" height="80" />

[![npm version](https://badge.fury.io/js/claude-self-reflect.svg)](https://www.npmjs.com/package/claude-self-reflect)
[![npm downloads](https://img.shields.io/npm/dm/claude-self-reflect.svg)](https://www.npmjs.com/package/claude-self-reflect)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![GitHub CI](https://github.com/ramakay/claude-self-reflect/actions/workflows/ci.yml/badge.svg)](https://github.com/ramakay/claude-self-reflect/actions/workflows/ci.yml)

[![Claude Code](https://img.shields.io/badge/Claude%20Code-Compatible-6B4FBB)](https://github.com/anthropics/claude-code)
[![MCP Protocol](https://img.shields.io/badge/MCP-Enabled-FF6B6B)](https://modelcontextprotocol.io/)
[![Local First](https://img.shields.io/badge/Local%20First-Privacy-4A90E2)](https://github.com/ramakay/claude-self-reflect)

[![GitHub stars](https://img.shields.io/github/stars/ramakay/claude-self-reflect.svg?style=social)](https://github.com/ramakay/claude-self-reflect/stargazers)
[![GitHub issues](https://img.shields.io/github/issues/ramakay/claude-self-reflect.svg)](https://github.com/ramakay/claude-self-reflect/issues)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](https://github.com/ramakay/claude-self-reflect/pulls)

**Claude forgets everything. This fixes that.**

Single 44MB binary. No databases. No containers. No API keys required.

[Install](#install) | [How It Works](#how-it-works) | [MCP Tools](#mcp-tools) | [FAQ](https://ramakay.github.io/claude-self-reflect/#/docs/troubleshooting)

</div>

### The Forgetting Problem

Claude starts fresh every session. Solutions you found, architectures you designed, bugs you debugged — all gone. Context retention drops below **20% after 10 sessions**.

<a href="https://ramakay.github.io/claude-self-reflect/">
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs-site/public/images/card-01-hook-dark.png" />
  <img src="docs-site/public/images/card-01-hook-light.png" alt="The Forgetting Problem" width="720" />
</picture>
</a>

### One Binary. 44MB.

Everything runs locally — SQLite, FastEmbed vectors (384-dim), HNSW search (<1ms), AST analysis across 6 languages. No Docker, no database, no API keys. **6 hooks** across the session lifecycle. **12 MCP tools** for search.

<a href="https://ramakay.github.io/claude-self-reflect/#/docs/architecture">
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs-site/public/images/card-02-arch-dark.png" />
  <img src="docs-site/public/images/card-02-arch-light.png" alt="Architecture — One Binary, 44MB" width="720" />
</picture>
</a>

### The Pipeline

Three layers progressively improve search quality — **9.3x improvement**. Quality scores: **0.074 → 0.345 → 0.691**. Higher quality context. Better decisions. Fewer tokens.

<a href="https://ramakay.github.io/claude-self-reflect/#/docs/enrichment">
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs-site/public/images/card-03-pipeline-dark.png" />
  <img src="docs-site/public/images/card-03-pipeline-light.png" alt="The Pipeline — 3 layers, 9.3x improvement" width="720" />
</picture>
</a>

> **[Explore the full documentation →](https://ramakay.github.io/claude-self-reflect/)**

## Table of Contents

- [The Problem](#the-forgetting-problem) — Why Claude needs memory
- [The Architecture](#one-binary-44mb) — How CSR solves it
- [The Pipeline](#the-pipeline) — Progressive enrichment (9.3x improvement)
- [Install](#install) — One command setup
- [What You'll Ask](#what-youll-ask) — Natural language, no syntax
- [Performance](#performance) — Sub-millisecond search, 93ms startup
- [MCP Tools](#mcp-tools) — 12 search tools
- [Hooks](#hooks) — 6 session lifecycle hooks
- [AI Narratives](#ai-narratives-optional) — Optional 9.3x quality boost
- [CLI Reference](#cli-reference)
- [Upgrading from v7.x](#upgrading-from-v7x)
- [Troubleshooting](#troubleshooting)
- [Contributors](#contributors)

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh
```

One command. Downloads the binary, runs setup, registers MCP server, installs 6 hooks. Restart Claude Code.

| Platform | Support |
|----------|---------|
| macOS (Apple Silicon) | Prebuilt binary |
| Linux x86_64 / WSL | Prebuilt binary |
| Linux ARM64 | Prebuilt binary |
| macOS (Intel) | Build from source |

<details>
<summary>Alternative: npm</summary>

```bash
npm install -g claude-self-reflect
```

</details>

<details>
<summary>Build from source</summary>

```bash
git clone https://github.com/ramakay/claude-self-reflect.git
cd claude-self-reflect/csr-engine
cargo build --release
cp target/release/csr-engine ~/.local/bin/
csr-engine setup
```

</details>

## What You'll Ask

After install, just ask Claude naturally:

- *"How did we solve re-renders on this component?"*
- *"What did we tell Joe about that commit?"*
- *"What were our frustrations with this approach?"*
- *"Where did we put the auth middleware config?"*

No special syntax. No commands. CSR finds relevant past context and injects it automatically.

## How It Works

Everything runs locally in a single process. No network services, no containers.

- **SQLite** stores chunks, embeddings, enrichment state
- **FastEmbed** (all-MiniLM-L6-v2) generates 384-dim vectors locally
- **HNSW** index provides sub-millisecond approximate nearest neighbor search
- **AST analysis** extracts functions, types, imports from code (Rust, Python, TS, JS, Go, TSX)
- **3-layer enrichment** progressively improves search quality from 0.074 to 0.691

*\*Layer 3 (AI Narratives) is optional and requires an Anthropic API key.*

## Performance

| Metric | Value |
|--------|-------|
| **Cached startup** | 93ms |
| **Search latency (p95)** | <1ms |
| **Binary size** | 44MB |
| **Import speed** | ~20 conversations/sec |
| **Embedding** | 0.73ms/text (batch) |

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

6 hooks fire at strategic moments during Claude Code sessions:

| Hook | What it does |
|------|-------------|
| **SessionStart** | Surfaces relevant past context at conversation start |
| **UserPromptSubmit** | Predicts and injects context before Claude responds |
| **PostToolUse** | Tracks file edits with session-scoped dedup |
| **Stop** | Stores iteration learnings, detects stuck patterns |
| **PreCompact** | Backs up state before context compaction |
| **SessionEnd** | Stores session narrative for future retrieval |

All hooks use catch-all error handling. They never block Claude Code.

## AI Narratives (Optional)

Transform raw conversations into rich, searchable narratives with 9.3x better search quality. Requires an Anthropic API key.

```bash
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
```

## Upgrading from v7.x

v8.0 replaces the Python/Docker stack with a single Rust binary.

```bash
# Stop old services
docker compose down 2>/dev/null
claude mcp remove claude-self-reflect 2>/dev/null

# Install v8
curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh
```

Your conversation data (`~/.claude/projects/`) is untouched. The new engine re-imports from the same JSONL files.

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| No search results | Run `csr-engine setup` |
| MCP tools not available | Run `csr-engine setup`, restart Claude Code |
| "spawn ENOENT" in MCP | Ensure `csr-engine` is in PATH |
| Slow first startup | Normal (~14s for index rebuild, subsequent: ~93ms) |

Full troubleshooting guide: [Documentation](https://ramakay.github.io/claude-self-reflect/#/docs/troubleshooting)

<details>
<summary>Uninstall</summary>

```bash
claude mcp remove claude-self-reflect
rm -rf ~/.claude-self-reflect/
rm ~/.local/bin/csr-engine
npm uninstall -g claude-self-reflect  # if installed via npm
```

</details>

<details>
<summary>Contributors (v1–v7)</summary>

- **[@TheGordon](https://github.com/TheGordon)** - Fixed timestamp parsing (#10)
- **[@akamalov](https://github.com/akamalov)** - Ubuntu WSL insights
- **[@kylesnowschwartz](https://github.com/kylesnowschwartz)** - Security review (#6)

</details>

---

[Documentation](https://ramakay.github.io/claude-self-reflect/) | [npm](https://www.npmjs.com/package/claude-self-reflect) | [Issues](https://github.com/ramakay/claude-self-reflect/issues) | MIT License
