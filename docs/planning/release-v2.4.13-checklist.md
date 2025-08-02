# Release v2.4.13 Checklist

## Pre-Release
- [x] Check current version: `gh release list` - Latest is v2.4.12
- [ ] Resolve all merge conflicts
- [ ] Security scan passes
- [ ] All CI/CD checks green
- [ ] Contributors acknowledged

## Release Steps
- [ ] Commit changes to main
- [ ] Tag created and pushed
- [ ] GitHub release created
- [ ] NPM package published (automated)
- [ ] Issue #13 closed with explanation

## Verification
- [ ] GitHub release visible
- [ ] NPM package updated
- [ ] No rollback needed

## Changes in this release:
- Fixed Dockerfile.importer to embed Python dependencies directly
- Removed COPY scripts line that causes failures with global npm installs
- This fixes the "requirements.txt not found" error reported in issue #13