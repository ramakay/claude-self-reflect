# Release v2.4.13 - Docker Global NPM Installation Fix

This patch release fixes a critical issue where global npm installations would fail during Docker builds due to missing requirements.txt file.

## 🐛 Bug Fixes

### Fixed Docker Build for Global NPM Installations
- **Issue**: When installing via `npm install -g claude-self-reflect`, the Docker importer would fail with "requirements.txt not found" error
- **Root Cause**: The Dockerfile was trying to COPY a file that doesn't exist in the npm package distribution
- **Solution**: Embedded Python dependencies directly in the Dockerfile instead of copying requirements.txt
- **Impact**: Global npm installations now work correctly with Docker setup

## 📝 Technical Details

The Dockerfile.importer has been updated to:
1. Install Python dependencies directly using `pip install` with explicit versions
2. Remove the problematic `COPY scripts/requirements.txt` line
3. Add comments explaining the volume mounting approach for scripts

This ensures compatibility with both:
- Local development (where scripts/ directory exists)
- Global npm installations (where only published files are available)

## 🙏 Acknowledgments

Special thanks to:
- @mattias012 for reporting issue #13 and providing detailed error logs
- @vbp1 for additional testing and feedback

## 📦 Installation

To upgrade to this version:

```bash
# For global installation
sudo npm install -g claude-self-reflect@2.4.13

# For local installation
npm install claude-self-reflect@2.4.13
```

## 🔧 Compatibility

- No breaking changes
- Backward compatible with existing installations
- Docker Compose configuration remains unchanged

## 📚 Related Issues

- Fixes #13: Docker compose requirements.txt not found error

---

For more information, visit our [GitHub repository](https://github.com/ramakay/claude-self-reflect)