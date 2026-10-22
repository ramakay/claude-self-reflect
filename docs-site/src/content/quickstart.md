---
title: Quick Start
---

> Everything below assumes CSR is activated — you've run `csr-engine setup` (`claude-self-reflect setup` for an npm install) and restarted Claude Code. Setup also installed the csr-companion plugin unless you passed `--no-companion`; if you did, run `csr-engine companion plugin install`. Installing alone doesn't activate anything; see [Installation](#/docs/installation).

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

- **Session start**: one recap line on where you left off, and one learnt line on what CSR stored for your last session
- **File mentions**: name a file in your prompt, or write `@path`, and its edit history comes along
- **Worktrees and subfolders**: start Claude in either and you get the repo's history, not an empty memory
- **Subagent runs**: imported and searchable like any other session
- **After compaction**: the recap names the task you were on
- **Resume**: after `claude -c` or a resume, CSR names the session you continued

CSR also maps the repo you work in, so it can answer where a function lives even if no conversation touched it.

## Run Evaluation

```bash
csr-engine eval        # Quick (6 checks, ~20ms after the index loads)
csr-engine eval --full # Full (24 results)
```
