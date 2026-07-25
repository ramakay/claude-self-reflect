---
title: Privacy & Security
---

## Local by Default

| Component | Cloud? |
|-----------|--------|
| Conversation source | No — local JSONL |
| Embeddings | No — FastEmbed |
| Vector search | No — HNSW in-memory |
| Storage | No — SQLite |
| MCP server | No — stdio |

**Zero network connections** by default. No telemetry.

## Consent-Gated Activation

Installing CSR does not activate it. Nothing is written to `~/.claude/settings.json`, no MCP server is registered, and no conversations are indexed until you explicitly run `csr-engine setup` (or approve the installer's prompt). npm postinstall is download-only, so sandboxed evaluation (`npm install --prefix`) leaves your live Claude Code configuration untouched. See [Installation](#/docs/installation) for the `CSR_AUTO_SETUP` / `CSR_SKIP_SETUP` controls.

## Optional Cloud: AI Narratives

Only Layer 3 uses cloud API. Requires explicit opt-in:
1. Your own ANTHROPIC_API_KEY
2. Running `csr-engine daemon`

Cost: ~$0.012/conversation.

## Data

All at `~/.claude-self-reflect/`. Delete anytime:
```bash
rm -rf ~/.claude-self-reflect/
```

Source conversations never modified.

## Security

- No open ports (stdio only)
- No code execution from search results
- Read-only access to conversations
- Standard file permissions
