---
title: Installation
---

## Requirements

- **macOS** (Apple Silicon) or **Linux** (x86_64 / ARM64). The Linux binaries are built on Ubuntu 24.04 (glibc 2.39); older glibc is not supported
- **Claude Code** CLI installed
- About 138 MB disk for the binary plus the embedding model (downloaded on first run); the database grows with your history
- For the optional companion: Claude Code 2.1.296 or later. The [herdr](https://herdr.dev) terminal multiplexer (`brew install herdr`) is optional: with it the companion is a pane beside Claude, without it a panel inside Claude Code. See [Choose your view](#/docs/companion-views).

No Docker, no API keys, and no Node, TypeScript or Python at run time: the companion pane runs inside the `csr-engine` binary.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh
```

Downloads the binary (SHA256-verified) and then **asks before activating**. Setup, which imports conversations, registers the MCP server, installs all 8 hooks on 7 events, installs the csr-companion plugin and, on macOS, installs and starts the background daemon, only runs after you confirm at the prompt.

Non-interactive installs (CI, scripts) never activate automatically. Control the behavior with:

| Variable | Effect |
|----------|--------|
| `CSR_AUTO_SETUP=1` | Run setup without prompting |
| `CSR_SKIP_SETUP=1` | Download only, never run setup |
| `CSR_NO_DAEMON=1` | Setup does not install or start the daemon (dreams and background enrichment then need `csr-engine daemon` run by hand) |

To pin a release, pass `--version` (`curl -fsSL .../install.sh | sh -s -- --version v10.1.0`) or set `CSR_VERSION=10.1.0`; both `10.1.0` and `v10.1.0` work. To install from files you already have, set `CSR_TARBALL=/path/to/csr-engine-<target>.tar.gz` with `checksums.txt` beside it (or `CSR_CHECKSUMS=/path`), or set `CSR_RELEASE_BASE_URL` to a directory, `file://` URL or https URL that holds both files (used as given, no version directory added). A local tarball, directory or file:// source makes no network call, and the checksum is always verified: without a checksums file the install stops. Answering yes at "Run setup now?" also gives setup its consent for dreams, so it does not ask a second time.

### What calls `claude -p`

Some features run the Claude Code CLI in print mode (`claude -p`) as a child process. It runs under your own Claude account, so it uses your plan's usage (or your API key's billing, if that is how Claude Code is set up); CSR sends nothing to any other service for this. The callers are the `session-briefing` SessionStart hook (on startup and resume; once a briefing is stored for a project, there is no new call for it for 30 minutes), session narratives and stories, the daemon's ratification pass, and dreams. `CSR_NO_AI_NARRATIVES=1` turns all of them off, the briefing included. `CSR_NO_DREAMING=1` stops dreams only and `CSR_NO_RATIFICATION=1` stops ratification only.

| Caller | When it calls | Typical count | Pace and switch |
|---|---|---|---|
| Session briefing | a session start or resume | about one per session; none for a project briefed in the last 30 min | `CSR_NO_AI_NARRATIVES=1` |
| Ratification (daemon) | once per conversation: each session, subagent run and Codex rollout | one per session you ran, plus one per subagent run and one per Codex rollout; a conversation whose answer fails to parse twice is not tried again | at most 4 an hour (`CSR_RATIFICATION_PER_HOUR`), only conversations active in the last 7 days (`CSR_RATIFICATION_DAYS`, 0 for all); off: `CSR_NO_RATIFICATION=1` |
| Narratives and stories | a finished session, a story refresh | a few per active session | `CSR_NO_AI_NARRATIVES=1` |
| Dreams (daemon) | one pass per cadence | one pass per 6 h at most, within the budget setup shows | `CSR_DREAM_INTERVAL_SECS`, `CSR_NO_DREAMING=1` |

A fresh install does not work through your whole imported history at once: ratification makes at most 4 calls in its first hour and leaves conversations older than a week alone (`csr-engine ratify` still takes any). To change a pace or a switch for the daemon, run `csr-engine setup` with the variable set: setup copies `CSR_*` variables into the launch agent.

The default model differs by caller, and each has its own variable. Each chain tries your variable first (if set), then the caller's default, then the Claude Code CLI's own default (no `--model`).

| Caller | Default model | Variable that changes it |
|--------|---------------|--------------------------|
| Briefing, session narratives and stories, `csr-engine dream backfill`, `csr-engine dream --report` headlines | `haiku` | `CSR_NARRATIVE_MODEL` |
| Ratification (daemon) | `haiku` | `CSR_RATIFICATION_MODEL` |
| Dreams: dream threads and the plan pass (`CSR_DREAM_THREADS=1`), `csr-engine dreams` | `sonnet-5` (the `balanced` tier) | `CSR_DREAM_THREAD_MODEL` names a model; `CSR_DREAM_EFFORT` (`less`, `balanced` or `max`) picks `haiku-4`, `sonnet-5` or `opus-5`, and the model variable wins |

`csr-engine status` shows the first model dreams try in `dream.effort.model`. Set the variable where Claude Code starts (hooks read it there) and run setup with it set so the daemon's agent keeps it. Defaults are unchanged.

