# Memory Investigation Findings - Import Watcher OOM Issues

## Executive Summary
The import watcher was experiencing consistent OOM kills (exit code -9) despite using only 137.4MB of the allocated 2GB memory limit. The root cause appears to be related to subprocess spawning in Docker containers, not actual memory exhaustion.

## Timeline & Investigation

### Initial State
- Import watcher failing every 60 seconds with exit code -9
- No successful imports for over 24 hours
- Memory limit set to 1GB (reduced from 2GB in v2.4.15)

### Key Findings

1. **Memory Usage Analysis**
   - Peak memory during import: 137.4MB (6.7% of 2GB limit)
   - Idle memory usage: ~7MB
   - FastEmbed model pre-cached in Docker image (384MB saved)
   - Actual memory usage well within limits

2. **The "Smoking Gun"**
   - Subprocess spawned by watcher gets killed with -9
   - Direct execution inside container works fine
   - All subprocess methods fail (subprocess.run, os.system, fork/exec)
   - Issue is specific to subprocess spawning, not memory limits

3. **Docker Environment**
   ```
   Virtual memory limit: soft=-1, hard=-1
   Data segment limit: soft=-1, hard=-1
   Stack size limit: soft=8388608, hard=-1
   Cgroup memory limit: Could not read
   ```

4. **Subprocess Testing Results**
   - Test 1 (subprocess.run): returncode=-9
   - Test 2 (os.system): returncode=35072 (137 << 8)
   - Test 3 (fork/exec): status=9
   - All methods consistently killed by OOM killer

## Temporary Solution
1. Increased memory limit back to 2GB
2. Modified watcher script to remove `capture_output=True`
3. Imports now running but still experiencing intermittent kills

## Root Cause Analysis
The issue appears to be Docker's memory accounting for subprocesses:
- Parent process (watcher) uses minimal memory (~7MB)
- Child process (import script) triggers OOM killer despite low usage
- Possible causes:
  - Docker cgroup memory accounting issues
  - Memory spike during subprocess creation
  - Python subprocess buffering with capture_output

## Recommendations

### Short-term
1. Keep memory limit at 2GB for stability
2. Use subprocess without output capture
3. Monitor for improvements

### Long-term
1. Consider restructuring to avoid subprocess spawning:
   - Import directly in watcher process
   - Use threading instead of subprocess
   - Implement as a single long-running process

2. Investigate Docker memory settings:
   - Check if swap is enabled
   - Review cgroup v1 vs v2 differences
   - Consider memory overcommit settings

3. Alternative approaches:
   - Use Docker healthchecks instead of subprocess
   - Implement import as a cron job
   - Use a message queue for import triggers

## Code Changes Made

1. **docker-compose.yaml**
   ```yaml
   watcher:
     mem_limit: 2g  # Increased from 1g
     memswap_limit: 2g
   ```

2. **import-watcher.py**
   ```python
   # Changed from:
   result = subprocess.run(cmd, capture_output=True, text=True)
   
   # To:
   result = subprocess.run(cmd)  # No output capture
   ```

## Monitoring Script
Created `monitor-memory.sh` to track real-time memory usage during import cycles. This helped identify the low actual memory usage.

## Next Steps
1. Continue monitoring with 2GB limit
2. Test streaming importer functionality
3. Consider architectural changes to eliminate subprocess usage
4. Document findings for future reference

## Related Issues
- v2.4.15 memory optimization (FastEmbed pre-caching)
- Docker container stability
- Import performance and reliability