# Release v2.4.7 - Critical Local Mode Fix

## Overview
This release fixes a critical issue preventing local mode from working properly. Users can now use Claude Self Reflect with local embeddings (FastEmbed) without requiring a Voyage API key.

## 🐛 Bug Fixes

### Fixed Local Mode Setup (Issue #21)
- **Problem**: Setup wizard was trying to use obsolete `server_v2.py` module which required Voyage API
- **Solution**: Updated to use correct server module (`python -u -m src`)
- **Impact**: Local embeddings now work properly without Voyage API key requirement

### Technical Changes
- Updated `installer/setup-wizard-docker.js` to use correct Python module path
- Updated `mcp-server/run-mcp-docker.sh` with proper module reference
- Removed obsolete `mcp-server/src/server_v2.py` that was causing confusion
- Version bump in `package.json` to 2.4.7

## 🙏 Acknowledgments
Thank you to the community for reporting this issue and helping us maintain a smooth local-first experience!

## Installation
```bash
npm install -g claude-self-reflect@2.4.7
```

## Verification
After installation, you can verify local mode works without Voyage API:
```bash
# Run setup wizard
claude-self-reflect setup

# Choose local embeddings when prompted
# No Voyage API key required!
```

---
*Released on January 29, 2025*