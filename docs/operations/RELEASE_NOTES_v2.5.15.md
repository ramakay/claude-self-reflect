# Claude Self-Reflect v2.5.15 Release Notes

## 🐛 Critical Fix: Project-Specific Search Resolution

### Issue Fixed
Project-specific searches were returning no results due to a collection naming mismatch between import and search operations.

### Root Cause
- Import scripts used raw project path formats from conversation logs
- MCP server expected normalized project names when searching
- This caused MD5 hash mismatches, resulting in searches looking in non-existent collections

### Solution Implemented

#### Updated Components
1. **fix-misplaced-conversations.py**
   - Now uses `normalize_project_name()` for consistent collection naming
   - Automatically migrates existing misplaced conversations

2. **streaming-importer.py**
   - Normalizes project names before storing in payloads
   - Ensures new imports use correct naming

3. **import-conversations-unified.py**
   - Updated to normalize project paths during import
   - Maintains consistency with MCP server expectations

### Migration Guide

#### For Existing Users
If you have existing data that needs migration:

```bash
# Activate virtual environment
source venv/bin/activate  # or source .venv/bin/activate

# Run migration script
python scripts/fix-misplaced-conversations.py --fix

# Restart MCP server (required)
claude mcp remove claude-self-reflect
# Restart Claude Code
claude mcp add claude-self-reflect "/path/to/run-mcp.sh" \
  -e QDRANT_URL="http://localhost:6333" -s user
```

#### Verification
Test your project-specific searches:
```bash
# Example: Search within a specific project
mcp__claude-self-reflect__reflect_on_past
  query: "your search terms"
  project: "your-project-name"
```

### Testing
- Comprehensive test suite added: `scripts/test-search-functionality.py`
- Validates searches across multiple projects
- Ensures collection naming consistency
- Tests both project-specific and cross-project searches

### Impact
- ✅ Fixes all project-specific search failures
- ✅ No data loss during migration
- ✅ Backwards compatible with existing data
- ✅ Performance unchanged (~100ms for project searches)

### Technical Details

The `normalize_project_name()` function ensures consistent naming by:
- Removing path prefixes from Claude conversation logs
- Extracting the actual project name
- Creating consistent MD5 hashes for collection names

Example transformation:
```
Input:  -Users-username-projects-my-project
Output: my-project
Hash:   abc12345 → conv_abc12345_local
```

### Upgrade Instructions

1. Pull latest changes
2. Run migration script if you have existing data
3. Restart MCP server
4. Verify searches work correctly

### Breaking Changes
None - the fix is backwards compatible and includes automatic migration.

### Contributors
Thank you to the community for identifying and helping diagnose this issue.

---

**Version**: 2.5.15  
**Release Date**: August 2025  
**Type**: Bug Fix  
**Priority**: High