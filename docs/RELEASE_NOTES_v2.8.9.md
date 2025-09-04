# Release Notes - v2.8.9

**Updated: September 4, 2025 | Version: 2.8.9**

## Summary
This is a critical bug fix release that resolves two major installation issues preventing new users from successfully installing and running Claude Self Reflect. These fixes address broken npm global installations and Python 3.13 dependency conflicts.

## Critical Bug Fixes

### NPM Global Installation Fixed (#47)
- **Issue**: Setup wizard was referencing non-existent `run-mcp-clean.sh` script for npm installations
- **Impact**: All npm global installations (`npm install -g claude-self-reflect`) were failing with MCP connection errors
- **Fix**: Changed setup wizard to use correct `run-mcp.sh` script which is properly included in the npm package
- **Files Modified**: 
  - `installer/setup-wizard-docker.js`: Updated script path reference
  - `package.json` and `pyproject.toml`: Version bumps

### Python 3.13 Dependency Conflict Resolved (#49)
- **Issue**: Docker builds failing on Python 3.13 due to numpy version conflict
- **Impact**: `qdrant-client 1.15.0` requires `numpy>=2.1.0` for Python 3.13, but Dockerfiles were pinned to `numpy==1.26.4`
- **Fix**: Updated Docker configurations to use `numpy>=2.1.0`
- **Files Modified**:
  - `Dockerfile.safe-watcher`: Updated numpy dependency
  - `Dockerfile.streaming-importer`: Updated numpy dependency

## Security Improvements

### False Positive Security Scan Resolution
- **Issue**: Security scans were flagging `VOYAGE_KEY=` pattern in logging as potential credential exposure
- **Fix**: Refined logging patterns to avoid false positives while maintaining security
- **Impact**: Cleaner CI/CD pipeline without unnecessary security alerts

## Major Feature (Carried from v2.8.8)

### New MCP Tool: get_full_conversation
- **Purpose**: Provides complete conversation file access instead of truncated search results
- **Benefit**: Agents can now retrieve full JSONL files (95% value vs 5% from 200-char excerpts)
- **Features**:
  - Direct JSONL file path access for conversation IDs
  - Handles both local and _local collection naming conventions
  - Graceful handling of missing files
  - Full conversation context for enhanced agent analysis

## Documentation Enhancements

### README Improvements
- **Added**: Comprehensive MCP Tools reference section
- **Improved**: Table of contents with collapsible sections
- **Removed**: Excessive emojis for professional presentation
- **Enhanced**: Installation and troubleshooting guidance

## Technical Specifications

### Compatibility
- **Python**: 3.10+ (full Python 3.13 support)
- **Node.js**: 18.0.0+
- **Docker**: Latest stable versions
- **Claude Code**: Full MCP integration

### Dependencies
- Updated numpy requirements for Python 3.13 compatibility
- Maintained backward compatibility with existing installations
- No breaking changes to existing API

## Installation

### NPM Global Installation (Recommended)
```bash
npm install -g claude-self-reflect@2.8.9
claude-self-reflect
```

### Docker Installation
```bash
git clone https://github.com/ramakay/claude-self-reflect.git
cd claude-self-reflect
docker compose --profile mcp --profile watch up -d
```

### Upgrade from Previous Versions
```bash
# NPM users
npm install -g claude-self-reflect@latest

# Docker users
docker compose down
git pull
docker compose --profile mcp --profile watch build --no-cache
docker compose --profile mcp --profile watch up -d
```

## Verification Steps

After installation, verify the fixes:

1. **NPM Installation**: Check MCP connection in Claude Code
   ```bash
   claude mcp list
   # Should show claude-self-reflect with status: connected
   ```

2. **Python 3.13 Support**: Docker builds should complete without dependency errors
   ```bash
   docker compose build
   # Should complete without numpy conflict errors
   ```

3. **MCP Tools Access**: Test tools are available in Claude
   - reflect_on_past
   - store_reflection  
   - get_full_conversation

## Contributors

Special thanks to our community contributors who reported these critical issues:

- **@dantodor** - Reported npm global installation issue with detailed debugging information (#47)
- **@cmbcbe** - Identified Python 3.13 dependency conflict and provided error logs (#49)

## Related Issues

- **Resolves #47**: Wrong command line in installation
- **Resolves #49**: numpy dependency issue
- **Improves**: Security scan accuracy and CI/CD reliability

## Breaking Changes
**None** - This release maintains full backward compatibility while fixing critical installation issues.

## Support

- **GitHub Issues**: [Report bugs and feature requests](https://github.com/ramakay/claude-self-reflect/issues)
- **Documentation**: [Complete setup guide and troubleshooting](https://github.com/ramakay/claude-self-reflect#readme)
- **MCP Reference**: [Available tools and usage examples](https://github.com/ramakay/claude-self-reflect/blob/main/docs/development/MCP_REFERENCE.md)

---

This release ensures Claude Self Reflect works reliably for all new installations and provides the foundation for continued development of semantic conversation memory features.