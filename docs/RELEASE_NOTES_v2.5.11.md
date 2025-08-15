# Release Notes - v2.5.11

## Summary
Major infrastructure release with critical cloud mode fix, robust embedding management system, real-time indexing progress tracking, and enhanced search experience. This comprehensive update resolves cloud mode functionality while introducing significant performance improvements and user experience enhancements.

## Major Features & Improvements

### Real-Time Indexing Progress Tracking
- **Live Status Display**: Search results now show indexing progress with percentage completion
- **Backlog Visibility**: Clear indication of pending conversations awaiting import
- **Smart Monitoring**: Lightweight status checks every 5 minutes to avoid performance impact
- **User Experience**: Immediate feedback on system indexing state

### Robust Embedding Management System
- **New EmbeddingManager Class**: Complete rewrite of embedding initialization with proper fallback
- **Stale Lock Cleanup**: Automatically removes stuck FastEmbed locks from previous runs
- **Thread Optimization**: Configures optimal thread limits before model loading
- **Cache Management**: Controlled cache directory with proper cleanup mechanisms
- **Memory Efficiency**: Lazy initialization - models loaded only when needed

### Enhanced Search Experience
- **Upfront Summaries**: Key information displayed before detailed XML results
- **Relevance Indicators**: Clear scoring (high/good/partial relevance)
- **Performance Metrics**: Detailed timing breakdown for search operations
- **Result Categorization**: Results organized by recency (today/this week/older)

### Critical Cloud Mode Fix
- **Environment Variables**: Fixed propagation from Claude Code's `-e` flags to Python MCP server
- **Voyage AI Support**: Restored cloud embedding functionality with 1024-dimensional vectors
- **Debug Logging**: Added startup diagnostics showing configuration status
- **Seamless Switching**: Proper mode transitions between local and cloud embeddings

## Technical Details

### New Architecture Components

#### EmbeddingManager Class (`embedding_manager.py`)
- **Robust Initialization**: Handles both local and cloud embedding models with intelligent fallback
- **Cache Optimization**: Manages FastEmbed cache directory with automatic cleanup
- **Lock Management**: Removes stale lock files from interrupted downloads
- **Thread Control**: Sets optimal thread limits before model loading to prevent resource contention
- **Timeout Handling**: Configurable download timeouts with graceful error handling

#### Indexing Status Tracking
- **Real-Time Monitoring**: Tracks conversation import progress across all projects
- **Performance Optimized**: Updates every 5 minutes to minimize overhead
- **Detailed Metrics**: Provides indexed/total counts, percentage completion, and backlog size
- **Thread-Safe**: Prevents concurrent status checks with proper locking

#### Enhanced Search Response Format
- **Upfront Information**: Key stats displayed before collapsible XML results
- **Indexing Visibility**: Progress shown prominently when system is importing
- **Result Categorization**: Automatic grouping by recency (today/this week/older)
- **Performance Insights**: Detailed timing breakdown for optimization

### Environment Variable System
- **Files Modified**: 
  - `mcp-server/run-mcp.sh`: Added explicit export statements for all environment variables
  - `mcp-server/src/server.py`: Updated `load_dotenv()` with `override=False` to prioritize process environment
  - `mcp-server/src/embedding_manager.py`: New robust embedding management system
  - `package.json`: Bumped version to 2.5.11

### Supported Environment Variables
- `PREFER_LOCAL_EMBEDDINGS`: Controls local (FastEmbed) vs cloud (Voyage AI) embedding mode
- `VOYAGE_KEY`, `VOYAGE_KEY_2`: API keys for Voyage AI cloud embeddings  
- `QDRANT_URL`: Vector database connection URL
- `ENABLE_MEMORY_DECAY`: Time-based relevance weighting
- `DECAY_WEIGHT`, `DECAY_SCALE_DAYS`: Memory decay configuration
- `EMBEDDING_MODEL`: Specifies embedding model for local mode
- `FASTEMBED_DOWNLOAD_TIMEOUT`: Download timeout for FastEmbed models

### Performance Improvements
- **Lazy Loading**: Embedding models initialize only when first needed
- **Cache Efficiency**: Proper FastEmbed cache management prevents re-downloads
- **Thread Optimization**: Controlled threading for model loading operations
- **Memory Management**: Reduced startup memory usage through deferred initialization

## Verification
**Cloud Mode Testing Completed**:
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