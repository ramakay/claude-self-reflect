# Contributing & Release Process

## Contributing

See our [Contributing Guide](../CONTRIBUTING.md) for development setup and guidelines.

## Releasing New Versions (Maintainers)

Since our GitHub Actions automatically publish to npm, the release process is simple:

### Prerequisites
```bash
# 1. Ensure you're logged into GitHub CLI
gh auth login  # Only needed first time
```

### Release Process

```bash
# 2. Create and push a new tag
git tag v2.3.0  # Use appropriate version number
git push origin v2.3.0

# 3. Create GitHub release (this triggers npm publish)
gh release create v2.3.0 \
  --title "Release v2.3.0" \
  --notes-file CHANGELOG.md \
  --draft=false

# The GitHub Action will automatically:
# - Build the package
# - Run tests
# - Publish to npm
# - Update release assets
```

Monitor the release at: https://github.com/ramakay/claude-self-reflect/actions

### Version Numbering

Follow semantic versioning:
- **Major** (X.0.0): Breaking changes
- **Minor** (0.X.0): New features, backward compatible
- **Patch** (0.0.X): Bug fixes, backward compatible

### Pre-Release Checklist

1. **Update version** in package.json
2. **Update CHANGELOG.md** with release notes
3. **Run tests** locally: `npm test`
4. **Test Docker build**: `docker compose build`
5. **Verify MCP functionality** with Claude Desktop

### Post-Release

1. **Verify npm package**: `npm view claude-self-reflect version`
2. **Test installation**: `npm install -g claude-self-reflect@latest`
3. **Update documentation** if needed
4. **Announce** in discussions/Discord if significant release

### Rollback Procedure

If a release has issues:

```bash
# 1. Delete the problematic release
gh release delete vX.Y.Z --yes

# 2. Delete the tag
git tag -d vX.Y.Z
git push origin :refs/tags/vX.Y.Z

# 3. Unpublish from npm (within 72 hours)
npm unpublish claude-self-reflect@X.Y.Z

# 4. Fix issues and create new patch version
```

### GitHub Actions

Our CI/CD pipeline automatically:
1. Runs tests on push to main
2. Publishes to npm when a release is created
3. Builds and tests Docker images
4. Runs security scans with Snyk

The npm publish secret is stored as `NPM_TOKEN` in repository secrets.