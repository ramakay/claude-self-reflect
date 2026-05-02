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

### Slow first startup (~14s)
Normal — rebuilding HNSW index. Subsequent: ~93ms.

## AI Narratives

### Unexpected charges
```bash
pkill -f "csr-engine daemon"
unset ANTHROPIC_API_KEY
```

## Help

- `csr-engine eval --full` — 20 diagnostic tests
- [GitHub Issues](https://github.com/ramakay/claude-self-reflect/issues)
