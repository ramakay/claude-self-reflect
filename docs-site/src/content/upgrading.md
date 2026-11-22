---
title: Upgrading
---

## Upgrade from 9.5.8

9.5.8 is the last 9.x release on npm.

- **npm:** run `npm install -g claude-self-reflect`, then `claude-self-reflect setup`.
- **curl installer:** run the install script and answer yes at "Run setup now?". Where it cannot prompt, it prints the `csr-engine setup` command to run.

Then restart Claude Code. 9.5.8 setup installed no companion plugin; this setup adds csr-companion. Inside herdr it opens a pane beside Claude; without herdr, type `/csr` to open its panel. See [Choose your view](#/docs/companion-views). Setup repoints the MCP registration at the new binary and adds the hooks 10.1.0 needs. Your data is not touched. The first daemon start rebuilds the vector index once, and MCP tools answer keyword-only until it publishes. Then check `csr-engine status` as described in "Upgrading within 10.x" below.

## What Changed in v8


v8.0 replaced the Python/Docker/Qdrant stack with a single Rust binary.

| | v7.x (retired) | v8.0 |
|--|------|------|
| Runtime | Python | Rust binary |
| Vector DB | Qdrant (Docker) | HNSW (in-process) |
| Embeddings | Voyage AI (cloud) | FastEmbed (local) |
| Dependencies | Docker, Python, npm | None |
| Install | 5+ steps | One command |

## Upgrade from v7.x

```bash
# Remove old services
docker compose down 2>/dev/null
claude mcp remove claude-self-reflect 2>/dev/null

# Install v8 — the installer asks before running setup
curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh

# If you declined (or installed non-interactively), activate explicitly:
csr-engine setup

# Restart Claude Code
```

Your JSONL conversation files are untouched. v8 re-imports from the same source.

## Upgrading within 10.x

A new binary does not change what Claude Code or launchd already hold. After you install it (the install script, `npm install -g claude-self-reflect`, or a copy of the binary), do these steps:

1. Run setup again: `claude-self-reflect setup` if you installed with npm, `csr-engine setup` otherwise. It is safe to repeat. It registers hooks a newer build added (for example `PostCompact`) and rewrites the launch agent when its settings changed (for example `ProcessType Interactive`). It tells you what it changed.
2. Run `csr-engine status`. Two fields tell you whether the upgrade is complete:
   - `hooks_missing` lists hook events the build installs that `~/.claude/settings.json` lacks. `degraded` repeats the cause in words. `csr-engine hook install --apply` fixes just the hooks.
   - `stale_processes` lists every live process (the daemon, an MCP server, a companion pane) that still runs the older binary, and `mcp_binary_stale` turns true.
3. Restart Claude Code sessions that were open during the upgrade. An open session keeps its old MCP server until it restarts. A dormant pane shows up in `stale_processes` until you restart it.

With npm, use `claude-self-reflect setup` for step 1, not `csr-engine setup`. npm 11.20 and later do not run a package's install script until you allow it, so `npm install -g claude-self-reflect` alone can leave the old engine in place. From 10.1.1, `claude-self-reflect setup` compares the engine with the package version and downloads the new engine before it runs setup. If the download fails, it prints the reason and the command to run, and runs the engine that is there. `claude-self-reflect status` does not download; when the engine is older than the package, it says so in one line. `csr-engine --version` shows which engine you have.

The daemon restarts on its own when its binary is replaced. Your data in `~/.claude-self-reflect/` and `~/.claude/projects/` is not touched.

## Coming from Another Tool

Install CSR alongside your current tool. Test both. CSR indexes the same `~/.claude/projects/**/*.jsonl` files — no migration needed.

## Rollback to v7.x

> **Note**: v7.x required Docker and Qdrant. Only roll back if you still have that infrastructure.

```bash
rm ~/.local/bin/csr-engine && rm -rf ~/.claude-self-reflect/
# Restart your v7.x Docker services manually
```
