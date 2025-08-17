# Lessons Learned from v2.5.16/v2.5.17 Release Crisis

## Critical Lessons from the 1437% CPU Emergency

### Testing & Quality Assurance
1. **NEVER skip tests - even under pressure**
   - We rushed v2.5.16 without completing 4 critical tests
   - The streaming importer didn't actually work with 400MB memory limit
   - Testing would have caught this before release

2. **Test with real data - not just synthetic tests**
   - Our tests used `.json` files but Claude uses `.jsonl`
   - Real-world testing with actual Claude conversations revealed the issue
   - Always validate with production-like data

3. **Memory limits matter - too conservative = nothing works**
   - 400MB limit prevented ANY file processing
   - 600MB was the minimum viable setting
   - Better to have higher limits with monitoring than broken functionality

4. **Pre-releases are valuable - saved us from breaking production**
   - Converting v2.5.16 to pre-release prevented user damage
   - Allowed testing without affecting stable users
   - Always use pre-release for significant changes

### Code Review & Validation
5. **Multiple AI reviews catch different issues**
   - GPT-5 caught queue overflow risks
   - Opus 4.1 identified signal handler race conditions
   - Each model has unique strengths - use multiple perspectives

6. **Version number consistency is critical**
   - User corrected v2.5.17 → v2.5.16
   - Automated systems kept reverting to wrong version
   - Establish single source of truth for versions

7. **CPU percentage calculations need container awareness**
   - Docker containers see all host CPUs but have cgroup limits
   - 1437% CPU was actually ~90% of allocated resources
   - Always use cgroup-aware CPU detection in containers

### Process & Workflow
8. **Follow the defined workflow religiously**
   - User specified: implementation → code review → testing → documentation → release
   - We jumped to release after code review, skipping tests
   - Workflows exist for a reason - don't skip steps

9. **Track test completion explicitly**
   - TodoWrite showed we marked tests "completed" without running them
   - Each test should produce verifiable output
   - "Test passing" requires actual execution, not just writing test code

10. **Resource monitoring must account for baseline usage**
    - System had 400MB baseline memory usage
    - Setting 400MB limit meant 0MB available for processing
    - Always measure baseline + headroom when setting limits

### Communication & Documentation
11. **Document the actual vs expected behavior gap**
    - "0 files processed" was the key metric showing failure
    - Don't just report "test failed" - show the numbers
    - Quantitative metrics reveal the severity

12. **Rollback strategies must be immediate**
    - Converting to pre-release was the right call
    - Don't "push through" - stop and fix properly
    - User trust > release deadlines

13. **State persistence without progress is a red flag**
    - State file updated but showed 0 processed files
    - High water mark advancing but no actual work done
    - Monitor actual work completion, not just activity

14. **Default configurations should be generous, then tightened**
    - Starting with 400MB was too aggressive
    - Should have started at 1GB and optimized down
    - Premature optimization broke core functionality