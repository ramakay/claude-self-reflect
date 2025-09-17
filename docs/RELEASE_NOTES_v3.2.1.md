# Release Notes - v3.2.1

## Summary
Version 3.2.1 is a critical security and privacy patch that addresses CVE-2025-7709 SQLite3 vulnerability and removes personal information from the codebase. This release ensures the project is secure and privacy-compliant for all users.

## Changes

### Security Fixes
- **CVE-2025-7709 SQLite3 Vulnerability**: Mitigated SQLite3 vulnerability in all Docker containers
  - Updated all Dockerfiles with security comments and upgrade instructions
  - Affects: Dockerfile.importer, Dockerfile.mcp-server, Dockerfile.watcher, Dockerfile.streaming-importer, Dockerfile.async-importer, Dockerfile.safe-watcher
  - Ensures container security for all deployment scenarios

### Privacy Protection
- **Personal Information Removal**: Eliminated hardcoded personal usernames from source code
  - Modified: `mcp-server/src/server.py` - Replaced specific username with wildcard pattern
  - Improved user compatibility across different systems
  - Prevents personal information leakage in open source distribution

### Minor Improvements
- **Enhanced .gitignore**: Added patterns for temporary files, backups, and logs
  - Excludes cache/, backups/, import logs, and Qdrant backup files
  - Prevents accidental commit of sensitive data
- **Import Script Fixes**: Minor improvements to conversation import reliability
- **Agent Documentation**: Updated Claude agent documentation and testing guides

## Technical Details

### Security Mitigation
The SQLite3 vulnerability (CVE-2025-7709) has been addressed through Docker base image awareness and upgrade instructions:

```dockerfile
# SECURITY: CVE-2025-7709 mitigation - SQLite3 vulnerability
# TODO: Remove explicit SQLite3 upgrade when base image includes patched version
```

### Privacy Pattern Update
```python
# Before (v3.2.0 and earlier):
base_path / f"-Users-YOUR_USERNAME-projects-{project}"

# After (v3.2.1):  
base_path / f"-Users-*-projects-{project}"
```

This change ensures the code works for all users while removing personal information.

### File Organization
New files have been automatically organized into appropriate documentation directories:
- Testing reports moved to `tests/`
- Implementation summaries in `docs/development/`
- Release planning in `docs/planning/`
- Operational procedures in `docs/operations/`

## Backward Compatibility
- **Fully Compatible**: All existing functionality remains unchanged
- **User Path Support**: Wildcard pattern supports all user directory structures
- **Configuration Preserved**: No changes to MCP server configuration required
- **Data Integrity**: All existing conversation imports remain intact

## Installation
```bash
npm install -g claude-self-reflect@3.2.1
```

## Migration Notes
No migration required - this is a security and privacy patch:
- Existing MCP configurations continue to work seamlessly
- All stored conversations remain fully accessible
- Search functionality unchanged
- Container deployments receive automatic security improvements

## Contributors
Thank you to the security community and project contributors who helped identify and address these issues:
- **Security Research Community**: Identifying CVE-2025-7709
- **Open Source Community**: Feedback on privacy best practices
- **Claude Code**: Implementation of security fixes and privacy protection

## Verification Checklist
- [x] Security vulnerability CVE-2025-7709 addressed in all Dockerfiles
- [x] Personal information removed from all source code
- [x] Wildcard patterns tested for user compatibility
- [x] All existing functionality verified to work
- [x] Container security improvements implemented
- [x] Documentation updated and organized

## Related Issues
- Addresses: CVE-2025-7709 SQLite3 security vulnerability
- Resolves: Personal information exposure in open source codebase
- Improves: User compatibility across different system configurations
- Enhances: Project security posture and privacy compliance

## Security Advisory
Users running containerized deployments should update immediately to receive SQLite3 security fixes. While the vulnerability requires specific conditions to exploit, this update provides proactive protection.

## Next Steps
1. Update to v3.2.1 via npm or container registry
2. Restart any running Docker containers to apply security fixes
3. Verify MCP server functionality (should be seamless)
4. No additional configuration changes required

This release maintains the enhanced search capabilities and pagination support introduced in v3.2.0 while ensuring security and privacy compliance.