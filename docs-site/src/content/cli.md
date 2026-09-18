---
title: CLI Reference
---

## Commands

### Default (no args)
Starts MCP server (stdio mode).

### --serve-http
Serves MCP over Streamable HTTP from one process instead of one stdio process
per session, so a single loaded index answers every session. Register the
endpoint with `claude mcp add --transport http claude-self-reflect
http://127.0.0.1:7391/mcp -s user` in place of the stdio entry.
```bash
csr-engine --serve-http 127.0.0.1:7391
```
Loopback only unless `CSR_SERVE_HTTP_ALLOW_NON_LOOPBACK=1` is set: the endpoint
is unauthenticated.

### setup
One-shot: import + MCP registration + hook installation.
```bash
csr-engine setup
```

### status
```bash
csr-engine status           # Full JSON (incl. "narratives" block: AI calls/tokens today + total)
csr-engine status --compact  # One-line for statusbar, e.g. "AI 3c/12.4k tok today" or "AI off"
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
csr-engine daemon --serve-http 127.0.0.1:7391  # also host the MCP endpoint
```
With `--serve-http` the daemon hosts the MCP endpoint itself, sharing the
storage, embeddings and index it has already loaded.

### eval
```bash
csr-engine eval                   # Quick (5 tests)
csr-engine eval --full            # Full (20 tests, 6 categories)
csr-engine eval --continuity-live # Live continuity probe vs the real index
```

### telemetry
Ops dashboard: hook latency percentiles, startup timings, enrichment health.
```bash
csr-engine telemetry              # Text report
csr-engine telemetry --since 7d   # Window: 30m|24h|7d|all
csr-engine telemetry --json       # Machine-readable
csr-engine telemetry --tui        # Live ratatui dashboard
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
| `--serve-http <ADDR>` | Serve MCP over Streamable HTTP instead of stdio |
| `--version` | Print version |

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| CSR_NARRATIVE_MODEL | (none) | Override AI narrative model (chain: this → `haiku` → CLI default) |
| CSR_NO_AI_NARRATIVES | (none) | Set to `1` to disable AI narratives |
| CSR_DB_PATH | ~/.claude-self-reflect/csr-engine.db | DB location |
| CSR_SERVE_HTTP_ALLOW_NON_LOOPBACK | (none) | Set to `1` to let `--serve-http` bind a non-loopback address |

## Data Locations

| Path | Contents |
|------|----------|
| ~/.claude-self-reflect/csr-engine.db | SQLite database |
| ~/.claude-self-reflect/index/ | HNSW cache |
| ~/.claude-self-reflect/hook-timing.log | Hook performance |
| ~/.local/bin/csr-engine | Binary |
