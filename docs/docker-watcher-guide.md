# Docker Import Watcher Service Guide

The Docker import watcher service provides automated, continuous import of Claude conversation logs into the vector database. This guide covers configuration, operation, and troubleshooting.

## Overview

The watcher service monitors your Claude conversation directory and automatically imports new or modified conversations every 60 seconds. It maintains state tracking to avoid re-importing unchanged files, significantly reducing resource usage.

## Architecture

The watcher consists of:
- Python-based import script running in a container
- State file tracking (`/config/imported-files.json`)
- Shared volumes for conversation logs and configuration
- Integration with Qdrant vector database

## Configuration

### Environment Variables

```bash
# Required
CLAUDE_LOGS_PATH=~/.claude/projects  # Path to Claude conversation logs
QDRANT_URL=http://qdrant:6333        # Qdrant connection URL

# Optional
VOYAGE_KEY=your-api-key              # For Voyage AI embeddings (defaults to local)
PREFER_LOCAL_EMBEDDINGS=true         # Use FastEmbed instead of Voyage AI
IMPORT_CYCLE_SECONDS=60              # Check interval (default: 60)
BATCH_SIZE=10                        # Files per batch (default: 10)
```

### Volume Mounts

The watcher requires these volumes:
- `${CLAUDE_LOGS_PATH}:/logs:ro` - Read-only access to conversation logs
- `./config:/config` - State file storage (requires write permissions)
- `./scripts:/scripts:ro` - Import scripts

## Running the Watcher

### Start the watcher service:
```bash
docker-compose --profile watch up -d
```

### View logs:
```bash
docker-compose logs -f watcher
```

### Stop the watcher:
```bash
docker-compose --profile watch down
```

## State Management

The watcher maintains state in `/config/imported-files.json`:
```json
{
  "file-uuid.jsonl": {
    "mtime": 1735645200.5,
    "chunks_imported": 42
  }
}
```

This prevents re-importing unchanged files and tracks:
- File modification time
- Number of chunks imported
- Import history

## Performance Optimization

### Memory Management
- Batch processing (10 files by default)
- Explicit garbage collection after each file
- Streaming import for large files

### CPU Usage
- State tracking prevents redundant imports
- Previously: 800%+ CPU from constant re-imports
- Now: Minimal CPU with proper state management

## Troubleshooting

### Permission Errors

If you see:
```
ERROR - Failed to save state file: [Errno 13] Permission denied: '/config/imported-files.json.tmp'
```

This is resolved in v2.4.12+ with an init container that sets proper permissions. For older versions:

```bash
# Manual fix
docker-compose exec watcher chown 1000:1000 /config
```

### High CPU Usage

Symptoms:
- Watcher consuming excessive CPU
- Logs show repeated imports of same files
- No state file being saved

Solution:
- Ensure v2.4.12+ is deployed
- Check `/config` directory permissions
- Verify state file is being written

### Missing Imports

If conversations aren't being imported:
- Check conversation log path is correct
- Verify JSONL files are valid format
- Review watcher logs for errors
- Ensure Qdrant is accessible

### State File Recovery

To reset and reimport all files:
```bash
docker-compose exec watcher rm /config/imported-files.json
docker-compose restart watcher
```

## Monitoring

### Health Checks

Monitor these indicators:
- State file updates every cycle
- CPU usage remains low
- Memory usage stable
- Import success messages in logs

### Log Patterns

Healthy operation:
```
INFO - Loaded state with 218 previously imported files
INFO - Skipping unchanged file: uuid.jsonl
INFO - Found 0 new/modified files to import
```

Issues requiring attention:
```
ERROR - Failed to save state file
ERROR - Connection to Qdrant failed
WARNING - Invalid JSONL format
```

## Best Practices

1. **Resource Allocation**: Ensure sufficient memory for batch processing
2. **Log Rotation**: Implement log rotation to prevent disk filling
3. **Monitoring**: Set up alerts for error patterns
4. **Backups**: Regular backups of the state file
5. **Updates**: Keep the service updated for performance improvements

## Integration with CI/CD

The watcher service integrates with automated deployments:
- State persists across container restarts
- Rolling updates supported
- Health checks ensure service availability

## Security Considerations

- Read-only mount for conversation logs
- Restricted network access (internal only)
- No external API exposure
- State file contains only metadata

## Future Enhancements

Planned improvements:
- Configurable import intervals
- Web UI for monitoring
- Metrics export for observability
- Dynamic batch sizing
- Multi-threaded processing