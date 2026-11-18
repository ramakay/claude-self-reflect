---
title: Configuration
---

## MCP Registration

Automatic via `csr-engine setup`. Manual:
```bash
claude mcp add claude-self-reflect "csr-engine" -s user
```

## Hooks

Configured in `~/.claude/settings.json` via `csr-engine hook install --apply`.

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| CSR_NARRATIVE_MODEL | — | Override the model for the briefing, session narratives and stories, `dream backfill` and `dream --report` (default `haiku`, then the CLI default). The optional Batch API narrator also reads it, with a default of `claude-haiku-4-5-20251001` |
| CSR_RATIFICATION_MODEL | — | Override the model for the daemon's ratification pass (default `haiku`, then the CLI default) |
| CSR_DREAM_THREAD_MODEL | — | Override the model for dream threads, the plan pass and `csr-engine dreams`. Without it, the `CSR_DREAM_EFFORT` tier picks the model |
| CSR_DREAM_EFFORT | balanced | `less`, `balanced` or `max`. Picks the dream model: `haiku-4`, `sonnet-5` (default) or `opus-5`. `CSR_DREAM_THREAD_MODEL` wins over it |
| CSR_NO_AI_NARRATIVES | — | Set to `1` to disable AI narrative generation entirely |
| CSR_STORY_REWRITES | — | Opt-in. Set to `1` and Haiku writes each session's story: after every compaction, and at session end if you asked something since the last story. Each is one metered Haiku call (`claude -p`, about $0.003). Unset, the story after each compaction is CSR's free extract |
| CSR_TIMING_LOG | ~/.claude-self-reflect/hook-timing.log | Custom hook-timing log path (test runs redirect to a temp file automatically) |
| CSR_NO_TIMING_DETAIL | — | Set to `1` to turn off the prompt detail lines in hook-timing.log; timing lines stay on |
| CSR_NO_PROMPT_GATE | — | Set to `1` to turn off the prompt hook's send gate. With the gate on, a search-based block goes out only when its best ranked score (the `top=` value in hook-timing.log) is 0.45 or more and 3 or more past-conversation items fit in it. Otherwise the hook sends nothing and logs `reason=below_floor` or `reason=thin_pool`. The daemon must run with the same value as the hook: when they differ, the daemon does not answer the prompt |

### Code map

The code map reads your repos and stores the map in the local database. `CSR_NO_CODEMAP=1` turns off the daemon job, the file watcher, the hook enqueue, the `csr-engine codemap` commands and the two MCP sections. These tune it.

| Variable | Default | Description |
|----------|---------|-------------|
| CSR_NO_CODEMAP | — | Set to `1` (or `true`) to turn the code map off |
| CSR_CODEMAP_REPOS | — | Repos to map besides the ones your sessions are in: paths separated by `:` |
| CSR_CODEMAP_MAX_REPOS | 16 | Most repos the daemon polls and watches. Named repos come first, then the newest session signal |
| CSR_CODEMAP_ACTIVE_HOURS | 24 | How long after its last session signal a repo stays polled and watched. An idle repo keeps its map |
| CSR_CODEMAP_POLL_SECS | 60 | Seconds between polls of each active repo |
| CSR_CODEMAP_NO_WATCH | — | Set to `1` (or `true`) to turn off the file watcher and keep the poll and the hook queue |
| CSR_CODEMAP_LOCK_WAIT_SECS | 30 | Seconds a scan waits for another scan of the same repo before it gives up |

The briefing, story, ratification and dream calls run through the Claude Code CLI (`claude -p`), which signs in the way Claude Code does on your machine. `ANTHROPIC_API_KEY` is read directly only by the optional Batch API narrator, in the daemon or in the MCP server. A `claude -p` call also inherits a key that is in its environment.

## Database

The database is `~/.claude-self-reflect/csr-engine.db`. To use another file, pass the global flag `--db-path <path>` to any `csr-engine` command. There is no environment variable for it. `--projects-dir <path>` does the same for the Claude projects directory.

### Backup
```bash
cp ~/.claude-self-reflect/csr-engine.db ~/.claude-self-reflect/backup.db
```

### Reset
```bash
rm ~/.claude-self-reflect/csr-engine.db
rm -rf ~/.claude-self-reflect/index-v3/
csr-engine setup
```

Source data (`~/.claude/projects/`) is never modified.

## Disk Usage

| Conversations | DB Size | Index |
|---------------|---------|-------|
| 100 | ~10 MB | ~5 MB |
| 500 | ~50 MB | ~20 MB |
| 1,000 | ~100 MB | ~35 MB |
