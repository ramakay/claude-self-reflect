# NPM Packaging Checklist

This checklist prevents packaging issues like #71 where refactored modules were missing from the npm package.

## Quick Reference

**When adding new files to `scripts/`:**
1. Add to `package.json` `"files"` array immediately
2. Update `tests/test_npm_package_contents.py` if needed
3. Run `npm pack --dry-run` locally to verify
4. CI/CD will automatically test on every commit

---

## Developer Checklist

Use this checklist when adding new Python modules, scripts, or resources that should be packaged.

### Adding New Scripts

- [ ] **Add to package.json**
  ```json
  "files": [
    "scripts/your-new-module.py",
    ...
  ]
  ```

- [ ] **Verify locally**
  ```bash
  npm pack --dry-run | grep "your-new-module"
  ```

- [ ] **Run packaging test**
  ```bash
  python tests/test_npm_package_contents.py
  ```

- [ ] **Update test if needed**
  - Add to `REQUIRED_MODULES` in `tests/test_npm_package_contents.py` if critical

### Adding New Directories

- [ ] **Add glob pattern to package.json**
  ```json
  "files": [
    "your-directory/**/*.py",
    ...
  ]
  ```

- [ ] **Verify all files included**
  ```bash
  npm pack --dry-run | grep "your-directory/"
  ```

---

## PR Review Checklist

Reviewers should verify:

### For PRs Adding New Files

- [ ] **package.json updated**
  - Check `"files"` array includes new modules
  - Verify glob patterns are correct

- [ ] **CI/CD test passing**
  - `npm-package-test` job must pass
  - Check test output for new files

- [ ] **CodeRabbit approval**
  - Wait for automated review
  - No packaging-related warnings

### For Refactoring PRs

- [ ] **All refactored modules in package.json**
  - If splitting files: add each new file
  - If renaming: update old name to new name
  - If moving: update paths

- [ ] **Test coverage for new modules**
  - Unit tests exist and pass
  - Import tests verify module loading

---

## CI/CD Checks

Automated checks run on every commit:

### 1. NPM Package Structure Test
```bash
npm pack --dry-run
node installer/cli.js help
```
- Verifies package.json is valid
- Tests CLI loads correctly

### 2. NPM Package Contents Test
```bash
python3 tests/test_npm_package_contents.py
```
- Parses `npm pack --dry-run` output
- Verifies all required modules present
- Fails build if files missing

**CI Location**: `.github/workflows/ci.yml` → `npm-package-test` job

---

## Pre-Release Checklist

Use before creating a release:

### Manual Verification

- [ ] **Run full packaging test**
  ```bash
  python tests/test_npm_package_contents.py
  ```

- [ ] **Check package size**
  ```bash
  npm pack
  ls -lh claude-self-reflect-*.tgz
  # Should be ~200-400KB
  ```

- [ ] **Test local installation**
  ```bash
  npm install -g ./claude-self-reflect-*.tgz
  claude-self-reflect --version
  claude-self-reflect setup --help
  ```

- [ ] **Verify critical files**
  ```bash
  tar -tzf claude-self-reflect-*.tgz | grep -E "(metadata_extractor|message_processors|import_strategies|embedding_service|doctor)"
  ```

### Post-Release Verification

- [ ] **npm registry check**
  ```bash
  npm view claude-self-reflect@latest
  npm view claude-self-reflect@latest dist.tarball
  ```

- [ ] **Fresh install test**
  ```bash
  npm uninstall -g claude-self-reflect
  npm install -g claude-self-reflect@latest
  claude-self-reflect setup
  ```

---

## Common Issues & Solutions

### Issue: "Module not found" after npm install

**Symptom**: Users report `ModuleNotFoundError` for a Python module

**Cause**: Module not in `package.json` "files" array

**Fix**:
1. Add module to `package.json` "files" array
2. Create hotfix release (bump patch version)
3. Notify affected users in issue/discussion

**Example**: Issue #71, fixed in v5.0.5

### Issue: Package too large

**Symptom**: npm pack creates >1MB tarball

**Cause**: Including unnecessary files (tests, docs, node_modules)

**Fix**:
1. Check `.npmignore` or `package.json` "files" array
2. Exclude test data, logs, temporary files
3. Use specific paths, not broad globs

### Issue: Missing entire directory

**Symptom**: `npm pack` doesn't include a directory

**Cause**: Directory not in "files" array or .npmignore excludes it

**Fix**:
1. Add directory glob to package.json:
   ```json
   "files": ["missing-dir/**/*.py"]
   ```
2. Check `.npmignore` doesn't exclude it
3. Verify no `.gitignore` conflicts

---

## Tool Reference

### Useful Commands

```bash
# Dry run - see what would be packaged
npm pack --dry-run

# See file sizes
npm pack --dry-run | grep "npm notice" | sort -k2 -h

# Extract specific file list
npm pack --dry-run 2>&1 | grep "scripts/" | cut -d' ' -f4

# Test local install without publishing
npm pack
npm install -g ./claude-self-reflect-*.tgz

# Compare with previous version
npm pack --dry-run > current.txt
npm view claude-self-reflect@5.0.4 dist.tarball | curl -sL - | tar -tzf - > previous.txt
diff current.txt previous.txt
```

### Test Files

- **Regression test**: `tests/test_npm_package_contents.py`
  - Runs on every CI build
  - Verifies 10 critical modules + 4 directories
  - References: Issue #71, Discussion #70

- **Refactoring tests**: `tests/test_import_refactoring.py`
  - 20 tests for refactored modules
  - Ensures modules are importable

### CI/CD Integration

- **Workflow**: `.github/workflows/ci.yml`
- **Job**: `npm-package-test`
- **Runs on**: Every push, every PR
- **Dependencies**: Requires Node.js 18.x or 20.x

---

## Who's Involved

Preventing packaging issues requires:

| Role | Responsibility | When |
|------|---------------|------|
| **Developer** | Add files to package.json when creating them | Adding new files |
| **CodeRabbit** | Flag PRs adding scripts without package.json updates | PR creation (automated) |
| **CI/CD** | Run packaging test on every commit | Every push (automated) |
| **CSR Validator** | Quick check before releases | Pre-release (agent) |
| **Open Source Maintainer** | Manual verification & smoke test | Release time (agent) |
| **Community** | Report issues if modules missing | Post-release |

---

## History

### Issue #71 (v5.0.4 → v5.0.5)

**Problem**: 5 refactored modules missing from npm package
- `metadata_extractor.py`
- `message_processors.py`
- `import_strategies.py`
- `embedding_service.py`
- `doctor.py`

**Impact**: 100% of v5.0.4 users affected (ModuleNotFoundError)

**Root Cause**: PR #69 refactored import script but didn't update package.json

**Prevention**:
- Created `tests/test_npm_package_contents.py` (regression test)
- Added test to CI/CD pipeline
- This checklist created

**Lessons Learned**:
1. Large refactorings need extra packaging attention
2. Automated testing catches issues before release
3. Developer checklists prevent mistakes
4. Fast hotfix releases minimize user impact

---

## Updates

- **v5.0.5 (2025-09-30)**: Initial checklist created after #71
- Document location: Root of repository
- Maintained by: Open Source Maintainer agent

---

## Quick Start

**For developers adding new files:**
1. Update `package.json` "files" array
2. Run `python tests/test_npm_package_contents.py`
3. Commit if test passes

**For reviewers:**
1. Check if PR adds new `scripts/*.py` files
2. Verify package.json updated
3. Wait for CI/CD `npm-package-test` to pass

**For releases:**
1. Run full test suite
2. Test local install from tarball
3. Verify on npm registry after publish
