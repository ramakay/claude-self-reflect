# Release Notes - v3.0.1

## Summary
Critical hotfix addressing npm package installation failures in v3.0.0. This release fixes missing modular importer components that prevented successful global installations.

## Changes

### Critical Fixes
- **Fixed npm package installation**: Added missing `scripts/importer/` directory to npm package files array
- **Added missing Python dependency**: Added dependency-injector to pyproject.toml requirements  
- **Created proper entry points**: Added __main__.py entry point for modular importer CLI execution
- **Enhanced backward compatibility**: Added main() function to importer/main.py with compatibility wrappers

### Technical Details
- **Files modified**:
  - `package.json`: Added `scripts/importer/**/*.py` to files array
  - `pyproject.toml`: Added dependency-injector to requirements
  - `scripts/importer/__main__.py`: New entry point for CLI execution
  - `scripts/importer/main.py`: Added main() function for compatibility
- **Root cause**: v3.0.0 modular architecture created new importer/ subdirectory but npm package excluded it
- **Impact**: All new global installations of v3.0.0 failed with "ModuleNotFoundError: No module named 'importer'"

### Verification
- Package files array now includes all modular importer components
- Global npm installation tested successfully
- Modular import system fully functional with dependency injection
- Backward compatibility maintained for existing workflows

## Installation
```bash
npm install -g claude-self-reflect@3.0.1
```

## Migration from v3.0.0
If you installed v3.0.0 and encountered import failures:
1. Uninstall: `npm uninstall -g claude-self-reflect`
2. Reinstall: `npm install -g claude-self-reflect@3.0.1`
3. Run setup wizard: `claude-self-reflect`

## Contributors
Thank you to the community for reporting the critical installation issue that led to this rapid hotfix release.

## Related Issues
- Resolves critical npm package installation failures in v3.0.0
- Fixes "ModuleNotFoundError: No module named 'importer'" errors
- Ensures modular architecture components are properly distributed