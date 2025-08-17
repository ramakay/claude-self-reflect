# Claude Self-Reflect v2.5.17 - Hotfix Release

**Released: August 17, 2025**

## Critical Memory Limit Fix

### Issue Resolved
The streaming importer in v2.5.16 was unable to process files due to an overly conservative memory limit (400MB) that prevented operation on systems with normal memory usage.

### Fix Applied
- Increased default memory limit from 400MB to 600MB
- This allows the streaming importer to function properly while still protecting against OOM conditions

### Testing Completed
✅ Streaming importer successfully processes files with new limit
✅ Created 293 chunks from 3 test files
✅ CPU throttling working correctly (maintains ~500% usage)
✅ Memory stable at ~460MB during operation
✅ State persistence working correctly

### Changes
- `docker-compose.yaml`: MEMORY_LIMIT_MB increased to 600
- `streaming_importer_final.py`: Default memory limit increased to 600MB

### Performance Metrics
- Files processed: 3 files in 20 seconds
- Chunks created: 293 total
- Memory usage: Stable at ~460MB
- CPU usage: Throttled to ~500%

### Upgrade Instructions
For Docker users:
```bash
docker-compose down streaming-importer
docker-compose up -d streaming-importer
```

For direct Python users:
```bash
export MEMORY_LIMIT_MB=600
python scripts/streaming_importer_final.py
```

### Known Issues
- Large backlog of conversations (29+ days) may take time to process
- Initial memory usage will be higher until backlog clears

This hotfix ensures the streaming importer can actually process files in production environments.