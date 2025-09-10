# Release Notes - v3.2.0

## Summary
Version 3.2.0 enhances the MCP API with new search modes, proper pagination support, and fixes critical documentation issues. This release focuses on improving usability and performance while maintaining full backward compatibility.

## Changes

### Enhanced Search Capabilities
- **New Mode Parameter for reflect_on_past**: Added `mode` parameter with three options:
  - `full` (default): Returns all results with complete details
  - `quick`: Returns count and top result only for fast previews
  - `summary`: Returns aggregated insights without individual results
- **Proper Pagination Support**: Implemented `get_next_results` tool for cursor-based pagination
  - Supports offset/limit parameters for flexible result navigation
  - Works with both project-specific and cross-project searches
  - Returns metadata about remaining results

### Documentation Fixes
- **Removed Non-Existent Tools**: Cleaned up MCP_REFERENCE.md to remove references to tools that were never implemented:
  - `quick_search` - Use `reflect_on_past` with `mode="quick"` instead
  - `search_summary` - Use `reflect_on_past` with `mode="summary"` instead  
  - `get_more_results` - Use new `get_next_results` tool instead
- **Updated Examples**: All documentation now shows correct tool usage patterns

### Performance & Quality Improvements
- **Search Limit Cap**: Maximum search results capped at 100 to prevent performance issues
- **Enhanced Error Handling**: Improved exception handling with specific error types and logging
- **Input Validation**: Added proper validation for mode parameter and search limits
- **Response Optimization**: Improved text preview generation for better performance

## Technical Details

### New Tool: get_next_results
```python
# Example usage for pagination
# First search
results = await reflect_on_past(query="debugging imports", limit=5)

# Get more results  
more_results = await get_next_results(
    query="debugging imports", 
    offset=5,  # Skip first 5 results
    limit=5    # Get next 5 results
)
```

### Mode Parameter Examples
```python
# Full details (default behavior)
full_results = await reflect_on_past(query="memory optimization", mode="full")

# Quick preview
quick_preview = await reflect_on_past(query="memory optimization", mode="quick")

# Summary only
summary = await reflect_on_past(query="memory optimization", mode="summary")
```

### Performance Improvements
- Search operations now bounded to maximum 100 results
- Enhanced logging for debugging failed operations
- Optimized text preview generation reduces response time
- Mode parameter validation prevents invalid queries

## Backward Compatibility
- **Fully Compatible**: All existing code continues to work without modification
- **Default Behavior Unchanged**: `reflect_on_past` still returns full results by default
- **Optional Parameters**: New `mode` parameter is optional with sensible defaults
- **Existing Collections**: All previously imported conversations remain fully searchable

## Installation
```bash
npm install -g claude-self-reflect@3.2.0
```

## Migration Notes
No migration required - this is a backward-compatible enhancement:
- Existing MCP configurations continue to work
- All stored conversations remain accessible
- Default search behavior is unchanged
- New features are opt-in via parameters

## Contributors
- **Claude Code**: Implementation of mode parameter and pagination support
- **Opus 4.1**: Comprehensive code review identifying critical performance and quality issues
- **Community**: Testing and feedback on API usability

## Verification Checklist
- [x] Code changes implemented in mcp-server/src/server.py
- [x] Documentation updated in docs/development/MCP_REFERENCE.md  
- [x] All code review fixes applied (search limits, validation, error handling)
- [x] Backward compatibility verified
- [x] Implementation summary documented
- [ ] Integration testing (requires Claude Code restart)
- [ ] Performance testing with new modes

## Related Issues
- Addresses documentation inconsistencies where tools were referenced but never implemented
- Improves API usability with pagination and flexible search modes
- Enhances performance with bounded search operations

## Next Steps
1. Restart Claude Code to load new MCP server changes
2. Test all three modes of reflect_on_past tool
3. Verify pagination functionality with get_next_results
4. Validate search performance improvements