# Memory Footprint and Performance

## Overview
Claude Self-Reflect uses memory efficiently through incremental imports and optimized batch processing. This document details memory usage patterns and performance characteristics.

## Memory Requirements

### Docker Memory Allocation
- **Minimum**: 2GB RAM allocated to Docker
- **Recommended for Initial Setup**: 4GB RAM 
- **Steady State**: 2GB RAM is sufficient after initial import

### Actual Memory Usage
- **Idle**: <100MB
- **During Import**: 500MB-1.5GB (depends on number of new files)
- **Peak (First Import)**: Up to 2GB when processing entire conversation history

## First-Time Usage

When you first install Claude Self-Reflect, the import watcher needs to process your entire conversation history:

1. **Initial Import Duration**: 2-7 minutes (depending on conversation history size)
2. **Memory Peak**: 1.5-2GB during initial processing
3. **Files Processed**: All JSONL files in your Claude logs directory
4. **State Building**: Creates `/config/imported-files.json` tracking all processed files

⚠️ **Important**: This is a one-time operation. After the initial import, the system switches to incremental mode.

## Incremental Import Mode

After the initial import, the system operates efficiently:

- **Duration**: <60 seconds per cycle
- **Memory Usage**: 200-500MB typical
- **Files Processed**: Only new or modified files
- **Skip Rate**: 90%+ files are skipped as unchanged
- **State Persistence**: Progress saved after each file to prevent data loss

## Performance Optimization Details

### Batch Processing
- **Batch Size**: Optimized to 10 messages (reduced from 100)
- **Memory Impact**: 10x reduction in peak memory per batch
- **Trade-off**: Slightly more database calls but much better memory efficiency

### State Tracking
```json
{
  "imported_files": {
    "/logs/project/file.jsonl": {
      "last_modified": 1234567890,
      "last_imported": 1234567890,
      "chunks_imported": 150
    }
  }
}
```

### Memory Management
- **Garbage Collection**: Forced after each file processing
- **State Saves**: After each file (not just at end) to prevent loss on OOM
- **Embedding Model**: Loaded once, reused for all operations

## Performance Metrics

### First Import
- **Files**: 500-1000 typical
- **Chunks**: 5,000-15,000 typical  
- **Duration**: 2-7 minutes
- **Memory**: 1.5-2GB peak

### Subsequent Imports
- **Files Checked**: All
- **Files Processed**: 5-50 typical (new conversations)
- **Files Skipped**: 90%+ 
- **Duration**: 10-60 seconds
- **Memory**: 200-500MB peak

## Troubleshooting Memory Issues

### Symptoms of Memory Problems
1. Import watcher shows "Import failed with code -9"
2. Docker container restarts repeatedly
3. Import never completes

### Solutions

#### For Initial Setup
If experiencing OOM during first import:
1. Temporarily increase Docker memory to 4GB
2. Let initial import complete fully
3. Verify state file exists: `docker exec watcher ls -la /config/imported-files.json`
4. Reduce Docker memory back to 2GB

#### For Ongoing Issues
If memory issues persist after initial import:
1. Check batch size environment variable: `BATCH_SIZE` (should be 10 or less)
2. Verify state file is being updated
3. Check for unusually large conversation files
4. Consider increasing memory limit permanently

### Memory Limit Configuration

Docker Compose (docker-compose.yaml):
```yaml
watcher:
  mem_limit: 2g      # Increase to 4g if needed
  memswap_limit: 2g  # Match mem_limit value
```

Docker Desktop:
- Settings → Resources → Memory: Adjust slider
- Apply & Restart

## Architecture Considerations

### Why Memory Usage Varies
1. **Embedding Model**: FastEmbed model takes ~80MB when loaded
2. **File Processing**: Each conversation chunk needs embedding generation
3. **Batch Operations**: Multiple chunks processed together
4. **State Management**: File tracking data grows with conversation count

### Future Optimizations
- Streaming file processing (process chunks as read)
- Dynamic batch sizing based on available memory
- Embedding cache for common phrases
- Compressed state storage

## Monitoring Memory Usage

Check current usage:
```bash
docker stats claude-reflection-watcher
```

View detailed metrics:
```bash
docker exec watcher ps aux
```

Check state file size:
```bash
docker exec watcher ls -lh /config/imported-files.json
```

## Best Practices

1. **Initial Setup**: Start with 4GB, reduce to 2GB after first import
2. **Monitoring**: Check logs during first few import cycles
3. **Maintenance**: State file grows slowly (~1KB per 10 files)
4. **Updates**: New versions maintain backward compatibility with state files