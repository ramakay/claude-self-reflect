# v2.4.5 - Performance Revolution

## 🚀 10-40x Performance Improvement

This release brings massive performance improvements to Claude Self Reflect, making searches virtually instantaneous while maintaining high-quality results.

### Key Improvements

#### ⚡ Speed Enhancements
- **End-to-end response time**: Reduced from 28.9s-2min to just **200-350ms**
- **Search latency**: Optimized to 103-620ms (varies by collection count)
- **Response size**: Reduced by 40-60% through intelligent compression

#### 🎯 Fidelity Restored
- **Default 5 results**: Maintains comprehensive search coverage
- **350-character excerpts**: Optimal balance of context and performance
- **Improved relevance**: Better title and key-finding extraction

### New Features

#### Debug Mode
```bash
# Enable raw data output for troubleshooting
include_raw=true
```

#### Response Formats
```bash
# Choose between XML (default) or markdown
response_format=xml    # Structured data
response_format=markdown  # Human-readable
```

#### Brief Mode
```bash
# Get minimal responses for even faster results
brief=true  # 60% smaller responses
```

#### Progress Reporting
- Real-time search progress updates (requires client progressToken)
- Detailed timing breakdown in debug logs

### Technical Details

- **XML tag compression**: 40% payload reduction using single-letter tags
- **Timezone handling**: Fixed datetime comparison issues
- **Streaming support**: Works properly with reflection-specialist agent
- **MCP overhead**: Reduced from 85% to manageable levels

### Known Issues

- Specialized search tools (`quick_search`, `search_summary`, `get_more_results`) work through the reflection-specialist agent but not via direct MCP calls due to FastMCP limitations

### What's Next

We're continuing to optimize performance while exploring ways to enable the specialized search tools directly through MCP. The current workaround via the reflection-specialist agent provides full functionality.

### Contributors

Thank you to everyone who provided feedback on search performance and helped test these improvements!

---

**Full Changelog**: https://github.com/ramakay/claude-self-reflect/compare/v2.4.4...v2.4.5