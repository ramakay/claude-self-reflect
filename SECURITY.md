# Security Policy

## Supported Versions

| Version | Supported |
| ------- | --------- |
| 8.0.x   | Yes       |
| < 8.0   | No        |

## Security Model

Claude Self-Reflect v8 runs as a single local Rust binary (`csr-engine`).
There is no Docker service, Qdrant server, Python runtime, or hosted database in
the default deployment.

### Local-First Defaults

- Conversation files are read from `~/.claude/projects/`.
- CSR data is stored locally under `~/.claude-self-reflect/`.
- Embeddings are generated locally with FastEmbed.
- Search runs against a local SQLite database plus an in-memory HNSW index.
- No telemetry is collected.

### Optional Network Use

The default install does not send conversation text to third-party APIs.
Network use is limited to:

- Installer downloads from GitHub Releases.
- Optional AI narrative enrichment through Anthropic APIs, when explicitly
  configured by the user.

Do not enable optional AI enrichment unless the conversation data is appropriate
to send to that provider.

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

Optional enrichment may require provider API keys. Store keys in user-owned
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
