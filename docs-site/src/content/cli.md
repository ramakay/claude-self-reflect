---
title: CLI Reference
---

## Commands

### Default (no args)
Starts MCP server (stdio mode).

### setup
One-shot: import + MCP registration + hook installation.
```bash
csr-engine setup
```

### status
```bash
csr-engine status           # Full JSON
csr-engine status --compact  # One-line for statusbar
```

### hook
```bash
csr-engine hook install --apply  # Install all hooks
csr-engine hook session-start    # Run specific hook (called by Claude Code)
```

### daemon
Background enrichment for Layer 3 AI narratives.
```bash
csr-engine daemon --batch-size 10 --no-ai
```

### eval
```bash
csr-engine eval        # Quick (5 tests)
csr-engine eval --full # Full (20 tests, 6 categories)
```

### quality
AST-based code quality analysis.
```bash
csr-engine quality src/main.rs
```

## Flags

| Flag | Description |
|------|-------------|
| `--import` | Import conversations |
| `--enrich` | Backfill enrichment |
| `--watch` | Watch for new conversations |
| `--version` | Print version |

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| ANTHROPIC_API_KEY | (none) | Required for Layer 3 |
| CSR_DB_PATH | ~/.claude-self-reflect/csr-engine.db | DB location |

## Data Locations

| Path | Contents |
|------|----------|
| ~/.claude-self-reflect/csr-engine.db | SQLite database |
| ~/.claude-self-reflect/index/ | HNSW cache |
| ~/.claude-self-reflect/hook-timing.log | Hook performance |
| ~/.local/bin/csr-engine | Binary |
