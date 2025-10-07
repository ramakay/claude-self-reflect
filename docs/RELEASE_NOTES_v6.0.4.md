# Release Notes v6.0.4

**Release Date:** October 7, 2025
**Type:** Security & Stability Fix
**Severity:** High

## 🔒 Security Fixes

### 1. **Shell Injection Vulnerability (CRITICAL)**
- **Issue**: `installer/fastembed-fallback.js` used string interpolation in shell commands
- **Impact**: Potential shell injection if paths contained special characters
- **Fix**: Replaced `execSync` with `spawnSync` using array-based arguments
- **Files**: `installer/fastembed-fallback.js:70,83`

```javascript
// BEFORE (VULNERABLE)
execSync(`curl -L -o "${tarPath}" "${this.gcsUrl}"`)
execSync(`tar -xzf "${tarPath}" -C "${this.cacheDir}"`)

// AFTER (SECURE)
spawnSync('curl', ['-L', '-o', tarPath, this.gcsUrl])
spawnSync('tar', ['-xzf', tarPath, '-C', this.cacheDir])
```

## 🛠️ Stability Improvements

### 2. **Docker Compose v1/v2 Compatibility (HIGH)**
- **Issue**: Only supported Docker Compose v2 (`docker compose`)
- **Impact**: Failed on systems with Docker Compose v1 (`docker-compose`)
- **Fix**: Added v1 fallback when v2 fails
- **File**: `installer/update-manager.js:235-262`

### 3. **Qdrant Health Check Timeout (HIGH)**
- **Issue**: No timeout on Qdrant health check, could hang indefinitely
- **Impact**: Installation hung if Qdrant was unreachable
- **Fix**: Added 3-second timeout using AbortController
- **File**: `installer/update-manager.js:112-156`

### 4. **Promise.all Error Handling (HIGH)**
- **Issue**: Used `Promise.all` which throws on first rejection
- **Impact**: One failed check crashed entire update process
- **Fix**: Changed to `Promise.allSettled` with retry verification
- **File**: `installer/update-manager.js:304-332,354-394`

## 📦 What Changed

### Security & Stability
```javascript
// Shell command safety
- execSync with string interpolation (VULNERABLE)
+ spawnSync with array arguments (SECURE)

// Docker Compose compatibility
+ Fallback from v2 → v1

// Network timeout
+ 3-second timeout on Qdrant health check
+ Proper AbortController cleanup

// Error handling
- Promise.all (fails fast)
+ Promise.allSettled (collects all results)
+ Re-verification after fixes
```

### Code Quality
- Removed unused import (`join`) in `postinstall.js`
- Verified `functools.wraps` exists in `ast_grep_utils.py` decorator
- Removed unnecessary chmod on symlinks in `statusline-setup.js`

## 🔧 Files Changed

| File | Lines | Changes |
|------|-------|---------|
| `installer/fastembed-fallback.js` | +16/-8 | Shell injection fix |
| `installer/update-manager.js` | +82/-14 | Multiple stability fixes |
| `installer/postinstall.js` | +1/-1 | Remove unused import |
| `installer/statusline-setup.js` | +1/-1 | Fix symlink comment |
| `package.json` | +1/-1 | Version bump |

**Total**: 5 files, 101 insertions(+), 25 deletions(-)

## 📊 Impact Assessment

### Critical Fixes
- **Shell Injection**: Prevented potential command injection attacks
- **Hangs/Timeouts**: Installation no longer hangs on network issues
- **Docker Compatibility**: Works on both v1 and v2 systems

### Functional Changes
**IMPORTANT**: The Promise.allSettled change affects error handling flow:

**Before (v6.0.3)**:
- First check failure → entire update crashes
- No verification after fixes
- Silent failures possible

**After (v6.0.4)**:
- All checks run even if some fail
- Fixes are re-verified before proceeding
- Clear error messages for all failures

## 🎯 Migration Guide

### From v6.0.3 → v6.0.4

```bash
# Update package
npm update -g claude-self-reflect

# No additional steps needed - all fixes are backward compatible
```

### Verification
```bash
# Verify version
claude-self-reflect --version  # Should show 6.0.4

# Test update command (verifies all fixes)
claude-self-reflect update
```

## ⚠️ Breaking Changes

**None** - All changes are backward compatible and internal improvements.

## 🐛 Known Issues

None specific to this release.

## 📈 Quality Metrics

- **Security**: Shell injection vulnerability fixed
- **Stability**: 4 high-priority issues resolved
- **Compatibility**: Docker Compose v1 & v2 support
- **Error Handling**: Comprehensive retry and verification

## 🙏 Credits

Fixed based on CodeRabbit automated review identifying:
- 1 critical security issue
- 3 high-priority stability issues
- 3 code quality improvements

## 📚 Documentation Updates

- Updated error messages for better user guidance
- Improved timeout logging for network issues
- Added fallback explanations for Docker Compose

## 🔗 Related Issues

- Fixes shell injection vulnerability in fastembed-fallback
- Resolves Docker Compose v1/v2 compatibility
- Eliminates installation hangs on network timeouts
- Improves error recovery during update process

## 📝 Upgrade Priority

**HIGH** - Security fix for shell injection vulnerability

**Recommended for**: All users, especially those in:
- Corporate environments with Docker Compose v1
- Networks with restrictive firewalls
- Systems with potential malicious path manipulation

---

**Full Changelog:** https://github.com/ramakay/claude-self-reflect/compare/v6.0.3...v6.0.4
