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
