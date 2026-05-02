# CSR SwiftBar Status Plugin — Design

> Date: 2026-04-25
> Approach: SwiftBar plugin (shell script, 30s refresh)

## Menu Bar Title (always visible)

Alternates every cycle between:
- `🧠 1079c 585r ✓` — counts + health
- `🧠 93ms ✓` — last hook latency

## Dropdown Sections

1. **Engine Status** — conversations, chunks, reflections, projects, DB size, import %, health
2. **Today's Focus** — read from `~/.claude-self-reflect/current-focus.txt` (written by session-start hook)
3. **Last Session** — read from `~/.claude-self-reflect/last-session-summary.txt` (written by session-end hook)
4. **Recent Hook Activity** — last 8 entries from `hook-timing.log`
5. **Search Quality** — last injection stats from timing log
6. **Quick Actions** �� Run Import, Run Eval, Backfill Stories, Open DB, View Log
7. **About** — version, branch, binary path

## Files

```
csr-engine/extras/swiftbar/
  csr-status.30s.sh     — the plugin (bash + jq)
  install.sh            — symlinks into SwiftBar plugin dir
```

## Hook Integration

- session-start hook: writes one-line focus to `current-focus.txt`
- session-end hook: writes 2-3 sentence summary to `last-session-summary.txt`

## Dependencies

- SwiftBar (brew install swiftbar)
- jq (brew install jq)
- csr-engine binary in PATH
