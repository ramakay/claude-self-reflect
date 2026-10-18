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

Only Layer 3 sends text to a cloud model. It runs through `claude -p`, so it uses your existing Claude Code login and no API key. Running `csr-engine daemon` starts it. Set `CSR_NO_AI_NARRATIVES=1` to turn it off.

The daemon's separate Batch API narrator runs only if `ANTHROPIC_API_KEY` is set. Cost: ~$0.012/conversation.

## Data

All at `~/.claude-self-reflect/`. Delete anytime:
```bash
rm -rf ~/.claude-self-reflect/
```

Source conversations never modified.

## Security

- The MCP server speaks stdio and opens no port
- The dream journal (a web page) is served by the daemon on `127.0.0.1:7373` only. It has no setting to bind another interface. `CSR_JOURNAL_PORT` moves the port and `CSR_NO_JOURNAL_SERVER=1` turns it off
- No code execution from search results
- Read-only access to conversations
- Secrets are redacted before text is indexed or mirrored: API keys and tokens become `[secret redacted: <kind>]`. `CSR_NO_SECRET_SCRUB=1` turns this off
- Text wrapped in `<private>...</private>` is stripped before it is stored
- The transcript mirror (scrubbed, compressed copies of your transcripts) lives under `~/.claude-self-reflect/mirror/` with directories at `0700` and files at `0600`. `CSR_NO_MIRROR=1` turns it off
