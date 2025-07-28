# Claude Self-Reflect v2.4.0 Release Notes

## 🎉 Major Improvements

### Docker Volume Migration (PR #16)
- **Changed from bind mount to Docker named volumes** for better data persistence
- Automatic migration from `./data/qdrant` to `qdrant_data` volume
- Updated backup/restore scripts to work with named volumes
- Validated: All 33+ collections survive container restarts

### Enhanced Testing Infrastructure
- **New reflect-tester agent** for comprehensive system validation
- Phased testing approach handles Claude Code restart requirements
- Added 30-second wait after MCP restarts for proper initialization
- Tests both local and Voyage AI embedding modes

### Voyage AI Import Fix
- **Fixed missing `voyageai` dependency** in `scripts/requirements.txt`
- Successfully increased Voyage collections from 3 to 12+ during testing
- Both Docker and standalone imports now work with Voyage AI

### Security Improvements
- **Pinned all dependencies** to specific versions for reproducible builds
- Ran security scan with pip-audit - no vulnerabilities found
- Added tenacity==9.1.2 for safe retry handling

## 📊 Performance Baselines

### Local Embeddings (FastEmbed)
- **Embedding generation**: ~3ms
- **Single collection search**: ~5ms
- **Cross-collection search (10)**: ~19ms
- **Memory decay overhead**: <0.2ms for 1000 points

### Voyage AI Embeddings
- **Embedding generation**: ~190ms (includes API call)
- **Single collection search**: ~6ms
- **Cross-collection search (10)**: ~22ms
- **Collections created**: 1024-dimensional vectors

### System Statistics
- **Total collections tested**: 58 (29 local, 28 Voyage, 1 other)
- **Total conversation chunks**: 8,037+
- **Average points per collection**: 139

## 🛡️ Error Handling

### Verified Scenarios
- ✅ **Malformed JSON**: Import script gracefully skips invalid lines
- ✅ **Qdrant connection failures**: MCP tools return friendly error messages
- ✅ **Dimension mismatches**: Proper error when searching wrong collection type
- ✅ **Missing dependencies**: Clear error messages guide users

## 🔧 Configuration Updates

### Embedding Mode Switching
Switching between local and Voyage AI embeddings requires:
1. Update `.env` file: `PREFER_LOCAL_EMBEDDINGS=true/false`
2. Remove MCP: `claude mcp remove claude-self-reflect`
3. Re-add MCP: `claude mcp add claude-self-reflect "/path/to/run-mcp.sh"`
4. Restart Claude Code
5. Wait 30 seconds for initialization

### Environment Variables
```bash
QDRANT_URL=http://localhost:6333
ENABLE_MEMORY_DECAY=true
DECAY_WEIGHT=0.3
DECAY_SCALE_DAYS=90
PREFER_LOCAL_EMBEDDINGS=true
VOYAGE_KEY=your-api-key  # Required for Voyage AI mode
```

## 📝 Documentation Updates

### Updated Files
- `.claude/agents/reflect-tester.md` - Added 30-second wait and mode switching docs
- `.claude/agents/README.md` - Fixed missing reflection-specialist agent
- `CLAUDE.md` - Updated with correct MCP commands and testing procedures

### New Test Scripts
- `scripts/test-performance.py` - Comprehensive performance baseline testing
- `test-voyage-import.py` - Voyage AI import validation

## ✨ Newly Implemented

### Exponential Backoff for Voyage AI (PR #15)
- **Added retry logic with tenacity library** (Thanks to @jmherbst!)
- Exponential backoff: 30-120 seconds wait between retries
- Maximum 6 retry attempts for rate-limited requests
- Free tier users (3 RPM) now handled gracefully
- [View PR #15](https://github.com/ramakay/claude-self-reflect/pull/15)

## 📂 Project Isolation Feature (Issue #14)

### Already Implemented
The system **already provides per-project collection isolation**:
- Each project gets its own collection: `conv_<md5_hash>_<embedding_type>`
- Conversations are automatically isolated by project during import
- No cross-project contamination in search results

### How It Works
1. **Automatic Project Detection**: Based on directory structure
2. **Collection Naming**: MD5 hash of project path ensures uniqueness
3. **Metadata Storage**: Project name stored with each conversation chunk
4. **Search Scope**: Can search within specific project or across all projects

### Remaining Gap
While project isolation exists, there's no configuration file to exclude specific projects from import. All projects in `~/.claude/projects/` are still imported.

## 🐛 Bug Fixes

- Fixed agent documentation missing reflection-specialist
- Fixed Voyage AI import failures in Docker setup
- Fixed collection naming for test projects
- Updated backup/restore scripts for Docker volumes

## 🔄 Breaking Changes

None - all changes are backward compatible. Existing bind mount data automatically migrates to Docker volumes.

## 📈 Upgrade Guide

1. **Backup your data**: `./backup.sh ~/backups/pre-upgrade`
2. **Pull latest changes**: `git pull`
3. **Update dependencies**: `pip install -r scripts/requirements.txt`
4. **Restart services**: `docker compose down && docker compose up -d`
5. **Data migration**: Automatic on first start if using bind mounts

## 🧪 Testing Checklist

- [x] Docker volume persistence across restarts
- [x] Both embedding modes create correct collections
- [x] MCP tools work with all collection types
- [x] Error handling for malformed data
- [x] Performance baselines documented
- [x] Backup/restore functionality
- [x] Import watcher service operation

## 📋 Next Steps

1. Implement Issue #14 - Selective project import configuration
2. Add automated testing for error scenarios
3. Consider adding collection migration tools
4. Optimize cross-collection search performance

## 🙏 Acknowledgments

- PR #16 contributor for Docker volume migration
- @jmherbst for PR #15 - Exponential backoff implementation for Voyage AI
- Community testers who identified the Voyage AI import issue
- Users who reported the need for better error handling

---

**Full Changelog**: https://github.com/ramakay/claude-self-reflect/compare/v2.3.7...v2.4.0