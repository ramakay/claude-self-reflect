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
| CSR_NARRATIVE_MODEL | — | Override model for AI narratives (falls back to `haiku`, then CLI default) |
| CSR_NO_AI_NARRATIVES | — | Set to `1` to disable AI narrative generation entirely |
| CSR_DB_PATH | ~/.claude-self-reflect/csr-engine.db | Custom DB path |
| CSR_TIMING_LOG | ~/.claude-self-reflect/hook-timing.log | Custom hook-timing log path (test runs redirect to a temp file automatically) |

AI narratives run through the Claude Code CLI (`claude -p`) — no API key required.

## Database

### Backup
```bash
cp ~/.claude-self-reflect/csr-engine.db ~/.claude-self-reflect/backup.db
```

### Reset
```bash
rm ~/.claude-self-reflect/csr-engine.db
rm -rf ~/.claude-self-reflect/index/
csr-engine setup
```

Source data (`~/.claude/projects/`) is never modified.

## Disk Usage

| Conversations | DB Size | Index |
|---------------|---------|-------|
| 100 | ~10 MB | ~5 MB |
| 500 | ~50 MB | ~20 MB |
| 1,000 | ~100 MB | ~35 MB |
