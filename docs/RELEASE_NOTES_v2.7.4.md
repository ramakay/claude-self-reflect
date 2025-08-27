# Release Notes - Claude Self Reflect v2.7.4

**Updated: August 27, 2025 | Version: 2.7.4**

## Critical Bug Fix Release

This release addresses a critical collection name normalization issue that prevented metadata extraction functionality for new installations. The bug was blocking access to advanced search features including `search_by_file` and `search_by_concept` for users installing the system after v2.7.3.

## Release Notes

### Critical Bug Fixes

**Collection Name Mismatch Resolution**
- **Fixed collection naming inconsistency** between import scripts and delta metadata update processes
- **Removed hardcoded username dependency** from path normalization function that caused collection names to hash differently across execution contexts
- **Standardized collection identification** to ensure consistent naming across all components

**Docker Environment Improvements**
- **Added LOGS_DIR environment variable** to Docker Compose configuration for improved file discovery
- **Enhanced state file path detection** to properly handle both Docker containerized and local execution environments
- **Fixed state dictionary initialization** with correct default data types to prevent runtime errors

**Code Quality Improvements**
- **Removed hardcoded user paths** from test files to improve portability and prevent environment-specific failures
- **Cleaned up legacy test artifacts** that contained system-specific configurations

### Technical Details

The primary issue involved a path normalization function that inadvertently included hardcoded usernames, causing the MD5 hash generation for collection names to produce different results between the import process and metadata update process. This mismatch prevented the delta metadata update system from locating the correct collections, effectively disabling advanced search capabilities.

**Files Modified:**
- `scripts/delta-metadata-update-safe.py`: Fixed collection name normalization logic
- `scripts/import-conversations-unified.py`: Aligned collection naming with delta update process
- `docker-compose.yaml`: Added LOGS_DIR environment variable configuration
- Various test files: Removed hardcoded system paths

**Impact Resolution:**
- New installations now correctly process metadata extraction
- Advanced search features (`search_by_file`, `search_by_concept`) function as intended
- Docker deployments properly discover conversation files
- State management works consistently across execution environments

### Breaking Changes

None. This release maintains full backward compatibility with existing installations.

### Contributors

Special recognition to **@billzajac** for identifying and resolving this critical collection naming issue through PR #44. This contribution ensures that advanced search functionality works correctly for all new installations.

## Installation and Upgrade

### New Installations
```bash
npm install -g claude-self-reflect@2.7.4
```

### Existing Users
Users upgrading from previous versions will automatically benefit from the collection naming fix. No manual intervention required.

### Verification
To verify that metadata extraction is working correctly:

```bash
# Check that collections are being created with consistent names
python scripts/check-collections.py

# Verify metadata update process can locate collections
LIMIT=1 DRY_RUN=true python scripts/delta-metadata-update-safe.py
```

## Documentation and Support

- **GitHub Repository**: https://github.com/ramakay/claude-self-reflect
- **Issue Tracker**: https://github.com/ramakay/claude-self-reflect/issues  
- **NPM Package**: https://www.npmjs.com/package/claude-self-reflect
- **License**: MIT

For technical support or questions about this release, please open an issue on the GitHub repository.