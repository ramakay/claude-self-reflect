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
- **SessionStart briefing.** The SessionStart hook runs `claude -p`. It runs at most once per 30 minutes per project, and only when that project's episodes changed. `CSR_NO_AI_NARRATIVES=1` turns it off.
- **Session stories.** At SessionEnd, a session with enough messages gets a story. CSR builds it locally when it can; otherwise the hook starts a detached `csr-engine generate-story` process that runs `claude -p`. This needs no daemon. After a compaction the story is extracted from Claude Code's own summary with no model call. With `CSR_STORY_REWRITES=1` it is written by `claude -p` instead, after every compaction, and again at SessionEnd when you asked something since. `CSR_NO_AI_NARRATIVES=1` turns off every story call.
- **Ratification (daemon).** While `csr-engine daemon` runs, it runs one `claude -p` extraction per conversation. `CSR_NO_RATIFICATION=1` or `CSR_NO_AI_NARRATIVES=1` turns it off.
- **Dream threads (daemon).** Off by default. `CSR_DREAM_THREADS=1` turns on a `claude -p` extraction that the daemon's dream cycle runs, and the plan pass that follows it. `CSR_NO_AI_NARRATIVES=1` or `CSR_NO_DREAMING=1` turns them off.
- **Commands you run.** `csr-engine dreams` (strategy dreams), `csr-engine dream backfill` (one `claude -p` call per candidate, capped by `--budget-calls`; `--dry-run` makes none) and `csr-engine dream --report` (headline sentences in the journal) call `claude -p` when you run them. Nothing starts them in the background. `CSR_NO_AI_NARRATIVES=1` stops all three.
- **Batch API narrator.** This is the only caller that uses the Anthropic API directly instead of `claude -p`. It runs only when `ANTHROPIC_API_KEY` is set in the environment of the daemon or of the MCP server. The MCP server runs it itself when no daemon is live. Cost: ~$0.012/conversation. `CSR_NO_AI_NARRATIVES=1` does not stop it. `csr-engine daemon --no-ai` stops it in the daemon, and removing the key stops it in both.
- **Companion Dreams.** Dreams is on by default once the companion plugin is installed. It forks the session's model for one "what if...?" look, budgeted to about 1 token for every 10 Claude spends. Turn it off with Dreams in `/config`.

Every `claude -p` call above runs as a child process that inherits the environment of the process that starts it, and it signs in the way Claude Code does on that machine. For most of these calls CSR skips your Claude Code settings files, unless a settings file carries credentials (`apiKeyHelper`, or an `env` block with `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, or Bedrock or Vertex settings); then it loads them, and the call is billed to the account those credentials belong to. Dream threads and strategy dreams never load your settings files. A call with no key in its environment and no such setting uses your Claude Code login. A call inherits an `ANTHROPIC_API_KEY` that is in its environment, so Claude Code can bill the call to that key.

`CSR_NO_AI_NARRATIVES=1` covers the briefing, every story call, ratification, dream threads, and the three commands above. It does not cover the Batch API narrator or companion Dreams. No single setting turns off all of these.

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
