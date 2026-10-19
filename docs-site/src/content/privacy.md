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

The index, search and storage run on your machine, and CSR sends no telemetry. A few features do use the network or a model. The section Network and model use, below, lists each one with its off switch.

## Consent-Gated Activation

Installing CSR does not activate it. Nothing is written to `~/.claude/settings.json`, no MCP server is registered, and no conversations are indexed until you explicitly run `csr-engine setup` (or approve the installer's prompt). npm postinstall is download-only, so sandboxed evaluation (`npm install --prefix`) leaves your live Claude Code configuration untouched. See [Installation](#/docs/installation) for the `CSR_AUTO_SETUP` / `CSR_SKIP_SETUP` controls.

## Code Map

The code map only reads your repos: it runs no git command that writes and refreshes no index. It is stored in the local SQLite database. The daemon never opens `~/Desktop`, `~/Documents`, `~/Downloads`, iCloud Drive or `/Volumes`, so the macOS privacy prompt never fires for it; a session working in one of those folders maps its own repo. `CSR_NO_CODEMAP=1` turns the code map off.

## Network and model use

- **Embedding model download.** The first run downloads the embedding model (AllMiniLM-L6-v2, about 87 MB on disk) into your user cache folder, `csr-engine/fastembed`. After that, embeddings run locally.
- **SessionStart briefing.** The hook that setup installs runs `claude -p` with your Claude Code login and no API key. It runs at most once per 30 minutes per project, and only when that project's episodes changed. `CSR_NO_AI_NARRATIVES=1` turns it off.
- **Daemon narratives (Layer 3).** These run through `claude -p` too, and only while `csr-engine daemon` runs. `CSR_NO_AI_NARRATIVES=1` turns them off.
- **Batch API narrator.** This runs only when `ANTHROPIC_API_KEY` is set in the environment of the daemon or of the MCP server. The MCP server runs these loops itself when no daemon is live. Cost: ~$0.012/conversation. Unset the key to stop it.
- **Companion Dreams.** Dreams is on by default once the companion plugin is installed. It forks the session's model for one "what if...?" look, budgeted to about 1 token for every 10 Claude spends. Turn it off with Dreams in `/config`.

`CSR_NO_AI_NARRATIVES=1` covers the briefing and the daemon narratives only. Dreams has its own switch, and the Batch API narrator stops when the key is unset. No single setting turns off all of these.

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
