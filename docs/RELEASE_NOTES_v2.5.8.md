# Release Notes - v2.5.8

Updated: 2025-08-11 | Version: 2.5.8

## Summary
Critical bug fix addressing project-scoped search functionality that was fundamentally broken since inception. This release ensures that searches are properly isolated to the current project instead of always returning results from the claude-self-reflect collection.

## Changes

### Bug Fixes
- **CRITICAL: Fixed Project-Scoped Search Isolation** (#22)
  - **Root Cause**: MCP server was using `os.getcwd()` which always returned the server's directory (`mcp-server/`), not Claude Code's working directory
  - **Impact**: All searches returned claude-self-reflect conversations regardless of which project users were actually working in
  - **Solution**: Modified `run-mcp.sh` to capture original working directory as `MCP_CLIENT_CWD` environment variable
  - **Files Modified**:
    - `mcp-server/run-mcp.sh`: Added `export MCP_CLIENT_CWD="$PWD"` to capture client working directory
    - `mcp-server/src/server.py`: Updated project detection to use `MCP_CLIENT_CWD` instead of `os.getcwd()`
    - `mcp-server/src/utils.py`: Enhanced project name normalization for various path formats

### Technical Details
- **Environment Variable**: `MCP_CLIENT_CWD` now contains the actual Claude Code working directory
- **Backward Compatibility**: Existing search functionality remains unchanged, now correctly scoped
- **No User Action Required**: Fix is automatic upon restart
- **Performance**: No impact on search performance or latency

### Verification
- Project-scoped searches now correctly return results only from the current project
- Cross-project searches still work when explicitly requested with `project="all"`
- No more cross-project contamination in search results
- Works automatically without user configuration changes

## Installation
```bash
# Update to the latest version
git pull origin main

# Restart MCP server in Claude Code
claude mcp remove claude-self-reflect
claude mcp add claude-self-reflect "/path/to/mcp-server/run-mcp.sh" -e QDRANT_URL="http://localhost:6333"

# Restart Claude Code for changes to take effect
```

## Contributors
Special thanks for reporting and helping to identify this critical issue that affected project isolation.

## Related Issues
- Resolves project-scoped search returning incorrect cross-project results
- Fixes semantic search context bleeding between different projects