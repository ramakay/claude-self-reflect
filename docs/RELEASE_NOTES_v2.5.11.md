# Release Notes - v2.5.11

## Summary
Critical bug fix enabling proper cloud mode operation with Voyage AI embeddings. This release resolves an issue where environment variables from Claude Code's `-e` flags were not being passed to the MCP server Python process, preventing users from accessing cloud embedding functionality.

## Critical Fix

### MCP Environment Variable Propagation Failure
- **Issue**: Environment variables set via `claude mcp add -e PREFER_LOCAL_EMBEDDINGS=false` were not reaching the Python MCP server process
- **Impact**: Cloud mode with Voyage AI embeddings was non-functional, MCP always defaulted to local FastEmbed mode
- **Root Cause**: Shell script `run-mcp.sh` did not export environment variables from Claude Code to the Python process
- **Solution**: Enhanced `run-mcp.sh` to properly export all MCP-related environment variables with debug logging

## Technical Details

### Environment Variable Fix
- **Files Modified**: 
  - `mcp-server/run-mcp.sh`: Added explicit export statements for all environment variables
  - `mcp-server/src/server.py`: Updated `load_dotenv()` with `override=False` to prioritize process environment
  - `package.json`: Bumped version to 2.5.11

### Affected Environment Variables
- `PREFER_LOCAL_EMBEDDINGS`: Controls local (FastEmbed) vs cloud (Voyage AI) embedding mode
- `VOYAGE_KEY`: API key for Voyage AI cloud embeddings  
- `VOYAGE_KEY_2`: Alternative API key for Voyage AI
- `QDRANT_URL`: Vector database connection URL
- `ENABLE_MEMORY_DECAY`: Time-based relevance weighting
- `DECAY_WEIGHT`, `DECAY_SCALE_DAYS`: Memory decay configuration
- `EMBEDDING_MODEL`: Specifies embedding model for local mode

### Debug Enhancement
- Added startup logging to show which environment variables are set
- Environment variables are now properly masked in logs for security
- Clear indication of embedding mode (local vs cloud) during MCP server startup

## Verification
✅ **Cloud Mode Testing Completed**:
- Environment variables properly passed via `claude mcp add -e` flags
- MCP server correctly receives `PREFER_LOCAL_EMBEDDINGS=false`
- Search results show `<embed>voyage</embed>` confirming cloud mode
- 1024-dimensional Voyage AI vectors validated
- Seamless switching between local and cloud modes
- Data integrity maintained during mode transitions

## Installation
```bash
npm install -g claude-self-reflect@2.5.11
```

## Upgrade Instructions
After installing this fix:

1. **Remove existing MCP server**:
   ```bash
   claude mcp remove claude-self-reflect
   ```

2. **Restart Claude Code completely** for changes to take effect

3. **Re-add MCP with environment variables**:
   ```bash
   # For cloud mode (Voyage AI)
   claude mcp add claude-self-reflect "/path/to/run-mcp.sh" -e PREFER_LOCAL_EMBEDDINGS="false" -e VOYAGE_KEY="your-key" -e QDRANT_URL="http://localhost:6333" -s user
   
   # For local mode (FastEmbed)  
   claude mcp add claude-self-reflect "/path/to/run-mcp.sh" -e PREFER_LOCAL_EMBEDDINGS="true" -e QDRANT_URL="http://localhost:6333" -s user
   ```

4. **Verify configuration**: Check MCP server logs show correct embedding mode

## Breaking Changes
None. This is a backward-compatible fix that maintains existing functionality while enabling proper environment variable support.

## Benefits
- **Cloud Embedding Support**: Users can now access Voyage AI's superior semantic search accuracy
- **Proper Configuration**: Environment variables work as documented in MCP_REFERENCE.md
- **Enhanced Debugging**: Clear logging shows configuration status during startup
- **Mode Flexibility**: Easy switching between local and cloud embedding modes

## Contributors
Critical bug fix identified and resolved by the Claude Self Reflect development team with comprehensive testing validation.

## Related Issues
This fix resolves the core issue preventing cloud mode operation. Users who experienced search results always showing `<embed>local</embed>` despite cloud configuration can now access full Voyage AI functionality.

## Support
If you encounter any issues with this release:
- Check GitHub Issues: https://github.com/ramakay/claude-self-reflect/issues
- Verify environment variables are properly set with debug logs
- Ensure Claude Code has been completely restarted after MCP reconfiguration
- Confirm Voyage AI API key is valid if using cloud mode

## What's Next
With environment variable support now working correctly, future releases will focus on:
- Enhanced cloud mode features and optimizations
- Improved user experience for mode switching
- Additional embedding model support