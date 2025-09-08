# Critical Issues Found in v3.0.0 Release

## Summary
The v3.0.0 release has critical missing components that prevent the modular architecture from working for new users.

## Issues Identified

### 1. ❌ Modular Importer Directory Not Included in NPM Package
**Problem**: The `scripts/importer/` directory was missing from package.json files array
**Impact**: New users installing via npm would not get the modular architecture
**Fix**: Added `"scripts/importer/**/*.py"` to package.json

### 2. ❌ Missing dependency-injector Package
**Problem**: The modular architecture requires dependency-injector but it wasn't in requirements
**Impact**: Import would fail with ImportError
**Fix**: Added `"dependency-injector>=4.0.0,<5.0.0"` to pyproject.toml

### 3. ❌ No Entry Point for Modular System
**Problem**: No __main__.py or main() function to run the modular importer
**Impact**: Users couldn't execute the new modular system
**Fix**: Created __main__.py and added main() function

### 4. ⚠️ Docker Files Still Reference Old Script
**Problem**: Docker configurations still use import-conversations-unified.py
**Impact**: Docker users would use old monolithic version
**Status**: Created compatibility wrapper, full fix pending

### 5. ⚠️ Setup Wizard May Reference Old Scripts
**Problem**: Setup wizard may still reference old import scripts
**Impact**: New installations might use old version
**Status**: Needs investigation

## Files Modified for v3.0.1

1. **package.json** - Added importer directory to files array
2. **mcp-server/pyproject.toml** - Added dependency-injector
3. **scripts/importer/__main__.py** - Created entry point
4. **scripts/importer/main.py** - Added main() function
5. **scripts/import-modular.py** - Created wrapper script
6. **scripts/import-conversations-unified-v3.py** - Compatibility wrapper

## Testing Required

- [ ] Fresh npm install
- [ ] Docker container build
- [ ] Setup wizard flow
- [ ] Import with modular system
- [ ] Voyage AI imports

## Recommended Actions

1. **Immediate**: Release v3.0.1 hotfix with these changes
2. **Short-term**: Update all Docker files to use modular system
3. **Medium-term**: Deprecate old monolithic scripts
4. **Long-term**: Remove backward compatibility wrappers

## Version Bump

Should be v3.0.1 (patch release) as this fixes critical bugs without adding features.