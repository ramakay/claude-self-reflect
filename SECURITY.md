# Security Policy

## Supported Versions

| Version | Supported |
| ------- | --------- |
| 10.1.x  | Yes |
| 9.5.x   | Critical and high security fixes only |
| < 9.5   | No |

## Security Model

Self-Reflect 10.1 runs as a single local Rust binary (`csr-engine`).
There is no Docker service, Qdrant server, Python runtime, or hosted database.

### Local-First Defaults

- Conversation files are read from `~/.claude/projects/` (plus Claude Code plans,
  tasks and history, and Codex rollouts when present). They are never modified.
- CSR data is stored locally under `~/.claude-self-reflect/`.
- Embeddings are generated locally with FastEmbed.
- Search runs against a local SQLite database plus an in-memory HNSW index.
- Secrets are redacted before text is indexed or mirrored.
- No telemetry is collected.

### Network and model use

These run without any opt-in once `csr-engine setup` has activated CSR:

- **Model download.** The first run downloads the embedding model
  (all-MiniLM-L6-v2) from Hugging Face into the user cache folder.
- **Background `claude -p` calls.** Several features run the Claude Code CLI in
  print mode as a child process, under the user's own Claude Code login or API
  key: the async SessionStart briefing, session stories and narratives, the
  daemon's ratification pass, and dreams. These send conversation excerpts to
  Anthropic the same way Claude Code itself does.
- **Companion Dreams.** On by default once the companion plugin is installed. It
  forks the session's model for one "what if" look per cycle.
- **Local dream journal.** The daemon serves the journal web page on
  `127.0.0.1:7373` only (loopback, no setting binds another interface).
  `CSR_JOURNAL_PORT` moves it; `CSR_NO_JOURNAL_SERVER=1` turns it off.
- **Batch API narrator.** Runs only when `ANTHROPIC_API_KEY` is set in the
  environment of the daemon or MCP server.
- **Installers** download release assets from GitHub Releases.

Off switches (set them where Claude Code and the daemon start):

- `CSR_NO_AI_NARRATIVES=1` stops the briefing, every story call, ratification,
  dream threads and the dream commands.
- `CSR_NO_DREAMING=1` stops dreams.
- `CSR_NO_RECAP=1` stops the SessionStart recap paragraph (built locally, no
  model call).
- Companion Dreams: turn Dreams off in `/config`. The Batch API narrator:
  remove the key, or run `csr-engine daemon --no-ai`.

No single setting turns off every model call. The full list, with each caller and
its switch, is in the docs: Privacy & Security
(https://ramakay.github.io/claude-self-reflect/#/docs/privacy).

Do not run CSR on conversation data that is not appropriate to send to
Anthropic unless the switches above are set.

## Installer Integrity

The npm postinstall path and shell installer download release assets over HTTPS
from `github.com/ramakay/claude-self-reflect`. Binary release archives are
verified against the release `checksums.txt` before installation.

Users who need offline or custom binary management can set:

```bash
CSR_SKIP_BINARY_DOWNLOAD=1
```

Then install a trusted `csr-engine` binary manually into the desired install
directory.

## File Permissions

Recommended permissions:

```bash
chmod 700 ~/.claude-self-reflect
chmod 600 ~/.claude-self-reflect/*.db 2>/dev/null || true
chmod 700 ~/.local/bin
chmod 755 ~/.local/bin/csr-engine
```

For shared workstations, rely on OS user isolation. Do not place
`~/.claude-self-reflect` or Claude transcript directories in a world-readable
location.

## Hook Safety

CSR installs Claude Code hooks that run `csr-engine hook ...`.

- Hooks use catch-all error handling and should not block Claude Code.
- Session-start and predictive injection output must frame retrieved memories as
  past context, not current instructions.
- Hook config installation removes older Python CSR hook commands when applying
  the Rust hook configuration.

## API Key Handling

`claude -p` calls use the credentials Claude Code uses on the machine. The Batch
API narrator uses `ANTHROPIC_API_KEY`. Store keys in user-owned
configuration files or environment variables only.

- Never commit API keys.
- Rotate keys if they appear in transcripts, shell history, or logs.
- Review provider privacy policies before enabling external enrichment.

## Reporting a Vulnerability

Do not create public GitHub issues for security vulnerabilities.

Preferred reporting path:

1. Go to https://github.com/ramakay/claude-self-reflect/security/advisories
2. Click "Report a vulnerability"
3. Include reproduction steps, affected version, impact, and suggested fix when
   available.

Expected response:

- Acknowledgment within 48 hours.
- Initial assessment within 7 days.
- Fix timeline estimate within 14 days.

## Disclosure Policy

- We follow coordinated disclosure.
- Security fixes are released as patch versions.
- Public disclosure happens after a fix is available.
- Credit is given to security researchers with permission.

## Security Updates

- Critical: released within 48 hours.
- High: released within 7 days.
- Medium: released within 30 days.
- Low: included in the next regular release.
