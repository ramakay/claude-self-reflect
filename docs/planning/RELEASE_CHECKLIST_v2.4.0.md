# Release Checklist for v2.4.0

## Pre-Release (COMPLETED)
- [x] Run security scan with pip-audit - **No vulnerabilities found**
- [x] Test Docker volume migration - **Works correctly**
- [x] Test both local and Voyage AI embeddings - **Both functional**
- [x] Update release notes with all changes
- [x] Credit contributors (@jmherbst for PR #15)
- [x] Commit all changes to feature branch
- [x] Push to remote
- [x] Update PR #12 with v2.4.0 scope

## Release Process (PENDING USER APPROVAL)

### Step 1: Merge PR #12
```bash
# After PR approval, merge to main
gh pr merge 12 --merge
```

### Step 2: Switch to main and pull
```bash
git checkout main
git pull origin main
```

### Step 3: Create and push tag
```bash
git tag -a v2.4.0 -m "Release v2.4.0 - Docker volumes, Voyage AI fixes, and enhanced testing"
git push origin v2.4.0
```

### Step 4: Create GitHub release
```bash
gh release create v2.4.0 \
  --title "v2.4.0 - Docker Volumes & Enhanced Testing" \
  --notes-file docs/RELEASE_NOTES_v2.4.0.md \
  --target main
```

### Step 5: Update npm package (if applicable)
```bash
# Update version in package.json
npm version 2.4.0 --no-git-tag-version
git add package.json
git commit -m "chore: bump version to 2.4.0"
git push origin main

# Publish to npm
npm publish
```

### Step 6: Post-Release
- [ ] Announce in Discord/community channels
- [ ] Update README.md badges if needed
- [ ] Close related issues (#11, #14, #15, #16)
- [ ] Thank contributors in community channels

## Notes
- PR #12 is ready for review at: https://github.com/ramakay/claude-self-reflect/pull/12
- All tests have passed
- Security scan shows no vulnerabilities
- Docker volume migration tested successfully
- Both embedding modes (local & Voyage) working correctly