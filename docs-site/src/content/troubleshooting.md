---
title: Troubleshooting
---

## Installation

### "spawn ENOENT"
`csr-engine` not in PATH.
```bash
which csr-engine || curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc
```

### Intel Mac
No ONNX binaries. Use Linux VM, WSL2, or Apple Silicon.

### Windows paths
```bash
ln -s /mnt/c/Users/<windows-user>/.claude ~/.claude
```

## MCP

### Tools not available
```bash
claude mcp remove claude-self-reflect 2>/dev/null
csr-engine setup
# Restart Claude Code
```

## Search

### No results
```bash
csr-engine status    # Check counts
csr-engine --import  # Re-import
csr-engine eval      # Diagnostics
```

## Performance

### Slow first startup
Normal — rebuilding HNSW index. Later starts load the cached index; that takes longer for a larger index.

### Inspecting the DB with system sqlite3 (macOS)
macOS's bundled `sqlite3` can't load the FTS5 module, so it silently skips the `chunks_fts` table — integrity checks look ~10x faster than reality and FTS repairs won't work from the CLI. Use `csr-engine status --deep` for a true integrity check, or a Homebrew sqlite3.

## AI Narratives

### Unexpected charges
A charge can come from two places: the Batch API narrator, which calls the Anthropic API directly when `ANTHROPIC_API_KEY` is set in the environment of the daemon or of the MCP server, and any `claude -p` call, which is billed the way Claude Code is billed on your machine. A `claude -p` call bills an API key when one is in its environment or in a settings file that carries credentials. See [Privacy & Security](#/docs/privacy) for every caller and its switch.

Unsetting a variable in one shell does not reach a process that is already running. A running MCP server and a running daemon keep the key they started with, and a launchd daemon takes its environment from its plist, not from your shell. To stop the charges:

1. Remove `ANTHROPIC_API_KEY` from every place that supplies it: your shell profile, the `EnvironmentVariables` block of the daemon's plist if you run `scripts/launchd/com.ramakay.csr-engine.daemon.plist`, an `env` block for the MCP server in Claude Code's config, and an `env` block or `apiKeyHelper` in your Claude Code `settings.json`. If you passed `--anthropic-key` to `csr-engine setup`, also delete the line from `~/.claude-self-reflect/.env`.
2. Restart the daemon. For the launchd daemon:
```bash
launchctl kickstart -k gui/$(id -u)/com.ramakay.csr-engine.daemon
```
For a daemon you started by hand, stop it and start `csr-engine daemon` again from a shell that no longer has the key. To run a daemon with the Batch API narrator off whatever the environment holds, start it with `csr-engine daemon --no-ai`.
3. Restart Claude Code, so it starts its MCP servers again without the key.

`CSR_NO_AI_NARRATIVES=1` stops the briefing, the story calls, ratification and the dream commands. It does not stop the Batch API narrator. Companion Dreams is a separate switch: turn off Dreams in `/config`.

## Help

- `csr-engine eval --full` — 24 diagnostic tests
- [GitHub Issues](https://github.com/ramakay/claude-self-reflect/issues)
