# Release History

## v2.5.6 - Tool Output Extraction 
**Released:** 2025-08-11

### 🚀 Major Feature: Tool Output Extraction
- **Metadata v2 Schema**: Captures tool outputs and git file changes for enhanced cross-agent discovery
- **Tool Output Capture**: Extracts up to 15 tool outputs (500 chars each) per conversation
- **Git File Tracking**: Parses git diff/show/status outputs to identify modified files
- **Two-Pass JSONL Parsing**: Complete output capture during imports

### New Search Capabilities
- **`search_by_file`**: Find conversations with git-modified files
- **`search_by_concept`**: Improved for git-related concepts
- **Enhanced Semantic Search**: Tool outputs included in search index

### Technical Improvements
- **Performance**: Minimal overhead (~10ms per conversation)
- **Storage**: ~2-5KB increase per conversation with tool outputs
- **Backward Compatible**: Works with existing v1 metadata
- **Files Modified**: `streaming-importer.py`, `import-conversations-unified.py`

## v2.5.5 - Critical Fixes & Enhancements
**Released:** 2025-08-11

### 🔒 Critical Security Fix
- **CRITICAL**: Fixed pydantic version conflict preventing MCP server startup
- **Dependency Update**: pydantic >=2.11.7 for fastmcp 2.10.6 compatibility
- **Runtime Stability**: Prevents dependency resolution failures

### Streaming Importer Enhancements
- **File Validation**: Enhanced detection of empty files (0 bytes)
- **Summary Detection**: Automatic skipping of summary-only files without conversation data
- **State Tracking**: Improved handling of skipped files to prevent queue blocking
- **Re-validation Logic**: Files re-checked if they grow or change

### Repository Cleanup
- **Archive Organization**: Old release notes moved to `docs/archive/releases/`
- **Dependencies Updated**: openai 1.97.1→1.98.0, qdrant-client 1.15.0→1.15.1
- **Test Cleanup**: Removed test artifacts from root directory

## v2.5.4 - Documentation & Bug Fixes
**Released:** 2025-08-07

### Bug Fixes
- Fixed critical import path bug: streaming-importer.py now correctly uses `~/.claude/projects/` instead of `~/.claude/conversations/`
- Fixed state file compatibility to handle both old string format and new dictionary format
- Improved import visibility for recent conversations (HOT path optimization)

### Documentation
- Comprehensive documentation update across all 77 markdown files
- Added streaming import architecture details
- Updated performance metrics to reflect 10-40x improvements
- Clarified project-scoped search behavior
- Fixed outdated directory paths throughout docs

## v2.5.3 - Streamlined Documentation
**Released:** 2025-08-06

### Documentation Improvements
- Reduced README.md size by 44% (394 → 221 lines)
- Created comprehensive import architecture diagram
- Moved detailed content to organized docs/ subdirectories
- Added horizontal flow diagram with HOT/WARM/COLD paths
- Improved visual clarity with PNG diagrams

### Performance
- Optimized file organization with intelligent content-based sorting
- Enhanced import prioritization for better real-time visibility

## v2.4.5 - Performance Revolution
**Released:** 2025-07-29

### 🚀 10-40x Performance Improvement
- End-to-end response time: Reduced from 28.9s-2min to just **200-350ms**
- Search latency: Optimized to 103-620ms (varies by collection count)
- Response size: Reduced by 40-60% through intelligent compression

### New Features
- **Debug Mode**: `include_raw=true` for troubleshooting
- **Response Formats**: Choose between XML (default) or markdown
- **Brief Mode**: `brief=true` for 60% smaller responses
- **Progress Reporting**: Real-time search progress updates

### Technical Details
- XML tag compression: 40% payload reduction using single-letter tags
- Timezone handling: Fixed datetime comparison issues
- Streaming support: Works properly with reflection-specialist agent
- MCP overhead: Reduced from 85% to manageable levels

### Known Issues
- Specialized search tools (`quick_search`, `search_summary`, `get_more_results`) work through the reflection-specialist agent but not via direct MCP calls due to FastMCP limitations

## v2.4.3 - Project-Scoped Search
**Released:** 2025-07-28

### ⚠️ Breaking Change
Searches now default to current project only. Previously searched all projects.

### How It Works
```
# Example: Working in ~/projects/ShopifyMCPMockShop
You: "What authentication method did we implement?"
Claude: [Searches ONLY ShopifyMCPMockShop conversations]

# To search everywhere
You: "Search all projects for WebSocket implementations"
Claude: [Searches across ALL your projects]
```

### Key Behaviors
1. **Current Project First**: Searches your working directory's project by default
2. **Intelligent Cross-Project**: Claude suggests searching all projects when relevant
3. **Explicit Control**: Use "all projects" or "search everywhere" to override
4. **Direct Specification**: Name specific projects to search just those

### Why This Change?
- **Focused Results**: No more unrelated matches from other projects
- **Faster Searches**: Searching one collection vs 57+ collections
- **Better Context**: Results are always relevant to what you're working on
- **Privacy**: Projects remain isolated unless explicitly requested

## v2.4.0 - Docker Simplified Setup
**Released:** 2025-07-27

- Streamlined Docker Compose configuration
- Improved setup wizard with health checks
- Better error handling for existing installations

## v2.3.7 - Local Embeddings by Default
**Released:** 2025-07-25

- FastEmbed replaces Voyage AI as default for privacy
- Setup wizard improvements
- Security enhancements with automated scanning

## Earlier Versions
See [CHANGELOG.md](../CHANGELOG.md) for complete version history.