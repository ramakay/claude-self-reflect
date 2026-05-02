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
| ANTHROPIC_API_KEY | — | For Layer 3 AI narratives |
| CSR_DB_PATH | ~/.claude-self-reflect/csr-engine.db | Custom DB path |

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
