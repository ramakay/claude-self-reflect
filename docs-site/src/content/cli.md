---
title: CLI Reference
---

## Commands

### Default (no args)
Starts MCP server (stdio mode).

### setup
One-shot: import + MCP registration + hook installation + the csr-companion plugin (installed through Claude Code's plugin CLI).
```bash
csr-engine setup
csr-engine setup --no-companion   # skip the plugin
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
```

### eval
```bash
csr-engine eval                   # Quick (5 tests)
csr-engine eval --full            # Full (24 results)
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

### codemap
Live code map: a per-worktree map of every definition in your repos, so CSR can answer for code no conversation touched. Off with `CSR_NO_CODEMAP=1`.
```bash
csr-engine codemap add <path>             # Register a repo (any path inside it) and list its worktrees
csr-engine codemap remove <path>          # Stop mapping a repo: drops its views, keeps discovery from re-adding it
csr-engine codemap scan --repo <path>     # Map one repo's worktrees
csr-engine codemap scan --all             # Every registered repo plus the ones discovery finds
csr-engine codemap status [--json]        # Per repo and worktree: branch, head, files, dirty, untracked, age; totals
csr-engine codemap query <text> [--json] [--limit 20]   # Find a definition by name, signature or path
csr-engine codemap watch [--for <secs>]   # Run the daemon's freshness loop in the foreground and log each action
```
`query` shows which worktrees hold the same blob. `status` also lists the repos the map skips and why (`protected-folder: mapped by session only`, `idle > <N>h`, `cap <N>` or `CSR_NO_CODEMAP`).

`scan --session` and `drain` are what the hooks start for repos in macOS-protected folders; you rarely run them by hand. `scan --session` lets one scan enter the protected folder `--repo` is in, and `drain --repo <path>` rescans the queued edits of that repo and exits. The daemon never passes `--session`.

### quality
AST-based code quality analysis.
```bash
csr-engine quality src/main.rs
```

## Flags

| Flag | Description |
|------|-------------|
| `--db-path <path>` | SQLite database file (global; default `~/.claude-self-reflect/csr-engine.db`) |
| `--projects-dir <path>` | Claude projects directory (global) |
| `--import` | Import conversations |
| `--enrich` | Backfill enrichment |
| `--watch` | Watch for new conversations |
| `--version` | Print version |

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| CSR_NARRATIVE_MODEL | (none) | Override AI narrative model (chain: this → `haiku` → CLI default) |
| CSR_NO_AI_NARRATIVES | (none) | Set to `1` to disable AI narratives |
| CSR_NO_CODEMAP | (none) | Set to `1` to turn the code map off. See [Configuration](#/docs/configuration) for the other `CSR_CODEMAP_*` variables |

## Data Locations

| Path | Contents |
|------|----------|
| ~/.claude-self-reflect/csr-engine.db | SQLite database |
| ~/.claude-self-reflect/index-v3/ | HNSW cache |
| ~/.claude-self-reflect/hook-timing.log | Hook performance |
| ~/.local/bin/csr-engine | Binary |
