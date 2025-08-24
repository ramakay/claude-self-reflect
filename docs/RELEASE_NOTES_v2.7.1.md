# Release Notes - v2.7.1

**Updated: 2025-08-24 | Version: 2.7.1**

## Summary

Version 2.7.1 addresses a critical bug in the MCP server's reflection storage and retrieval system that prevented agents from accessing insights stored by previous agents. This patch release ensures proper cross-agent continuity and knowledge sharing while maintaining full backward compatibility.

## Critical Bug Fix

### Reflections Not Searchable from Project Context

**Problem**: When working within a specific project context, agents could store reflections but subsequent agents couldn't retrieve them. This broke the core promise of continuous memory and cross-agent learning.

**Root Cause**: The project-scoped search logic excluded the global reflections collection, making stored insights invisible to future agents working in the same project.

**Solution**: 
- Enhanced search logic to always include the reflections collection when searching from project contexts
- Added project metadata to newly stored reflections for better organization
- Maintained backward compatibility with existing reflections lacking project metadata

**Impact**: Agents can now properly build upon each other's insights, maintaining true conversation continuity across sessions.

## Technical Improvements

### Enhanced Search Architecture
- **Dual Collection Queries**: Project searches now query both project-specific collections and the global reflections collection
- **Metadata Enrichment**: New reflections include project context for future filtering capabilities
- **Performance Optimized**: No performance degradation despite enhanced search scope

### Backward Compatibility
- **Legacy Support**: Existing reflections without project metadata remain fully searchable
- **Automatic Migration**: No user action required - fix is transparent and immediate
- **Schema Evolution**: Enhanced metadata schema supports both old and new reflection formats

## Files Modified

- **`mcp-server/src/server.py`**: Core search and storage logic enhancements
- **`CLAUDE.md`**: Updated documentation reflecting reflection improvements

## Installation

```bash
npm install -g claude-self-reflect@2.7.1
```

For existing installations, the MCP server will automatically use the enhanced logic after restart. No manual intervention required.

## Verification Steps

1. **Cross-Agent Testing**: Store a reflection in one conversation, then verify it's discoverable in a new conversation within the same project
2. **Project Isolation**: Confirm project-scoped searches still work correctly while including relevant reflections  
3. **Legacy Compatibility**: Verify existing reflections continue to function without issues

## Contributors

Thank you to the development team for identifying and resolving this critical issue that ensures the core reflection system works as designed.

## Related Issues

This release resolves the cross-agent reflection accessibility issue reported in internal testing, ensuring the memory system fulfills its promise of continuous learning across agent interactions.

---

**Next Release**: v2.7.2 will focus on documentation improvements and minor enhancements to the setup process.