# Corporate Machine Installation Fixes - v6.0.2

## Summary

This document tracks all changes made to address installation issues on corporate machines, identified from field reports and CodeRabbit review.

## Problems Identified

### 1. **FastEmbed Model Download Blocked by Corporate Proxies**
- **Symptom**: SSL certificate verification failures when downloading from HuggingFace
- **Impact**: Critical - prevents embeddings from working
- **Root Cause**: Corporate proxies intercept SSL, Python's certificate validation fails

### 2. **Statusline Installation Failures**
- **Symptom**: `csr-status: command not found`, status doesn't appear in Claude Code
- **Impact**: Medium - optional feature, core works without it
- **Root Cause**: Missing `cc-statusline` npm package, no sudo access for symlinks

### 3. **AST-Grep Not Installed**
- **Symptom**: Quality analysis features don't work
- **Impact**: Low - optional feature for code quality
- **Root Cause**: AST-Grep is external dependency, not installed automatically

### 4. **Metadata Script Path Outdated**
- **Symptom**: Docker command fails with "No such file"
- **Impact**: Low - documentation issue
- **Root Cause**: v6.0 restructuring moved scripts, docs not updated

### 5. **No Comprehensive Update/Fix Command**
- **Symptom**: Users have to manually diagnose and fix each issue
- **Impact**: High - poor user experience
- **Root Cause**: Installer doesn't detect/fix missing features

## Solutions Implemented

### 1. FastEmbed GCS Fallback (`installer/fastembed-fallback.js`)
**Status**: ✅ Implemented

**What it does**:
- Tests HuggingFace connectivity
- If blocked, downloads model from Google Cloud Storage (no SSL issues)
- Configures Docker Compose for offline mode
- Updates MCP server for offline operation

**User benefit**: Automatic fallback, no manual intervention needed

### 2. Statusline Auto-Installation (`installer/statusline-setup.js`)
**Status**: ✅ Implemented

**What it does**:
- Installs `cc-statusline` npm package automatically
- Creates symlink in `~/bin` (no sudo required)
- Falls back to global install if sudo available
- Provides clear instructions for PATH setup

**User benefit**: Statusline "just works" or fails gracefully with helpful message

### 3. AST-Grep Detection (`shared/ast_grep_utils.py`)
**Status**: ✅ Implemented

**What it does**:
- Checks if AST-Grep is installed
- Provides helpful installation instructions when missing
- Allows quality tools to work without it

**User benefit**: Clear messaging, no confusing errors

### 4. Comprehensive Update Command (`installer/update-manager.js`)
**Status**: ✅ Implemented

**What it does**:
- Detects all missing features
- Categorizes as critical vs optional
- Automatically fixes what it can
- Provides clear instructions for manual steps

**Features checked**:
- Docker running
- Qdrant running
- FastEmbed model available
- Docker Compose configured for offline
- cc-statusline installed
- csr-status command available
- AST-Grep installed (optional)

**User benefit**: Single command to fix everything: `claude-self-reflect update`

### 5. Documentation Updates
**Status**: ✅ Implemented

- Fixed metadata script path in release notes
- Created comprehensive corporate proxy guide
- Added troubleshooting documentation

## CodeRabbit Review Issues (To Fix)

### High Priority

1. **installer/cli.js** - Extract `runBashScript()` helper to reduce duplication
2. **installer/cli.js** - Add error handling for dynamic import of update-manager
3. **installer/statusline-setup.js** - Add npm prerequisite check
4. **installer/statusline-setup.js** - Remove unnecessary chmod on symlink

### Medium Priority

5. **docs/corporate-proxy-installation.md** - Add "source ~/.bashrc" instructions (2 locations)
6. **docs/corporate-proxy-installation.md** - Use `${HOME}` instead of `~` in Docker mounts
7. **docs/RELEASE_NOTES_v6.0.1.md** - Remove personal username from path

### To Investigate

8. **installer/fastembed-fallback.js:119-143** - Issue not fully shown in output

## Testing Plan

### Local Testing

- [ ] Test `claude-self-reflect update` command
- [ ] Test FastEmbed fallback with simulated proxy block
- [ ] Test statusline installation without sudo
- [ ] Test AST-Grep detection when not installed
- [ ] Test npm pack --dry-run for package structure

### CI/CD Testing (via PR)

- [ ] CodeRabbit PR review
- [ ] Docker build tests
- [ ] npm package tests
- [ ] Python tests (3.10, 3.11, 3.12)

## Release Workflow

### Phase 1: Local Quality Gates ✅
- [x] CodeRabbit CLI review (`coderabbit --prompt-only`)
- [ ] Fix all critical/high issues
- [ ] Re-run CodeRabbit to verify fixes
- [ ] Codex architectural review (optional, if major changes)

### Phase 2: Commit & PR
- [ ] Commit all fixes
- [ ] Create PR with descriptive title/body
- [ ] Monitor CodeRabbit PR review

### Phase 3: PR Review & Fixes
- [ ] Address CodeRabbit PR comments
- [ ] Push fixes
- [ ] Wait for CI/CD to pass

### Phase 4: Release (Open Source Maintainer Agent)
- [ ] Merge PR
- [ ] Create GitHub release
- [ ] Publish to NPM
- [ ] Close related issues
- [ ] Announce in discussions

## Commands Added

```bash
# New commands available
claude-self-reflect update      # Detect and fix missing features
claude-self-reflect version     # Check version and updates
claude-self-reflect uninstall   # Clean uninstall

# Enhanced existing commands
claude-self-reflect statusline  # Now installs cc-statusline first
claude-self-reflect setup       # Now runs update-manager on install
```

## Files Modified

### New Files
- `installer/fastembed-fallback.js` - GCS fallback for FastEmbed
- `installer/update-manager.js` - Comprehensive feature checker/fixer
- `shared/ast_grep_utils.py` - AST-Grep detection utilities
- `docs/troubleshooting/corporate-proxy-installation.md` - Corporate setup guide

### Modified Files
- `installer/cli.js` - Added update/version/uninstall commands
- `installer/statusline-setup.js` - Added cc-statusline installation
- `installer/postinstall.js` - Integrated update-manager
- `docs/releases/v2.5.19-RELEASE-NOTES.md` - Fixed script path

## Next Steps

1. Fix remaining CodeRabbit issues (9 total)
2. Test all new features locally
3. Commit and create PR
4. Monitor CodeRabbit PR review
5. Fix any CI/CD issues
6. Use open-source-maintainer agent to complete release

## Notes

- This is a patch release (v6.0.1 → v6.0.2)
- No breaking changes
- Backward compatible with v6.0.x
- Focuses on installation robustness and user experience
- Particularly important for corporate/enterprise environments

## Success Criteria

✅ Installation works behind corporate proxies without manual steps
✅ Statusline installs automatically or provides clear fallback
✅ AST-Grep optional, doesn't block installation
✅ Single `update` command fixes all issues
✅ Clear error messages for all failure modes
✅ Documentation comprehensive for corporate users

---

**Branch**: fix/corporate-installation-issues
**Target Release**: v6.0.2
**Priority**: High (affects enterprise adoption)
