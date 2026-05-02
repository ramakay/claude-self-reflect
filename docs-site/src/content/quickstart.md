---
title: Quick Start
---

## Your First Search

Open Claude Code and ask:

> "What did we work on recently?"

Claude uses `get_recent_work` to show recent sessions grouped by project.

## Search Past Conversations

> "Have we dealt with Docker memory issues before?"

Claude uses `csr_reflect_on_past` for semantic search across all history.

## Search by File

> "What conversations involved docker-compose.yaml?"

## Explore Concepts

> "What do we know about authentication patterns across projects?"

## Store an Insight

> "Remember: we use JWT tokens with 15-minute expiry for the API gateway."

## What Happens Automatically

You don't need to do anything for these:

- **Session start**: Past context injected before you ask
- **Every prompt**: Context predicted and provided proactively
- **Session end**: Session summarized and indexed
- **Context compaction**: State backed up before trim
- **File edits**: Tracked for future file-based search

## Run Evaluation

```bash
csr-engine eval        # Quick (5 tests, <1s)
csr-engine eval --full # Full (20 tests)
```
