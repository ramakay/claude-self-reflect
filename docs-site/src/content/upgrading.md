---
title: Upgrading to v8
---

## What Changed

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

# Install v8
curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh

# Restart Claude Code
```

Your JSONL conversation files are untouched. v8 re-imports from the same source.

## Coming from Another Tool

Install CSR alongside your current tool. Test both. CSR indexes the same `~/.claude/projects/**/*.jsonl` files — no migration needed.

## Rollback to v7.x

> **Note**: v7.x required Docker and Qdrant. Only roll back if you still have that infrastructure.

```bash
rm ~/.local/bin/csr-engine && rm -rf ~/.claude-self-reflect/
# Restart your v7.x Docker services manually
```
