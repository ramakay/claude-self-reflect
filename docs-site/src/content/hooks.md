---
title: Active Memory Hooks
---

## The Hook Architecture

Six hooks fire at specific moments in the session lifecycle. Each performs its work in milliseconds and either injects context or stores data.

This is what separates CSR from passive memory tools. **Context finds you.**

## Session Lifecycle

![Session lifecycle hook timeline — six hooks fire at key moments, injecting context and storing data](/claude-self-reflect/images/hooks-lifecycle-2.png)

## SessionStart — Past Context Injection

Fires when you start a conversation. Searches your entire history, surfaces relevant past work, checks for anti-patterns from incomplete sessions.

What Claude sees:
```
[4d ago] We need to release this next version... (102 msgs)
[1w ago] Please see sessions-handoff.md... (34 msgs)
[2w ago] what did we discuss last session (111 msgs)

For deeper context, use csr_reflect_on_past("topic").
```

## UserPromptSubmit — Predictive Injection

Fires every prompt. Multi-signal scoring:

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
