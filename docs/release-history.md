# Release History

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