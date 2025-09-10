# Release Notes - v3.2.2

**Version**: 3.2.2  
**Date**: September 10, 2025  
**Type**: Patch Release - CodeQL Security Scan Fixes

## Summary

This patch release resolves Python syntax errors detected by GitHub's CodeQL security scanning, ensuring clean security reports and improved code quality.

## Fixed Issues

### Python Syntax Errors (CodeQL)
- **process-staged-imports.py**: Fixed indentation error after `try` statement (line 290)
- **optimize-search-thresholds.py**: 
  - Removed non-UTF8 characters causing encoding errors
  - Fixed multiple unterminated f-string literals
  - Added missing `except` block in error handling
  - Cleaned up duplicate lines from previous fixes

## Technical Details

The CodeQL scanner identified syntax errors that prevented proper static analysis of these Python scripts. While the scripts may have functioned in some environments, the syntax issues could cause:
- Import failures in strict Python environments
- Incomplete security scanning coverage
- Potential runtime errors with certain Python versions

## Testing

All Python scripts in the `scripts/` directory now pass validation:
```bash
python -m py_compile scripts/*.py  # All files compile successfully
```

## Migration Guide

No migration required - this is a syntax-only fix with no functional changes.

## Compatibility

- Fully backward compatible with v3.2.0 and v3.2.1
- No API changes
- No configuration changes required

---

**Note**: This is a maintenance release focused on code quality. Users can upgrade at their convenience.