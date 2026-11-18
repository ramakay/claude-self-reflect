---
title: Why Self-Reflect?
---

## Claude looking back at its own work

Claude Code starts every session cold. What it decided last week, why a function looks the way it does, which fix was tried and dropped: none of that is in front of it unless something puts it there.

Self-Reflect lets Claude reflect on its own work. It reads the record of past sessions and answers questions such as:

- What did we settle, and what is still open?
- Why does this code exist, and which session shaped it?
- Did a commit make an earlier conclusion obsolete?

To do that, Self-Reflect keeps a local index of your sessions. That index is infrastructure. The point is what Claude can do with it. See [Features](#/docs/features) for the full list.

## Single binary

`csr-engine` is one Rust binary. SQLite, HNSW search, FastEmbed embeddings, the MCP server and all 8 hooks are compiled into one executable. Install is `curl | sh`, and it asks before it activates anything. There is no Python, no Docker and no vector database. See [Installation](#/docs/installation).

## Context arrives without being asked

Self-Reflect hooks into seven moments of a session. Two of them inject context (SessionStart and UserPromptSubmit). The others capture and maintain it:

- **SessionStart**: shows where you left off. That is a recap when the last session left evidence (else the `CSR CONTINUUM` and `LAST` lines), a learnt line when Self-Reflect stored notes, and a compact receipt after a compact.
- **UserPromptSubmit**: predicts the context your prompt needs from several signals: semantic similarity, recency, file overlap and error patterns.
- **SessionEnd**: writes a searchable narrative. No manual tagging.
- **Stop**: imports the transcript, keeps a rolling summary and stores a structured episode.
- **PreCompact**: refreshes the recap anchor and starts one dream cycle.
- **PostCompact**: stores Claude Code's own compaction summary once, as derived context.
- **PostToolUse**: tracks file edits, with session-scoped deduplication.

## Progressive enrichment

A 3-layer pipeline improves what Claude can find:

1. **Layer 1: heuristic** (instant, free). Extracts key patterns at import time.
2. **Layer 2: V3 extraction** (inline, free). Structured extraction at session end.
3. **Layer 3: AI narrative** (optional, metered). Full narratives written through your own Claude Code login.

## Every project, one index

All your Claude Code projects are indexed automatically. A solution from one project can surface in another. There is no per-project configuration.

## Privacy

Search, storage and embeddings run on your machine. There are no accounts and no telemetry. The first run downloads the embedding model. A few features call a model through your own Claude Code login, and each has an off switch. See [Privacy & Security](#/docs/privacy).

## Consent first

Install and activation are separate steps. Installing (via `curl | sh` or npm) only downloads the SHA256-verified binary. Nothing touches `~/.claude/settings.json`, no MCP server is registered, and no conversations are indexed until you approve `csr-engine setup`. Interactive installs prompt. Non-interactive installs never activate on their own. See [Installation](#/docs/installation) for the `CSR_AUTO_SETUP` and `CSR_SKIP_SETUP` controls.

## Known gaps

We publish what is not working yet, with numbers. See [What we're working on](#/docs/report-card).
