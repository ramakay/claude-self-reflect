# Release Notes - v2.8.10

## Summary
This release fixes a critical Docker path normalization bug that was causing incorrect Qdrant collection names, preventing users from searching their conversations when using Docker-based imports.

## Critical Bug Fix

### Docker Path Normalization Issue
- **Impact**: Docker containers were creating wrong collection names due to `/logs/` mount path prefix not being handled
- **Affected Users**: ALL users running imports via Docker (both local FastEmbed and Voyage AI cloud)
- **Symptoms**: 
  - Collections created with incorrect hash-based names (e.g., `conv_22f17df6_local` instead of `conv_7f6df0fc_local`)
  - Users unable to search conversations imported through Docker
  - Search tools returning empty results despite successful imports

### Root Cause
Docker containers mount paths as `/logs/-Users-username-projects-projectname/` but the `normalize_project_name` function wasn't handling the `/logs/` prefix, causing different path normalization between Docker and local environments.

### Fix Applied
- **Modified Files**: 
  - `/scripts/utils.py` - Import scripts path normalization
  - `/mcp-server/src/utils.py` - MCP server path normalization
- **Solution**: Added Docker-specific path handling that detects `/logs/` prefix and processes the directory name correctly
- **Code Change**:
  ```python
  # Check if this is a Docker mount path specifically
  if str(path_obj).startswith("/logs/") and path_obj.name.startswith("-"):
      return normalize_project_name(path_obj.name, _depth + 1)
  ```

## Technical Improvements
- Added recursion depth protection to prevent infinite loops during path processing
- Improved path normalization consistency across different environments
- Enhanced error handling for malformed path inputs

## Impact Assessment
- **Critical**: Fixes search functionality for Docker users
- **Compatibility**: No breaking changes - existing collections remain functional
- **Migration**: Users may see duplicate collections temporarily until re-import or cleanup

## User Action Required
### For Users Experiencing Search Issues:
1. **Immediate Fix**: Update to v2.8.10
   ```bash
   npm install -g claude-self-reflect@2.8.10
   ```

2. **Re-import Affected Conversations**: 
   - Docker users should re-run imports to create collections with correct names
   - Old collections with wrong names can be safely deleted from Qdrant

3. **Verify Fix**:
   - Check Qdrant collections match your project names
   - Test MCP search functionality in Claude

### For New Installations:
No action required - the fix is included by default.

## Installation
```bash
npm install -g claude-self-reflect@2.8.10
```

## Contributors
Thank you to the community for reporting Docker-related search issues that led to discovering this critical path normalization bug.

## Related Issues
- Resolves Docker path normalization causing wrong collection names
- Fixes search functionality for containerized environments
- Addresses consistency issues between local and Docker deployments

## Verification Steps
After updating:
1. Check that new imports create correctly named collections
2. Verify MCP search returns expected results
3. Confirm both local and Docker environments use consistent collection naming

## Support
If you continue experiencing search issues after this update, please:
1. Check your Qdrant collections for correct naming
2. Re-run imports if necessary
3. Report any remaining issues on GitHub

This release ensures reliable conversation search across all deployment methods.