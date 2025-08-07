# Claude Self-Reflect v2.5.0 Release Plan

## Executive Summary

Version 2.5.0 focuses on addressing critical stability issues, user experience improvements, and completing missing features identified through recent investigations and community feedback.

## Critical Issues Identified

### 1. Docker Subprocess Memory Issue (Priority: CRITICAL)
**Current State:**
- Import watcher experiencing OOM kills despite using only 137MB of 2GB allocated memory
- Root cause: Docker subprocess memory accounting, not actual memory exhaustion
- Temporary fix applied (removed `capture_output=True`) but needs permanent solution

**Impact:** Prevents reliable imports, frustrating user experience

### 2. Missing Streaming Importer (Priority: HIGH)
**Current State:**
- Referenced in multiple files but implementation doesn't exist
- `Dockerfile.streaming-importer` exists but points to non-existent script
- `scripts/local-import.sh` attempts to use it (line 56) but would fail
- No streaming importer found in any branch

**Impact:** Incomplete feature that's documented but non-functional

### 3. User Experience Issues (Priority: HIGH)
**From Issue #27 - User Feedback:**
- Project scope confusion - must explicitly specify "all" to search effectively
- Installation confusion - unclear if MCP should be user or project level
- No clear upgrade/reinstallation process
- Per-project memories not working as expected

**Impact:** Confusing setup and usage, limiting adoption

## Release Strategy

### Phase 1: Critical Fixes (Week 1)

#### 1.1 Docker Memory Architecture Refactor
**Problem:** Subprocess spawning in Docker causes OOM kills
**Solution:**
- Refactor `import-watcher.py` to eliminate subprocess usage
- Implement direct import within watcher process using threading
- Add proper Docker healthchecks

**Files to modify:**
```
scripts/import-watcher.py       # Complete refactor
docker-compose.yaml            # Update healthchecks
scripts/monitor-memory.sh      # Enhanced monitoring
```

#### 1.2 Complete or Remove Streaming Importer
**Decision Required:** 
- Option A: Implement streaming importer functionality
- Option B: Remove references and use unified importer
- **Recommendation:** Option B - Remove references, document unified importer as the solution

**Actions for Option B:**
```
- Remove Dockerfile.streaming-importer
- Update scripts/local-import.sh to use unified importer
- Update documentation to clarify unified importer usage
```

### Phase 2: User Experience Improvements (Week 2)

#### 2.1 Enhanced Setup Wizard
**Features:**
- Auto-detect existing installations
- Clear upgrade path with data preservation
- Verbose progress indicators
- Handle broken venv states
- Automatic Docker cleanup and rebuild

**New Commands:**
```bash
claude-self-reflect upgrade    # Intelligent upgrade handler
claude-self-reflect doctor     # Diagnose installation issues
claude-self-reflect reset      # Clean reinstall with data backup
```

#### 2.2 Project Scope Improvements
**Changes:**
- Auto-detect current project from working directory
- Make project-specific search the default behavior
- Add visual indicators of search scope
- Better error messages when no results found

**MCP Server Updates:**
```python
# Default to current project
# Add --global flag for cross-project searches
# Clear messaging about search scope
```

### Phase 3: Documentation & Testing (Week 3)

#### 3.1 Comprehensive Documentation Update
- Step-by-step upgrade guide
- Troubleshooting common issues
- Architecture diagrams
- Video walkthrough for setup

#### 3.2 Testing Suite
- Automated tests for Docker memory issues
- MCP integration tests
- Upgrade path testing
- Project scope detection tests

### Phase 4: Release & Support (Week 4)

#### 4.1 Release Checklist
- [ ] All critical fixes tested
- [ ] Documentation updated
- [ ] Migration guide created
- [ ] Beta testing complete
- [ ] Release notes finalized

#### 4.2 Post-Release Support Plan
- Monitor GitHub issues closely (2-week sprint)
- Quick patch releases for critical issues
- Community feedback sessions
- Plan for v2.6.0 based on feedback

## Success Metrics

1. **Stability**
   - Zero OOM kills during normal operation
   - 99%+ import success rate
   - No Docker container restarts

2. **User Experience**
   - 90%+ successful upgrades without manual intervention
   - Project search works without explicit "all" parameter
   - Setup completes in <5 minutes

3. **Community Satisfaction**
   - Positive feedback on Issue #27 resolution
   - Reduced support issues
   - Increased adoption rate

## Risk Mitigation

1. **Backward Compatibility**
   - Maintain all v2.4.x APIs
   - Provide migration scripts
   - Keep old import scripts as fallback

2. **Data Safety**
   - Automatic backups before upgrade
   - Rollback instructions
   - Data validation after migration

3. **Communication**
   - Clear breaking changes documentation
   - Proactive community announcements
   - Beta testing with key users

## Technical Debt Addressed

1. Remove subprocess architecture in favor of threading
2. Consolidate import scripts (remove duplicate functionality)
3. Standardize error handling and logging
4. Improve Docker resource management

## Timeline

| Week | Phase | Deliverables |
|------|-------|--------------|
| 1 | Critical Fixes | Docker memory fix, Streaming importer decision |
| 2 | UX Improvements | Setup wizard v2, Project scope fixes |
| 3 | Testing & Docs | Full test suite, Updated documentation |
| 4 | Release | v2.5.0 release, Community support |

## Team Assignments

- **Core Development**: Docker fixes, import refactor
- **UX/Tools**: Setup wizard, CLI improvements
- **Documentation**: User guides, migration docs
- **QA/Testing**: Test suite, beta coordination
- **Community**: Issue monitoring, feedback collection

## Go/No-Go Criteria

**Must Have:**
- Docker memory issue resolved
- Project scope search working correctly
- Upgrade path documented and tested
- All tests passing

**Nice to Have:**
- Video tutorials
- Extended logging options
- Performance improvements

## Post-Release Plan

1. **Week 1-2**: Active monitoring and quick fixes
2. **Week 3-4**: Collect feedback for v2.6.0
3. **Month 2**: Plan next major release based on community needs

---

**Status**: DRAFT
**Last Updated**: August 2025
**Owner**: Claude Self-Reflect Team