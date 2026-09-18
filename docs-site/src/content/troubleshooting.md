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

### HTTP endpoint not reachable
Registered as an `http` server, nothing starts `csr-engine` for you: the
process has to be listening before Claude Code connects.
```bash
curl -sS -X POST http://127.0.0.1:7391/mcp \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"curl","version":"0"}}}'
```
A `serverInfo` block naming `csr-engine` means the endpoint is up. Connection
refused means it is not: start `csr-engine --serve-http 127.0.0.1:7391` (or
`csr-engine daemon --serve-http 127.0.0.1:7391`) first.

## Search

### No results
```bash
csr-engine status    # Check counts
csr-engine --import  # Re-import
csr-engine eval      # Diagnostics
```

## Performance

### Slow first startup (~14s)
Normal — rebuilding HNSW index. Subsequent: ~150ms.

### Inspecting the DB with system sqlite3 (macOS)
macOS's bundled `sqlite3` can't load the FTS5 module, so it silently skips the `chunks_fts` table — integrity checks look ~10x faster than reality and FTS repairs won't work from the CLI. Use `csr-engine status --deep` for a true integrity check, or a Homebrew sqlite3.

## AI Narratives

### Unexpected charges
```bash
pkill -f "csr-engine daemon"
unset ANTHROPIC_API_KEY
```

## Help

- `csr-engine eval --full` — 20 diagnostic tests
- [GitHub Issues](https://github.com/ramakay/claude-self-reflect/issues)
