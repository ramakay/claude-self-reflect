# Release Notes - v6.0.1

## Summary
Critical patch release fixing Docker volume mount failures on macOS global npm installations. Adds new utility commands for version checking and clean uninstallation.

## Critical Bug Fix

### Docker Volume Mount Failure on macOS
Resolves issue where Claude Self-Reflect would fail to start after global npm installation on macOS due to Docker Desktop path restrictions.

**Problem:**
- Global npm install places package in `/opt/homebrew/lib/node_modules/` (Homebrew) or `/usr/local/lib/node_modules/` (npm)
- Docker Desktop on macOS only allows mounts from `/Users`, `/Volumes`, `/private`, `/tmp`
- `docker-compose.yaml` had development-only volume mounts: `./src:/app/src:ro` and `./shared:/app/shared:ro`
- These resolved to restricted paths, causing Docker to reject the configuration

**Error Message:**
```
Error: Path /opt/homebrew/lib/node_modules/claude-self-reflect/src
       is not shared from the host and is not known to Docker.
```

**Solution:**
- Removed development-only volume mounts (`./src` and `./shared`) from all services
- Code is already embedded in Docker images via `Dockerfile.*` builds
- Kept user-facing mounts (`CONFIG_PATH` and `CLAUDE_LOGS_PATH`) which correctly expand to `/Users/...`
- No functionality impact - Docker images contain all necessary code

## New Features

### 1. Version Command
Check installed version and available updates:
```bash
claude-self-reflect version
```

**Features:**
- Shows current installed version
- Checks npm registry for latest version
- Displays update command if newer version available
- Shows Docker container status
- Quick health check

### 2. Uninstall Command
Complete removal with optional data backup:
```bash
claude-self-reflect uninstall
```

**Features:**
- Offers data backup before uninstalling
- Removes MCP server from Claude Desktop configuration
- Stops and removes Docker containers
- Removes Docker volumes
- Uninstalls npm package
- Cleans up configuration directory
- Safe with confirmation prompts

## CI/CD Improvements

### New Validation Job: `docker-compose-validation`
Prevents this class of issues from recurring in future releases.

**Script:** `scripts/ci/validate-docker-compose.sh`

**Checks:**
- Detects invalid `./src` and `./shared` mounts in service definitions
- Validates `CONFIG_PATH` has correct default: `~/.claude-self-reflect/config`
- Validates `CLAUDE_LOGS_PATH` has correct default: `~/.claude/projects`
- Enforces macOS Docker path restrictions
- Runs before `docker-build` to catch issues early

**Pipeline Position:**
```
python-test → npm-package-test → docker-compose-validation → docker-build → release
```

## Installation

### New Installation
```bash
npm install -g claude-self-reflect@6.0.1
claude-self-reflect setup
```

### Upgrade from Previous Version
```bash
npm update -g claude-self-reflect
# No additional steps needed - drop-in replacement
```

## Breaking Changes
**None** - This is a patch release with full backward compatibility.

## Migration Guide
**Not required** - Existing installations continue to work unchanged.

If you experience Docker mount issues after upgrade:
1. Stop containers: `docker compose down`
2. Restart containers: `docker compose --profile mcp up -d`
3. Verify: `docker compose ps`

## Technical Details

### Files Modified

| File | Change | Purpose |
|------|--------|---------|
| `docker-compose.yaml` | Modified | Removed `./src` and `./shared` mounts from 5 services |
| `scripts/setup/uninstall.sh` | New | Complete uninstall with backup option (232 lines) |
| `scripts/setup/version.sh` | New | Version check and update notifications (143 lines) |
| `scripts/ci/validate-docker-compose.sh` | New | CI validation to prevent regression (93 lines) |
| `.github/workflows/ci.yml` | Modified | Added `docker-compose-validation` job |
| `package.json` | Modified | Version bump: 6.0.0 → 6.0.1 |

### Services Updated
All Docker Compose services had `./src` and `./shared` mounts removed:
- `importer`
- `watcher`
- `streaming-importer`
- `async-importer`
- `safe-watcher`

### Testing Performed
- Docker compose validation: Passed
- npm package structure: Passed
- npm global install test: Passed
- Docker build: Passed
- Python tests (3.10, 3.11, 3.12): Passed
- Security scans (Snyk, CodeQL): Passed
- Code quality checks: Passed

## Verification

### Test Global Installation
```bash
# Uninstall old version
npm uninstall -g claude-self-reflect

# Install v6.0.1
npm install -g claude-self-reflect@6.0.1

# Verify version
claude-self-reflect version

# Test setup (should complete without Docker mount errors)
claude-self-reflect setup
```

### Verify Docker Containers
```bash
docker compose ps
# Should show all containers running without mount errors
```

## Known Issues
None in this release.

## Contributors
Thank you to everyone who reported issues and helped test this release:
- User report: Global npm installation Docker mount failure on fresh macOS install

## Related Issues
- Resolves: Docker volume mount failure on macOS global npm install
- Related to: #13 (previous Docker mount issues)

## Next Steps
After installing v6.0.1:
1. Run `claude-self-reflect version` to verify installation
2. Use `claude-self-reflect uninstall` if you need to remove CSR cleanly
3. Continue using Claude Code with full conversation memory

## Release Timeline
- PR Created: 2025-10-06
- CI/CD Passed: 2025-10-06
- PR Merged: 2025-10-06
- Release Created: 2025-10-06

---

**Full Changelog**: https://github.com/ramakay/claude-self-reflect/compare/v6.0.0...v6.0.1
