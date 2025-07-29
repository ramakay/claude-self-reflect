# Release Notes - v2.4.7

## 🚨 Critical Bug Fix Release

### Fixed
- **Import Watcher Failure** - Fixed critical issue where Docker import watcher was failing continuously
  - Root cause: Dockerfile.watcher was not properly copying required scripts
  - Impact: All Docker users had non-functional automatic imports
  - Resolution: Scripts are now correctly included in the container

### What This Means
- Automatic imports will now work correctly for Docker users
- The import watcher will process new conversations every 60 seconds
- No manual intervention required after updating

### Upgrading
```bash
# Stop and remove the broken container
docker stop import-watcher
docker rm import-watcher

# Pull the latest changes
git pull

# Rebuild and start the fixed watcher
docker compose -f docker-compose-optimized.yaml up -d import-watcher
```

### Performance Note
Full imports currently take 2-5 minutes depending on conversation volume. We're working on incremental import improvements for the next release to reduce this to seconds.

### Thank You
Special thanks to @TheGordon for reporting the import performance issue that led to discovering this critical bug.