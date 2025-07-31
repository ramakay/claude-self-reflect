# Release Notes - v2.4.12

## Critical Fix: Docker Import Watcher Permission Error

This release addresses a critical issue where the Docker import watcher service was unable to save its state file due to permission errors, resulting in excessive CPU usage.

### What Was Fixed

**Issue**: Import watcher failing with "[Errno 13] Permission denied: '/config/imported-files.json.tmp'"
- **Impact**: 800%+ CPU usage from re-importing all files every 60 seconds
- **Root Cause**: Incorrect permissions on the `/config` directory in Docker containers

**Solution**: Added an init container that sets proper permissions before the watcher service starts
- Ensures `/config` directory has correct ownership (1000:1000)
- Prevents permission errors when saving state
- Eliminates redundant imports and high CPU usage

### Changes

- Added `init-permissions` service to docker-compose.yaml
- Added dependency to ensure init runs before watcher and importer services
- No changes to application logic or functionality

### Performance Impact

**Before fix**:
- Watcher re-imported all files every cycle
- CPU usage exceeded 800% on multi-core systems
- Import times of 7+ minutes repeated continuously

**After fix**:
- Only new/modified files are imported
- CPU usage minimal during idle periods
- State properly tracked across restarts

### Documentation

- Added comprehensive Docker watcher guide in `docs/docker-watcher-guide.md`
- Covers configuration, troubleshooting, and best practices
- Includes monitoring and health check guidance

### Upgrade Instructions

For Docker users:
```bash
# Pull latest changes
git pull

# Restart services with new configuration
docker-compose down
docker-compose --profile watch up -d
```

The init container will automatically fix permissions on first run.

### Verification

Check that the watcher is operating correctly:
```bash
# View logs
docker-compose logs -f watcher

# Should see:
# INFO - Loaded state with X previously imported files
# INFO - Skipping unchanged file: [filename]
```

### Contributors

Thanks to @TheGordon for identifying the issue and providing the fix in PR #23.

### Related Issues

- Fixes #22: Import watcher permission errors and high CPU usage
- Addresses Docker deployment resource consumption concerns