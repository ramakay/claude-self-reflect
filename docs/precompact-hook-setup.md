# PreCompact Hook Setup Guide

## Overview
The PreCompact hook automatically preserves your Claude conversations to Qdrant whenever you use the `/compact` command (manual or auto-compaction). This ensures your conversation history is searchable even after compaction.

## Features
- **Automatic Import**: Conversations are staged and imported to Qdrant on compaction
- **Non-blocking**: Hook completes in <1 second, import happens asynchronously
- **Searchable History**: Use MCP tools to search compacted conversations
- **Failure Resilient**: Never blocks compaction even if import fails

## Installation

### 1. Install the Hook Script
```bash
# The hook is already installed at:
~/.claude/hooks/precompact-auto.py
```

### 2. Configure Claude Settings
The hook is already configured in `.claude/settings.json`:
```json
{
  "hooks": {
    "PreCompact": [{
      "hooks": [{
        "type": "command",
        "command": "python3 ~/.claude/hooks/precompact-auto.py 2>/dev/null || true"
      }]
    }]
  }
}
```

### 3. Set Up the Processor
The processor script imports staged conversations to Qdrant:
```bash
# Located at:
~/projects/claude-self-reflect/scripts/process-staged-imports.py
```

## Usage

### Manual Compaction
1. Use `/compact` in any Claude conversation
2. Hook automatically stages the conversation
3. Run the processor to import:
```bash
cd ~/projects/claude-self-reflect
source venv/bin/activate  # or .venv/bin/activate
python scripts/process-staged-imports.py
```

### Automatic Processing (Optional)
Add to crontab for automatic processing:
```bash
# Process staged conversations every 5 minutes
*/5 * * * * cd ~/projects/claude-self-reflect && source venv/bin/activate && python scripts/process-staged-imports.py 2>&1 | logger -t claude-import
```

### Verify Import
Search for your conversation using MCP:
```
mcp__claude-self-reflect__reflect_on_past
```

## Configuration

### Environment Variables
- `CLAUDE_STAGING_DIR`: Custom staging directory (default: system temp)
- `IMPORT_TIMEOUT`: Import timeout in seconds (default: 120)
- `QDRANT_URL`: Qdrant server URL (default: http://localhost:6333)

### Staging Directory
Staged conversations are stored in:
```
$CLAUDE_STAGING_DIR/claude-reflect-staging/
```
Or if not set:
```
/tmp/claude-reflect-staging/  # Linux/Mac
```

## File Structure
When a conversation is staged:
```
claude-reflect-staging/
├── <conversation-id>.jsonl   # Conversation data
├── <conversation-id>.meta    # Metadata (trigger, timestamp, etc.)
└── <conversation-id>.ready   # Marker file indicating staging complete
```

## Troubleshooting

### Hook Not Triggering
1. Check Claude settings:
```bash
cat ~/.claude/settings.json | jq '.hooks.PreCompact'
```

2. Test the hook manually:
```bash
echo '{"transcript_path": "~/.claude/projects/test/test.jsonl", "trigger": "manual", "session_id": "test"}' | python3 ~/.claude/hooks/precompact-auto.py
```

### Import Not Working
1. Check staging directory:
```bash
ls -la /tmp/claude-reflect-staging/
```

2. Run processor with verbose output:
```bash
python scripts/process-staged-imports.py
```

3. Check import logs:
```bash
tail -f ~/projects/claude-self-reflect/logs/import.log
```

### Conversations Not Searchable
1. Verify Qdrant is running:
```bash
docker ps | grep qdrant
```

2. Check if conversation was imported:
```bash
python -c "
from qdrant_client import QdrantClient
client = QdrantClient('localhost', port=6333)
collections = client.get_collections().collections
print(f'Total collections: {len(collections)}')
"
```

3. Force re-import:
```bash
python scripts/import-conversations-unified.py --file ~/.claude/projects/*/conversation-id.jsonl --force
```

## Security Considerations

### File Permissions
- Hook script: 755 (readable/executable by user)
- Staging directory: 700 (user access only)
- Staged files: 600 (user read/write only)

### Concurrent Access
- File locking prevents corruption during concurrent operations
- Atomic writes ensure data integrity
- Ready markers prevent partial file processing

### Error Handling
- Hook never blocks compaction (always exits 0)
- Failed staging is cleaned up automatically
- Import errors are logged but don't affect staging

## Performance

### Hook Performance
- Staging: <1 second for conversations up to 10MB
- Non-blocking: Returns immediately to Claude
- Memory efficient: Uses streaming file copy

### Import Performance
- Processing: 1-5 seconds per conversation
- Batch capable: Can process multiple staged conversations
- Concurrent safe: Multiple processors can run simultaneously

## Advanced Configuration

### Custom Staging Location
```bash
export CLAUDE_STAGING_DIR=/path/to/custom/staging
```

### Increase Import Timeout
```bash
export IMPORT_TIMEOUT=300  # 5 minutes
```

### Debug Mode
```bash
# Run processor with debug output
python scripts/process-staged-imports.py 2>&1 | tee debug.log
```

## Integration with Docker Watcher

If using the Docker watcher for continuous import:
```bash
# The watcher will pick up staged conversations automatically
docker logs claude-reflection-safe-watcher --tail 50
```

## Maintenance

### Clean Old Staged Files
```bash
# Remove processed files older than 7 days
find /tmp/claude-reflect-staging/processed -mtime +7 -delete

# Remove failed files older than 30 days  
find /tmp/claude-reflect-staging/failed -mtime +30 -delete
```

### Monitor Disk Usage
```bash
du -sh /tmp/claude-reflect-staging/
```

## Support

For issues or questions:
- Check logs: `~/projects/claude-self-reflect/logs/`
- Review this guide
- Search past conversations about PreCompact setup using MCP

## Version History
- v1.0.0 (2025-09-05): Initial implementation with staging pattern
- v1.0.1 (2025-09-05): Added retry logic for file locking
- v1.0.2 (2025-09-05): Enhanced error logging and validation
- v1.1.0 (2025-09-06): Fixed critical collection naming issue - extract actual project name, not full path