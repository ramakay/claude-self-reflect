# Corporate Machine Testing Guide

## Testing Steps After v6.0.2 Release

Once v6.0.2 is published to NPM, follow these steps on your corporate machine to verify all fixes work correctly.

### Prerequisites
- Corporate machine with restricted sudo access
- Corporate proxy that intercepts SSL
- Docker Desktop installed
- Node.js 18+ installed

### Test 1: Fresh Installation

```bash
# Uninstall old version (if exists)
npm uninstall -g claude-self-reflect

# Install new version
npm install -g claude-self-reflect@6.0.2

# Verify installation message shows:
# - ✅ FastEmbed ready (corporate proxy-compatible)
# - ✅ cc-statusline installed
# - ℹ️ Statusline integration details
```

**Expected Behavior**:
- No SSL certificate errors during FastEmbed download
- Automatically falls back to Google Cloud Storage if HuggingFace blocked
- cc-statusline installs automatically
- csr-status symlink created in ~/bin (no sudo prompt)

### Test 2: Update Command

```bash
claude-self-reflect update
```

**Expected Behavior**:
- Detects Docker running ✅
- Detects Qdrant status
- Detects FastEmbed model availability
- Installs cc-statusline if missing
- Creates csr-status symlink if missing
- Provides clear status of all features
- No crashes or stack traces

### Test 3: Setup Wizard

```bash
claude-self-reflect setup
```

**Expected Behavior**:
- Wizard starts successfully
- Offers Local (FastEmbed) and Cloud (Voyage) embedding options
- Creates Docker Compose configuration
- Starts Qdrant successfully
- No SSL errors during setup

### Test 4: FastEmbed Offline Mode

```bash
# Check FastEmbed model exists
ls ~/.cache/fastembed/

# Should see: sentence-transformers-all-MiniLM-L6-v2/

# Verify Docker Compose has offline config
grep -A 2 "HF_HUB_OFFLINE" docker-compose.yaml

# Should show:
# - HF_HUB_OFFLINE=1
# - Volume mount: ${HOME}/.cache/fastembed:/root/.cache/fastembed:ro
```

**Expected Behavior**:
- Model downloaded from Google Cloud Storage (not HuggingFace)
- Docker Compose configured for offline mode
- No SSL certificate errors

### Test 5: Statusline Integration

```bash
# Check csr-status command
csr-status --compact

# Expected output: [81%][✨] or similar
```

**If command not found**:
```bash
# Add ~/bin to PATH
export PATH="$HOME/bin:$PATH"
source ~/.bashrc  # or ~/.zshrc

# Try again
csr-status --compact
```

**Expected Behavior**:
- Command works after adding ~/bin to PATH
- Shows import percentage and quality indicator
- No errors

### Test 6: MCP Search Functionality

```bash
# Start Qdrant
docker compose up -d qdrant

# Import conversations
docker compose run --rm importer python /app/src/runtime/import-conversations-unified.py --limit 5

# Check status
claude-self-reflect status
```

**Expected Behavior**:
- Import completes without SSL errors
- Status shows percentage > 0%
- In Claude: "search for docker issues" returns results

### Test 7: Metadata Enrichment

```bash
# Run metadata update with correct path
docker compose run --rm importer python /app/src/runtime/delta-metadata-update-safe.py
```

**Expected Behavior**:
- Script found at /app/src/runtime/ (not /app/scripts/)
- Processes conversations successfully
- No "No such file" errors

### Test 8: AST-Grep Detection

```bash
# Try quality command (AST-Grep not installed)
python scripts/quality-gate.py

# Should see helpful message:
# ⚠️  Quality analysis requires AST-Grep
#    To install:
#    • macOS:  brew install ast-grep
#    • Linux:  npm install -g @ast-grep/cli
```

**Expected Behavior**:
- Clear, helpful message when AST-Grep missing
- No crashes or confusing errors
- Core functionality unaffected

### Test 9: Version & Help Commands

```bash
# Check version
claude-self-reflect version

# Check help
claude-self-reflect help

# Check doctor
claude-self-reflect doctor
```

**Expected Behavior**:
- All commands work without errors
- version shows current version and update check
- help shows all available commands including 'update'
- doctor provides comprehensive health check

### Troubleshooting Test Cases

#### Case A: HuggingFace Completely Blocked
1. Verify FastEmbed fallback downloads from GCS automatically
2. No manual intervention required
3. Docker containers start successfully

#### Case B: No Sudo Access
1. Statusline installs to ~/bin instead of /usr/local/bin
2. Clear instructions to add ~/bin to PATH
3. No sudo password prompts
4. Core MCP functionality works regardless

#### Case C: npm Not in PATH
1. Prerequisite check catches missing npm
2. Clear error message with solution
3. No confusing stack traces

#### Case D: Outdated Documentation
1. All script paths use correct v6.0 structure (/app/src/runtime/)
2. All Docker mount examples use ${HOME} not ~
3. All PATH examples include source command

### Verification Checklist

- [ ] Fresh install completes without errors
- [ ] FastEmbed downloads from GCS when HuggingFace blocked
- [ ] cc-statusline installs automatically
- [ ] csr-status command works (after PATH setup)
- [ ] update command detects and fixes missing features
- [ ] MCP search returns results
- [ ] Metadata script uses correct path
- [ ] AST-Grep shows helpful message when missing
- [ ] Docker Compose uses ${HOME} not ~
- [ ] All documentation examples work as-is

### Expected Issues (Known Limitations)

1. **Statusline requires PATH update**:
   - Not automatic, user must add ~/bin to PATH
   - This is correct behavior (can't modify shell config without permission)

2. **AST-Grep optional**:
   - Not installed automatically
   - Quality features gracefully disabled without it

3. **First-time Docker pull**:
   - May take time depending on network
   - This is normal Docker behavior

### Success Criteria

✅ All test cases pass
✅ No SSL certificate errors
✅ No sudo password prompts for core features
✅ Clear error messages for all failure modes
✅ Documentation examples work without modification
✅ update command fixes all detected issues

### Failure Reporting

If any test fails, please report with:
1. Test number and description
2. Full error message
3. Output of `claude-self-reflect doctor`
4. Docker logs: `docker compose logs`
5. Corporate proxy details (if applicable)

Open issue at: https://github.com/ramakay/claude-self-reflect/issues

---

**Version Tested**: v6.0.2
**Test Date**: [Fill in after running tests]
**Tester**: [Your name]
**Environment**: Corporate machine behind proxy, no sudo access
**Result**: [PASS/FAIL]
