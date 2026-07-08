---
title: Active Memory Hooks
---

## The Hook Architecture

Six hooks fire at specific moments in the session lifecycle. Each performs its work in milliseconds and either injects context or stores data.

This is what separates CSR from passive memory tools. **Context finds you.**

## Session Lifecycle

![Session lifecycle hook timeline — six hooks fire at key moments, injecting context and storing data](/claude-self-reflect/images/hooks-lifecycle-2.png)

## SessionStart — Past Context Injection

Fires when you start a conversation. Injects three things:

- **CONTINUUM** — last session's state: what was asked, where it ended (LAST), what's next (NEXT), and how many code anchors are still intact
- **EPISODE INDEX** — recent sessions as a pickup menu, newest first, each with its outcome and a one-call lookup
- **Anti-patterns** from incomplete past sessions

What Claude sees:
```
CSR CONTINUUM [2h ago]: fix the import chunking bug
LAST: Fix verified — coverage went from ~1% to full transcripts (outcome=success)
EPISODE INDEX — earlier threads, newest first:
- [1d ago] release prep v9.2 — gated on go (outcome=partial) → csr_reflect_on_past("conv_...")
- [3d ago] hook injection audit (outcome=success) → csr_reflect_on_past("conv_...")
```

## UserPromptSubmit — Predictive Injection

Fires every prompt. Routes first, then scores.

**Intent routing (v9.2)** — a semantic classifier (exemplar embeddings over the same local MiniLM model, no extra model shipped) detects two intents:

| Route | Intent | Threshold | Action |
|-------|--------|-----------|--------|
| A | Continue / StateRecall ("pick up where we left off", "what were we doing?") | 0.60 / 0.55 | Inject the matching episode's state directly |
| B | Topic correlation | recency-weighted, 7-day half-life | Match prompt to a past episode; surface it as a pickup pointer |

Everything else falls through to multi-signal scoring:

| Signal | Weight |
|--------|--------|
| Semantic similarity | 50% |
| Recency | 20% |
| File overlap | 20% |
| Error pattern match | 10% |

500-token budget. Skips trivial prompts and slash commands.

## SessionEnd — Automatic Narrative

Imports final transcript, runs V3 extraction, synthesizes session story locally (free), falls back to Haiku if needed.

Stored narrative example:
```
## Session Story: csr-engine hook system
Project: claude-self-reflect | 2 hours, 102 messages

### Key Decisions
- catch-all wrappers to never block Claude Code
- UTF-8 safe truncation with floor_char_boundary()
- Iteration dedup prevents 500 embeddings/session
```

## Stop — Stuck Detection

Surfaces anti-patterns from past sessions:
```
⚠️ Past anti-patterns detected:
- Mocking the database caused prod migration failures
- subprocess.run with shell=True caused injection vulnerabilities
```

## PreCompact — State Backup

Captures session state before context compaction. Preserves progress.

## PostToolUse — File Edit Tracking

Records files edited with deduplication. Enables `csr_search_by_file`.

## Error Handling

Every hook is wrapped in a catch-all. If anything fails, it logs and returns Ok — **never blocks Claude Code**.

## Install

```bash
csr-engine hook install --apply
```
