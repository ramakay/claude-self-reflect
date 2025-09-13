# Release v3.3.0 Checklist

## Pre-Release
- [x] Check current version: v3.2.4
- [x] Update package.json to v3.3.0
- [ ] Update CHANGELOG.md with comprehensive entry
- [ ] Update README.md with new features
- [ ] Ensure all new scripts are in package.json files list
- [ ] Security scan passes
- [ ] All CI/CD checks green
- [ ] Contributors acknowledged

## Major Changes This Release
### Critical Fixes
- [x] Fixed circular reference causing 100% CPU usage in get_embedding_manager
- [x] Fixed store_reflection dimension mismatch (supports both reflections_local and reflections_voyage)
- [x] Fixed SearchResult type inconsistency in parallel_search.py

### Major Improvements
- [x] Modularized server.py from 2321 to 728 lines (68% reduction)
  - Split into: search_tools, temporal_tools, reflection_tools, parallel_search, etc.
- [x] Restored rich formatting with emojis (🎯, ⚡, 📊) for better UX
- [x] All 15+ MCP tools now fully operational

### New Features
- [x] Temporal tools suite (get_recent_work, search_by_recency, get_timeline)
- [x] Enhanced metadata extraction (files_analyzed, tools_used, concepts)
- [x] Precompact hook functionality with import-latest.py
- [x] Real-time indexing with smart intervals (2s for hot files, 60s normal)

## Release Steps
- [ ] Commit version changes
- [ ] Create comprehensive CHANGELOG entry
- [ ] Update README with new features
- [ ] Create release notes document
- [ ] Tag created and pushed
- [ ] GitHub release created
- [ ] NPM package published (automated)
- [ ] Announcements sent

## Verification
- [ ] GitHub release visible
- [ ] NPM package updated
- [ ] No rollback needed
- [ ] All new features documented