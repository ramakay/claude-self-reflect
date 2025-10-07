# Release Notes v6.0.3

**Release Date:** October 7, 2025
**Type:** URGENT FIX
**Severity:** Critical

## 🚨 Critical Fixes

### 1. **Embedding Model Correction**
- **Issue:** v6.0.2 incorrectly reverted to `BAAI/bge-small-en-v1.5` model in `embedding_service.py`
- **Impact:** Model mismatch caused embeddings to use wrong model (384d but different model)
- **Fix:** Restored correct model: `sentence-transformers/all-MiniLM-L6-v2`
- **File:** `src/runtime/embedding_service.py:48`

### 2. **Required Dependencies Now Enforced**
- **Issue:** AST-Grep and cc-statusline were marked as optional
- **Impact:** Users missing critical tooling for code quality and statusline
- **Fix:** Both are now REQUIRED and automatically installed during setup

### 3. **csr-status Accessibility**
- **Issue:** csr-status required manual installation to `/usr/local/bin` or `~/bin`
- **Impact:** Corporate machines couldn't access csr-status without sudo
- **Fix:** Added `csr-status` to npm bin, auto-installs with package

## 📦 What Changed

### Embedding Model Fix
```python
# BEFORE (v6.0.2 - WRONG)
self.model = TextEmbedding(model_name="BAAI/bge-small-en-v1.5")

# AFTER (v6.0.3 - CORRECT)
self.model = TextEmbedding(model_name="sentence-transformers/all-MiniLM-L6-v2")
```

### Required Dependencies
```javascript
// AST-Grep: Now critical=true, auto-installs via npm
async checkASTGrep() {
    return {
        installed: false,
        name: 'AST-Grep',
        critical: true,  // ← Was false
        fix: () => this.installASTGrep()  // ← Now installs via npm
    };
}

// cc-statusline: Now critical=true
async checkCCStatusline() {
    return {
        installed: false,
        name: 'cc-statusline',
        critical: true,  // ← Was false
        fix: () => this.installCCStatusline()
    };
}
```

### npm Bin Addition
```json
// package.json
"bin": {
    "claude-self-reflect": "installer/cli.js",
    "csr-status": "scripts/csr-status"  // ← NEW
}
```

## 🔧 Installation Changes

### Automatic Installation
After upgrading to v6.0.3, the installer will:

1. ✅ Install AST-Grep via `npm install -g @ast-grep/cli`
2. ✅ Install cc-statusline via `npm install -g cc-statusline`
3. ✅ Link `csr-status` command automatically (no sudo needed)

### Manual Installation (if needed)
```bash
# Update to v6.0.3
npm update -g claude-self-reflect

# Run update command to install missing dependencies
claude-self-reflect update
```

## 📊 Files Changed

| File | Lines Changed | Purpose |
|------|---------------|---------|
| `src/runtime/embedding_service.py` | +6/-2 | Fix embedding model |
| `installer/update-manager.js` | +56/-15 | Enforce dependencies |
| `package.json` | +3/-1 | Add csr-status bin |

**Total:** 3 files, 65 insertions(+), 18 deletions(-)

## 🎯 Migration Guide

### From v6.0.2 → v6.0.3

**CRITICAL:** If you installed v6.0.2, you may have wrong embedding model cached.

```bash
# 1. Update package
npm update -g claude-self-reflect

# 2. Clear FastEmbed cache (forces re-download of correct model)
rm -rf ~/.cache/fastembed/BAAI

# 3. Run update command
claude-self-reflect update

# 4. Restart Docker services to pick up correct model
docker compose restart

# 5. Verify model
python -c "from fastembed import TextEmbedding; m = TextEmbedding('sentence-transformers/all-MiniLM-L6-v2'); print('✓ Correct model loaded')"
```

### Verification
```bash
# Check csr-status is accessible
which csr-status  # Should show path in npm bin

# Check AST-Grep installed
ast-grep --version  # or: sg --version

# Check cc-statusline installed
npm list -g cc-statusline
```

## ⚠️ Breaking Changes

**None** - This is a bug fix release. All changes are backward compatible.

## 🐛 Known Issues

None specific to this release.

## 📈 Quality Metrics

- **CI/CD:** All checks passing (19/19)
- **Security:** No new vulnerabilities
- **Code Review:** CodeRabbit approved
- **Testing:** Validated on macOS (Darwin 24.6.0)

## 🙏 Credits

Fixed based on user report identifying the BAAI model reversion issue.

## 📚 Documentation Updates

- Updated installation guide to reflect automatic dependency installation
- Updated troubleshooting guide for embedding model verification
- Added corporate machine setup guide for npm bin usage

## 🔗 Related Issues

- Fixes embedding model regression from v6.0.2
- Resolves corporate machine statusline accessibility
- Ensures all users have required tooling

## 📝 Upgrade Priority

**URGENT** - Users on v6.0.2 should upgrade immediately to fix embedding model.

---

**Full Changelog:** https://github.com/ramakay/claude-self-reflect/compare/v6.0.2...v6.0.3
