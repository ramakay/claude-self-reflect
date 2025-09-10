# Release Notes - v3.0.2

## Summary
Critical Windows compatibility fix and significant package size reduction. This release resolves Windows ESM import errors that were preventing successful installation and reduces npm package size by ~90MB.

## Changes

### Bug Fixes
- **Windows ESM Import Fix (Issue #51)**: Fixed Windows installation failure due to ESM dynamic import error
  - Added `pathToFileURL()` wrapper for dynamic imports in `installer/setup-wizard.js`
  - Ensures proper file URL conversion on Windows paths
  - Resolves "ERR_UNSUPPORTED_ESM_URL_SCHEME" errors during setup

### Package Optimization
- **Significant Package Size Reduction**: Removed .fastembed-cache from source control
  - Deleted 25 cached embedding model files (~90MB total)
  - Added `.fastembed-cache/` to .gitignore
  - Reduces npm package download time and storage requirements
  - Cache will be regenerated locally during first use

### Technical Details
- **Files Modified**:
  - `installer/setup-wizard.js`: Added pathToFileURL for Windows compatibility
  - `.gitignore`: Added .fastembed-cache exclusion
  - Removed: 25 cached model files from .fastembed-cache/

- **Impact**: 
  - Windows users can now install successfully
  - Significantly faster npm package downloads
  - First-run setup will download embeddings as needed

## Installation
```bash
npm install -g claude-self-reflect@3.0.2
```

## Windows Users
This release specifically addresses the Windows installation issue reported in #51. Windows users should now be able to complete the setup wizard without ESM import errors.

## Contributors
Thank you to the community members who reported Windows installation issues and helped identify the root cause.

## Related Issues
- Resolves #51: Windows ESM import error during installation
- Addresses package size concerns from community feedback

## Verification Steps
1. Windows installation completes without ESM errors
2. Setup wizard runs successfully on Windows
3. Package size significantly reduced
4. Embedding models download correctly on first use