# Metadata Enrichment Guide for Claude Self-Reflect

## Understanding the Metadata System

Claude Self-Reflect uses metadata to enable powerful search features like `search_by_concept` and `search_by_file`. This metadata includes:

- **concepts**: High-level topics detected in conversations (e.g., "docker", "security", "testing")
- **files_analyzed**: Files that were read or examined during conversations
- **files_edited**: Files that were modified during conversations
- **tools_used**: Tools that were used (Read, Edit, Bash, etc.)

## Common Issues and Solutions

### Issue 1: search_by_concept Returns No Results

**Symptoms:**
- `search_by_concept` returns "No conversations found about this concept"
- You see "NO METADATA FOUND" in search results

**Root Cause:**
- Conversations were imported before metadata extraction was implemented
- The streaming importer didn't extract metadata for older imports

**Solution:**
```bash
# Use the safe version that won't overwhelm your system
cd claude-self-reflect
source venv/bin/activate

# Process last 30 days of conversations
DAYS_TO_UPDATE=30 python scripts/delta-metadata-update-safe.py

# Or process ALL historical data (may take longer)
DAYS_TO_UPDATE=365 python scripts/delta-metadata-update-safe.py
```

### Issue 2: System Overload During Metadata Update

**Symptoms:**
- Kernel panic or system freeze
- "Server disconnected" errors
- Qdrant becomes unresponsive

**Root Cause:**
- Original delta-metadata-update.py makes too many concurrent requests
- No rate limiting or concurrency control

**Solution:**
Use the safe version with built-in protections:
```bash
# Configure for your system's capacity
BATCH_SIZE=5 \
RATE_LIMIT_DELAY=0.2 \
MAX_CONCURRENT_UPDATES=3 \
python scripts/delta-metadata-update-safe.py
```

### Issue 3: Partial Metadata Updates

**Symptoms:**
- Some conversations have metadata, others don't
- Inconsistent search results

**Solution:**
Check the update state and retry failed conversations:
```bash
# Check what's been updated
cat config/delta-update-state.json | python -m json.tool

# The safe script automatically retries failed conversations
# with exponential backoff and cooldown periods
python scripts/delta-metadata-update-safe.py
```

## How the Safe Update Script Works

The `delta-metadata-update-safe.py` script includes:

1. **Global Rate Limiting**: Ensures minimum delay between API calls (default 0.1s)
2. **Concurrency Control**: Limits simultaneous updates (default 5)
3. **Batch Processing**: Processes conversations in batches (default 10)
4. **State Persistence**: Saves progress after each batch
5. **Failure Recovery**: Tracks failed updates with retry counts and cooldown
6. **Memory Efficiency**: Limits file reads to avoid memory issues
7. **Collection Caching**: Avoids repeated API calls for collection names

## Configuration Options

Set these environment variables to tune performance:

```bash
# Number of conversations to process together
BATCH_SIZE=10

# Seconds to wait between Qdrant updates
RATE_LIMIT_DELAY=0.1

# Maximum concurrent update operations
MAX_CONCURRENT_UPDATES=5

# Days of history to process
DAYS_TO_UPDATE=7

# Maximum chunks per conversation
MAX_CHUNKS=20

# Test mode (no actual updates)
DRY_RUN=true
```

## Monitoring Progress

Watch the update progress:
```bash
# Tail the logs
python scripts/delta-metadata-update-safe.py 2>&1 | tee metadata-update.log

# Check state file
watch -n 5 'cat config/delta-update-state.json | python -m json.tool | tail -20'
```

## Understanding Search Fallback

With v2.5.19 improvements, search_by_concept now:

1. **Checks metadata health** before searching
2. **Falls back to semantic search** when metadata is missing
3. **Shows helpful status** in results:
   - `metadata_health`: Shows if metadata exists
   - `search_type`: Indicates if using metadata or fallback
   - Error messages suggest running the update script

## Best Practices

1. **Run metadata enrichment after major imports**
   ```bash
   # After importing new conversations
   python scripts/streaming_importer_final.py
   python scripts/delta-metadata-update-safe.py
   ```

2. **Use safe script for production**
   - Never use the original delta-metadata-update.py in production
   - Always use delta-metadata-update-safe.py with appropriate settings

3. **Monitor system resources**
   ```bash
   # Watch Qdrant memory usage
   docker stats claude-self-reflect-qdrant
   
   # Check system load
   top -o cpu
   ```

4. **Schedule regular enrichment**
   ```bash
   # Add to crontab for nightly updates
   0 2 * * * cd /path/to/claude-self-reflect && source venv/bin/activate && DAYS_TO_UPDATE=1 python scripts/delta-metadata-update-safe.py
   ```

## Troubleshooting Checklist

- [ ] Verified Qdrant is running and accessible
- [ ] Checked available system memory (need at least 2GB free)
- [ ] Used delta-metadata-update-safe.py (not the original)
- [ ] Configured appropriate rate limits for your system
- [ ] Checked state file for failed conversations
- [ ] Allowed cooldown period for failed updates (60s)
- [ ] Verified collections exist with correct names
- [ ] Ensured venv is activated before running scripts

## Emergency Recovery

If the system becomes unresponsive:

1. **Stop all updates**
   ```bash
   pkill -f delta-metadata-update
   ```

2. **Restart Qdrant**
   ```bash
   docker restart claude-self-reflect-qdrant
   # Or if running locally
   pkill qdrant
   ```

3. **Clear failed state** (if needed)
   ```bash
   # Back up current state
   cp config/delta-update-state.json config/delta-update-state.backup.json
   
   # Remove failed conversations to retry
   python -c "
   import json
   with open('config/delta-update-state.json', 'r') as f:
       state = json.load(f)
   state['failed_conversations'] = {}
   with open('config/delta-update-state.json', 'w') as f:
       json.dump(state, f, indent=2)
   "
   ```

4. **Resume with conservative settings**
   ```bash
   BATCH_SIZE=2 \
   RATE_LIMIT_DELAY=0.5 \
   MAX_CONCURRENT_UPDATES=2 \
   python scripts/delta-metadata-update-safe.py
   ```

## Getting Help

If issues persist:

1. Check the logs for specific errors
2. Verify your Qdrant version (>= 1.7.0 required)
3. Report issues at: https://github.com/anthropics/claude-self-reflect/issues
4. Include:
   - Error messages from logs
   - Contents of config/delta-update-state.json
   - System specifications (RAM, CPU cores)
   - Number of conversations in your system