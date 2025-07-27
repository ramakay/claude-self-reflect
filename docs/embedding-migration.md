# Embedding Mode Migration Guide

> [!WARNING]
> Switching between local and cloud embeddings is NOT a simple configuration change. It requires re-importing all conversations and creates separate collections. This guide is primarily for testers and advanced users.

## Understanding Embedding Modes

### Local Embeddings (Default)
- **Model**: FastEmbed with sentence-transformers/all-MiniLM-L6-v2
- **Dimensions**: 384
- **Collections**: Named with `_local` suffix
- **Privacy**: All processing happens on your machine
- **Performance**: Good accuracy, fast processing

### Cloud Embeddings (Voyage AI)
- **Model**: voyage-3-large
- **Dimensions**: 1024
- **Collections**: Named with `_voyage` suffix
- **Privacy**: Conversation text sent to Voyage AI
- **Performance**: Better accuracy, requires internet

## Why Switching is Complex

1. **Different Vector Dimensions**: Local (384) vs Cloud (1024) embeddings are incompatible
2. **Separate Collections**: Each mode creates its own set of collections
3. **No Cross-Search**: Cannot search across collections with different dimensions
4. **Data Duplication**: Switching modes doubles your storage requirements

## Migration Steps

### Prerequisites
- Backup any important reflections
- Ensure you have time for full re-import (can take 30+ minutes)
- Have your Voyage API key ready (if switching to cloud)

### Step 1: Export Important Data

```bash
# Store important reflections before migration
mcp__claude-self-reflect__store_reflection
```

### Step 2: Stop All Services

```bash
# Stop Docker containers
docker compose down

# Remove MCP server
claude mcp remove claude-self-reflect
```

### Step 3: Update Configuration

Edit `.env` file:
```bash
# For local mode:
PREFER_LOCAL_EMBEDDINGS=true

# For cloud mode:
PREFER_LOCAL_EMBEDDINGS=false
VOYAGE_KEY=your-voyage-api-key
```

### Step 4: Clear Existing Data (Optional)

If you want to remove old collections:
```bash
# Remove all Qdrant data
rm -rf qdrant_storage/
```

### Step 5: Restart Services

```bash
# Start Qdrant
docker compose up -d qdrant

# Recreate import containers
docker compose up -d watcher
```

### Step 6: Re-add MCP Server

**Important**: Restart Claude Code first to ensure clean environment

```bash
# After restarting Claude Code
claude mcp add claude-self-reflect "/path/to/claude-self-reflect/mcp-server/run-mcp.sh"
```

### Step 7: Re-import All Conversations

```bash
# Run unified import script
cd /path/to/claude-self-reflect
source mcp-server/venv/bin/activate
python scripts/import-conversations-unified.py
```

## Verification

### Check Collections
```bash
python scripts/check-collections.py
```

You should see:
- For local mode: Collections ending with `_local`
- For cloud mode: Collections ending with `_voyage`

### Test Search
Use the reflection tools to verify search is working:
```
mcp__claude-self-reflect__reflect_on_past
```

## Common Issues

### Mixed Collections
If you see both `_local` and `_voyage` collections:
- This is normal if you've used both modes
- The MCP will search appropriate collections based on current mode
- Consider removing old collections if storage is a concern

### MCP Not Using New Mode
- Ensure Claude Code was restarted after changing `.env`
- Verify environment: `docker compose exec watcher env | grep PREFER_LOCAL`
- Re-add MCP server if needed

### Import Failures
- Check Docker memory limits (2GB recommended)
- Ensure `.env` file has correct settings
- Verify Voyage API key is valid (for cloud mode)

## Best Practices

1. **Choose Once**: Pick your embedding mode during initial setup and stick with it
2. **Test First**: If you must switch, test with a small subset first
3. **Document Choice**: Note which mode you're using for future reference
4. **Storage Planning**: Account for increased storage if maintaining both modes

## When to Switch Modes

### Switch to Cloud When:
- Search accuracy is critical
- You're comfortable with data being processed externally
- You have reliable internet connection

### Switch to Local When:
- Privacy is paramount
- You need offline functionality
- You want predictable costs (no API usage)

## Conclusion

Switching embedding modes is a significant operation that should be done rarely. For most users, choosing the right mode during initial setup and staying with it is the best approach. This migration process is primarily intended for:

- Developers testing both modes
- Users with changing privacy requirements
- Evaluation of different embedding qualities

For production use, treat your embedding choice as permanent.