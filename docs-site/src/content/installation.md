---
title: Installation
---

## Requirements

- **macOS** (Apple Silicon) or **Linux** (x86_64 / ARM64)
- **Claude Code** CLI installed
- ~78 MB disk for the binary, ~87 MB for the embedding model (downloaded on first run); the database grows with your history
- For the optional companion pane: Claude Code 2.1.296 or later, and the [herdr](https://herdr.dev) terminal multiplexer (`brew install herdr`)

No Docker, no API keys, and no Node, TypeScript or Python at run time: the companion pane runs inside the `csr-engine` binary.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh
```

Downloads the binary (SHA256-verified) and then **asks before activating**. Setup, which imports conversations, registers the MCP server, installs all 6 hooks, installs the csr-companion plugin and, on macOS, installs and starts the background daemon, only runs after you confirm at the prompt.

Non-interactive installs (CI, scripts) never activate automatically. Control the behavior with:

| Variable | Effect |
|----------|--------|
| `CSR_AUTO_SETUP=1` | Run setup without prompting |
| `CSR_SKIP_SETUP=1` | Download only, never run setup |
| `CSR_NO_DAEMON=1` | Setup does not install or start the daemon (dreams and background enrichment then need `csr-engine daemon` run by hand) |

### npm

```bash
npm install -g claude-self-reflect
claude-self-reflect setup
```

By default `npm install` only downloads the checksummed binary — it does not modify `~/.claude/settings.json`, register MCP servers, or index conversations. Activation is the separate, explicit `claude-self-reflect setup` step, or set `CSR_AUTO_SETUP=1` during install to opt in. That command runs the binary npm downloaded, so it works before `~/.local/bin` is on your PATH.

### The companion plugin

After the other steps succeed, setup installs csr-companion (the pane beside the session, which needs the [herdr](https://herdr.dev) terminal multiplexer: `brew install herdr`), under the same consent as the rest of setup. The binary writes the plugin as a local marketplace under `~/.claude-self-reflect/csr-companion/marketplace`, and Claude Code's own plugin CLI installs it from there, so the plugin's version is always the engine's. The plugin is one generated file that starts `csr-engine companion serve`. Then run `/reload-plugins` in Claude Code, or restart it.

- `csr-engine setup --no-companion` skips the plugin.
- If the install fails or is skipped, setup prints the reason and the command that retries it: `csr-engine companion plugin install`.
- A failed plugin install never fails setup.

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

There is no `uninstall` command. `npm uninstall` does not touch your Claude Code settings, and `csr-engine hook install --apply` only adds hooks. Remove the hooks and the daemon by hand:

```bash
# 1. Stop the daemon LaunchAgent that setup installed (macOS)
launchctl bootout gui/$(id -u)/com.ramakay.csr-engine.daemon
rm ~/Library/LaunchAgents/com.ramakay.csr-engine.daemon.plist

# 2. Unregister the MCP server
claude mcp remove claude-self-reflect

# 3. Delete CSR's data, including the transcript mirror
rm -rf ~/.claude-self-reflect/

# 4. Delete the binary
rm ~/.local/bin/csr-engine
npm uninstall -g claude-self-reflect  # if installed via npm
```

Then open `~/.claude/settings.json` and delete every entry under `hooks` whose command starts with `csr-engine hook` (SessionStart, UserPromptSubmit, PostToolUse, Stop, PreCompact, SessionEnd). Claude Code keeps calling them until you do, and they fail once the binary is gone. If you installed the companion plugin, remove it with `/plugin` in Claude Code.
