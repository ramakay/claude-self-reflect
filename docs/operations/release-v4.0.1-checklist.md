# Release v4.0.1 Checklist

## Summary
Bug fix release addressing AttributeError in streaming-watcher.py and updating agent documentation with comprehensive workflows.

## Pre-Release
- [x] Check current version: v4.0.0 (Latest)
- [x] Identify changes: Fixed streaming-watcher.py AttributeError, updated agents
- [x] Check PR #56 CodeRabbit feedback: Comprehensive review completed
- [ ] Resolve all merge conflicts (if any)
- [ ] Security scan passes
- [ ] All CI/CD checks green
- [ ] Contributors acknowledged

## Changes Made
1. **Bug Fix**: Fixed AttributeError in streaming-watcher.py
   - Removed reference to `self.config.state_file`
   - Updated to display "UnifiedStateManager" instead

2. **Agent Updates**:
   - Updated CSR tester agent to include CodeRabbit CLI testing
   - Updated open-source-maintainer agent with complete release workflow
   - Enhanced CLAUDE.md with comprehensive workflow documentation

## Release Steps
- [ ] Create feature branch for fixes
- [ ] Create PR with changes
- [ ] Monitor CodeRabbit automated review
- [ ] Wait for CI/CD checks to pass
- [ ] Get PR approved and merged to main
- [ ] Tag created and pushed
- [ ] GitHub release created
- [ ] NPM package published (automated)
- [ ] Announcements sent

## Next Version: v4.0.1 (Patch - Bug fixes)
- Type: PATCH (bug fixes, no breaking changes)
- Previous: v4.0.0
- Target: v4.0.1

## Verification
- [ ] GitHub release visible
- [ ] NPM package updated
- [ ] No rollback needed
- [ ] Issues resolved and closed

## Related Issues
- Addresses streaming-watcher.py AttributeError
- Improves release process documentation
- Enhances agent capabilities