---
title: Installation
---

## Requirements

- **macOS** (Apple Silicon) or **Linux** (x86_64 / ARM64)
- **Claude Code** CLI installed
- ~50MB disk for binary, ~100MB for database

No Docker. No Python. No API keys.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh
```

Downloads the binary (SHA256-verified) and then **asks before activating**. Setup — which imports conversations, registers the MCP server, and installs all 6 hooks — only runs after you confirm at the prompt.

Non-interactive installs (CI, scripts) never activate automatically. Control the behavior with:

| Variable | Effect |
|----------|--------|
| `CSR_AUTO_SETUP=1` | Run setup without prompting |
| `CSR_SKIP_SETUP=1` | Download only, never run setup |

### npm

```bash
npm install -g claude-self-reflect
csr-engine setup
```

`npm install` only downloads the checksummed binary. It never modifies `~/.claude/settings.json`, registers MCP servers, or indexes conversations — activation is the separate, explicit `csr-engine setup` step (or set `CSR_AUTO_SETUP=1` during install to opt in).

## Verify

```bash
csr-engine status
```

Then restart Claude Code and try: "What did we work on recently?"

## Platform Support

| Platform | Status |
|----------|--------|
| macOS (Apple Silicon) | **Fully supported** |
| Linux x86_64 | **Fully supported** |
| Linux ARM64 | **Fully supported** |
| macOS (Intel) | Not supported (no ONNX binaries) |
| Windows | WSL2 required |

### Windows (WSL2)

Run Claude Code inside WSL2 Ubuntu, then install CSR there:

```bash
# Inside WSL2 Ubuntu
curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh
csr-engine setup
```

If CSR can't find your conversations, symlink the Claude data directory:

```bash
ln -s /mnt/c/Users/<windows-user>/.claude ~/.claude
```

> **Note**: Both Claude Code and CSR must run inside the same WSL2 environment. CSR is a native Linux binary — it doesn't run on Windows directly.

## Uninstall

```bash
claude mcp remove claude-self-reflect
rm -rf ~/.claude-self-reflect/
rm ~/.local/bin/csr-engine
```