launchd does not read your shell, so setup writes the daemon's settings into its agent file. Any other `CSR_*` variable you run setup with (for example `CSR_NO_DREAMING=1` or `CSR_NARRATIVE_MODEL`) is written into the agent, and anything an existing agent already had is kept; setup prints what it carried. A daemon agent you installed by hand under another label is adopted, not duplicated.

### npm

```bash
npm install -g claude-self-reflect
claude-self-reflect setup
```

By default `npm install` only downloads the checksummed binary — it does not modify `~/.claude/settings.json`, register MCP servers, or index conversations. Activation is the separate, explicit `claude-self-reflect setup` step, or set `CSR_AUTO_SETUP=1` during install to opt in. That command runs the binary npm downloaded, so it works before `~/.local/bin` is on your PATH. On macOS, an npm upgrade that replaces the binary also restarts an already loaded daemon (`launchctl kickstart -k`), even without `CSR_AUTO_SETUP`. It does not touch `~/.claude`.

### The companion plugin

After the other steps succeed, setup installs csr-companion, under the same consent as the rest of setup. Inside [herdr](https://herdr.dev) it opens a pane beside Claude by itself. Without herdr, type `/csr` in Claude Code to open its panel; in the 10.1.0 clean-machine test it did not open by itself. Setup does not choose between the two; see [Choose your view](#/docs/companion-views). The binary writes the plugin as a local marketplace under `~/.claude-self-reflect/csr-companion/marketplace`, and Claude Code's own plugin CLI installs it from there, so the plugin's version is always the engine's. The plugin is one generated file that starts `csr-engine companion serve`. Then run `/reload-plugins` in Claude Code, or restart it.

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
| Linux x86_64 | **Fully supported** (built on Ubuntu 24.04, glibc 2.39; older glibc is not supported) |
| Linux ARM64 | **Fully supported** (built on Ubuntu 24.04, glibc 2.39; older glibc is not supported) |
| macOS (Intel) | Not supported (no prebuilt binary; the ONNX runtime has no x86_64-apple-darwin build) |
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

`csr-engine uninstall` removes what `setup` installed and nothing else:

```bash
csr-engine uninstall --dry-run            # print the plan, change nothing (also the default without --yes)
csr-engine uninstall --yes                # do it; your data folder ~/.claude-self-reflect is kept
csr-engine uninstall --yes --purge        # also delete the data folder, except the transcript mirror
csr-engine uninstall --yes --purge-mirror # delete the data folder and the mirror too (implies --purge)
```

It removes CSR's hooks from `~/.claude/settings.json` (your other hooks stay; a `settings.json.csr-uninstall-<time>` backup is written first), the `claude-self-reflect` MCP entry (and any project-scoped entry of that name, through `claude mcp remove -s local` run from the project folder), the companion plugin and its cache under `~/.claude/plugins/cache/claude-self-reflect`, the launch agent `setup` wrote (a plist you wrote yourself is left alone), and the npm global package `claude-self-reflect` when `npm ls -g` lists it. Your data is kept by default: `~/.claude-self-reflect/` (database, settings, the transcript mirror) stays, and its path and size are printed with "kept; --purge deletes it". `--purge` deletes that folder except the transcript mirror, the only copy of transcripts Claude Code has deleted; `--purge-mirror` deletes the mirror too. (`--keep-data` from older versions is still accepted and does nothing.) `~/.claude/projects` is never touched. The Codex MCP entry is removed through `codex mcp remove`, with Codex's `config.toml` copied aside first. The launch agent is booted out and its plist deleted first, so nothing starts the daemon again at login. Uninstall then waits up to 35 s (launchd's `ExitTimeOut` plus 5 s) for the daemon to exit. A daemon still running then gets one SIGTERM, sent only when launchd and `daemon.lock` name the same pid, and 5 s more. If the daemon has not stopped, the npm package, the binary and, with `--purge`, the data folder stay, the command exits 1, and its last line starts with "not finished:" and gives the exact command that finishes once `launchctl print` no longer finds the job. It also deletes the model cache `~/Library/Caches/csr-engine` (the downloaded embedding model; it is not your data, so it goes without `--purge` too), and last every csr-engine binary the hooks, the MCP entries and the launch agent named, plus the one running: a copy in `~/.local/bin` is deleted, any other (a build folder, `/usr/local/bin`) is not touched and its exact `rm` line is printed. Every step prints a line for what it removed or kept. Running csr-engine sessions are listed at the end with their PIDs, and never signalled (only the launchd daemon can get that one SIGTERM): restart them, or they keep using the old hooks and, after `--purge`, recreate the data folder. The plan (without `--yes`) runs only read-only probes (`npm ls`, `ps`).

By hand, if you prefer:

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

Then open `~/.claude/settings.json` and delete every entry under `hooks` whose command starts with `csr-engine hook` (SessionStart, UserPromptSubmit, PostToolUse, Stop, PreCompact, PostCompact, SessionEnd). Claude Code keeps calling them until you do, and they fail once the binary is gone. If you installed the companion plugin, remove it with `/plugin` in Claude Code.
