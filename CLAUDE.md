# Claude Self-Reflect - Action Guide

## ⚠️ BREAKING CHANGES (v3.x → v4.0)

### Critical Migration Required
**⚠️ STOP**: If upgrading from v3.x, read this first!

#### Hash Algorithm Migration
- **Old**: MD5 IDs (legacy support enabled)
- **New**: SHA-256 + UUID for new conversations
- **Action**: Run `python scripts/migrate-ids.py` after backup

#### Embedding Dimensions
- **Local**: 384 dimensions (FastEmbed)
- **Cloud**: 1024 dimensions (Voyage)
- **Warning**: Collections are NOT cross-compatible
- **Action**: Rebuild collections if switching modes

#### Authentication Changes
- **New**: Qdrant requires authentication
- **Action**: Update `.env`: `QDRANT_API_KEY="your-key"`
- **Deadline**: Old connections fail after 2025-12-01

#### Async Pattern Changes
- **Old**: Threading-based operations
- **New**: Full asyncio implementation
- **Action**: Update custom scripts using the API

#### Collection Naming
- **Old**: Simple project names
- **New**: Prefixed naming
  - Local mode: `csr_project_local_384d` (384 dimensions)
  - Cloud mode: `csr_project_cloud_1024d` (1024 dimensions)
- **Action**: Run `python scripts/migrate-collections.py`

### Migration Checklist
- [ ] Backup Qdrant data: `python scripts/backup-qdrant.py`
- [ ] Run ID migration: `python scripts/migrate-ids.py`
- [ ] Update collection names: `python scripts/migrate-collections.py`
- [ ] Add authentication: `python scripts/migrate-auth.py`
- [ ] Update import tracking: `python scripts/migrate-imports.py`
- [ ] Test search functionality
- [ ] Verify all agents working

## 🎯 Primary Actions (Use These Daily)

### Search Past Conversations
```python
# Primary search tool - use liberally!
reflect_on_past("docker compose issues")

# Quick existence check
quick_search("have we discussed authentication?")

# Get insights without details
search_summary("performance optimization patterns")
```

### Check System Health
```bash
# Is everything working?
python mcp-server/src/status.py  # Real import status
docker ps | grep qdrant          # Vector DB running?
```

### Import New Conversations
```bash
source venv/bin/activate
python scripts/import-conversations-unified.py --limit 5  # Test first
python scripts/import-conversations-unified.py           # Full import
```

## ⚠️ Critical Rules

1. **PATH RULE**: Always use `/Users/username/...` never `~/...` in MCP commands
2. **TEST RULE**: Never claim success without running tests
3. **IMPORT RULE**: If status.py shows imports working, trust it (not other indicators)
4. **RESTART RULE**: After modifying MCP server code, restart Claude Code entirely

## 🔧 One-Time Setup

### Add MCP to Claude Code
```bash
# CRITICAL: Replace YOUR_USERNAME with actual username
claude mcp add claude-self-reflect \
  "/Users/YOUR_USERNAME/projects/claude-self-reflect/mcp-server/run-mcp.sh" \
  -e QDRANT_URL="http://localhost:6333" \
  -e QDRANT_API_KEY="your-key-if-auth-enabled" \
  -s user
```

### Start Required Services
```bash
docker compose up -d qdrant  # Vector database
docker start claude-reflection-safe-watcher  # Auto-importer
```

## 🚨 Troubleshooting Matrix

| Symptom | Check | Fix |
|---------|-------|-----|
| No search results | `docker ps \| grep qdrant` | `docker compose up -d qdrant` |
| Tools not available | `claude mcp list` | Remove & re-add MCP, restart Claude |
| Import shows 0% | Test with `reflect_on_past` | If search works, ignore the 0% |
| "spawn ~ ENOENT" | Check MCP path has `~` | Use full path `/Users/...` |

## 📁 Key Files

| What | Where | Purpose |
|------|-------|---------|
| Conversations | `~/.claude/projects/*/` | Source JSONL files |
| Import tracking | `~/.claude-self-reflect/config/imported-files.json` | What's been imported |
| MCP server | `mcp-server/src/server.py` | Main server (728 lines) |

## 🤖 Agent Activation Keywords

Say these to auto-activate specialized agents:
- "import showing 0 messages" → import-debugger
- "search seems irrelevant" → search-optimizer
- "find conversations about X" → reflection-specialist
- "Qdrant collection issues" → qdrant-specialist
- "quality issues detected" → quality-fixer
- "docker services fail" → docker-orchestrator
- "MCP tools not working" → mcp-integration
- "performance issues" → performance-tuner
- "test installations" → reflect-tester
- "release management" → open-source-maintainer

## 🔧 Quality Automation

### AST-GREP Integration
The system now includes comprehensive AST-GREP pattern analysis:
- **Unified Registry**: 100+ patterns for Python/TypeScript
- **Auto-fix**: Safe pattern fixes applied automatically
- **Quality Gates**: Pre-commit and post-generation hooks
- **Command**: `/fix-quality` to run quality fixer

### Hooks System
Automated hooks for quality control:
```bash
# Pre-commit: Updates quality cache
.claude/hooks/pre-commit

# Post-generation: Tracks edits and runs analysis
.claude/hooks/post-generation
```

### Quality Commands
```bash
# Run quality analysis
python scripts/ast_grep_final_analyzer.py

# Apply safe fixes
python scripts/ast_grep_final_analyzer.py --fix

# Check quality gate
python scripts/quality-gate.py --threshold 10

# Session quality tracking
python scripts/session_quality_tracker.py
```

## Mode Switching (Runtime, No Restart!)
```python
# Switch embedding modes without restarting
switch_embedding_mode(mode="cloud")  # Voyage AI, better accuracy
switch_embedding_mode(mode="local")  # FastEmbed, privacy-first
get_embedding_mode()                 # Check current mode
```

---
*Architecture details, philosophy, and history → See `docs/`*
*Full command reference → See `docs/development/MCP_REFERENCE.md`*