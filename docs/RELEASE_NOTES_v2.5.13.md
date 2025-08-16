# Release Notes - v2.5.13

**Updated: August 16, 2025 | Version: 2.5.13**

## Release Notes

### Bug Fixes

**npm Package Installation Issue (Issue #35)**
- Fixed missing `config/qdrant-config.yaml` file in npm package distribution
- Added `config/qdrant-config.yaml` to package.json `files` array
- Resolves Docker mount failures during setup wizard execution
- Ensures proper Qdrant configuration availability in global npm installations

**Docker Path Normalization Enhancement**
- Implemented `normalize_file_path` function in status reporting module
- Fixes path resolution discrepancies in Docker containerized environments
- Improves accuracy of file status reporting across different deployment contexts
- Addresses Docker path mapping inconsistencies for better operational visibility

### Technical Specifications

**Files Modified:**
- `package.json`: Added config file to distribution manifest
- `mcp-server/src/status.py`: Enhanced Docker path handling with normalization function

**Compatibility:**
- Docker Compose: Full compatibility maintained
- npm Global Installation: Issue resolved for all platforms
- MCP Integration: No breaking changes to existing tool interfaces

### Installation

```bash
npm install -g claude-self-reflect@2.5.13
```

### Verification

After installation, verify the setup wizard executes without Docker mount errors:

```bash
claude-self-reflect setup
```

The setup process should complete successfully without encountering "unable to mount" errors related to configuration files.

### Contributors

Special thanks to @dantodor for the detailed bug report and system configuration details that enabled rapid diagnosis and resolution of the npm installation issue.

### Documentation

- [Installation Guide](https://github.com/ramakay/claude-self-reflect#installation)
- [Docker Setup Documentation](https://github.com/ramakay/claude-self-reflect/blob/main/docs/DOCKER.md)
- [MCP Integration Reference](https://github.com/ramakay/claude-self-reflect/blob/main/docs/development/MCP_REFERENCE.md)

### Support

- **Issues**: [GitHub Issues](https://github.com/ramakay/claude-self-reflect/issues)
- **Discussions**: [GitHub Discussions](https://github.com/ramakay/claude-self-reflect/discussions)
- **Documentation**: [Project Wiki](https://github.com/ramakay/claude-self-reflect/wiki)

## Related Issues

- Resolves #35: Global npm installation Docker mount failures