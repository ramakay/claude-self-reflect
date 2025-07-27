# Troubleshooting Guide

This guide helps you resolve common issues with Claude Self-Reflect.

## Table of Contents
- [Installation Issues](#installation-issues)
- [Import Problems](#import-problems)
- [MCP Connection Issues](#mcp-connection-issues)
- [Search and Reflection Issues](#search-and-reflection-issues)
- [Performance Issues](#performance-issues)
- [Error Messages](#error-messages)

## Installation Issues

### npm install fails

**Symptom**: `npm install -g claude-self-reflect` fails with permission errors

**Solution**:
- On macOS/Linux: Use `sudo npm install -g claude-self-reflect`
- On Windows: Run terminal as Administrator
- Alternative: Install locally without `-g` flag

### Docker not found or not running

**Symptom**: Setup wizard shows "Docker is not running or not installed"

**Solution**:
1. Install Docker Desktop from [docker.com](https://docker.com)
2. Start Docker Desktop and wait for it to fully initialize (green icon)
3. Verify Docker is running: `docker info`
4. Re-run setup: `claude-self-reflect setup`

### Python version issues

**Symptom**: "Python 3.10+ not found" or syntax errors

**Solution**:
1. Check Python version: `python3 --version`
2. If < 3.10, install newer version from [python.org](https://python.org)
3. On macOS with Homebrew: `brew install python@3.11`
4. Ensure python3 is in PATH

### Virtual environment creation fails

**Symptom**: "Failed to create virtual environment"

**Solution**:
1. Install python3-venv package:
   - Ubuntu/Debian: `sudo apt install python3-venv`
   - macOS: Should be included with Python
2. Check disk space
3. Try manual creation: `python3 -m venv test-venv`

### Python SSL Module Issues

**Symptom**: `ModuleNotFoundError: No module named '_ssl'` or SSL certificate errors

**Common on**: macOS with pyenv, custom Python builds

**Automatic Fix**: The setup wizard automatically detects and fixes this by using brew Python

**Manual Solutions**:

**On macOS with pyenv:**
```bash
# The setup wizard handles this automatically, but for manual fix:
# Install Python with brew (includes SSL support)
brew install python@3.11

# Use brew Python for the virtual environment
/usr/local/opt/python@3.11/bin/python3 -m venv venv
```

**On Ubuntu/Debian:**
```bash
# Install SSL development libraries
sudo apt-get update
sudo apt-get install python3-dev libssl-dev

# Reinstall Python if needed
sudo apt-get install --reinstall python3
```

**Alternative workarounds:**
1. **Use trusted host for pip:**
   ```bash
   pip install --trusted-host pypi.org --trusted-host files.pythonhosted.org <package>
   ```

2. **Set pip to use system certificates:**
   ```bash
   pip config set global.cert $(python -m certifi)
   ```

3. **Create persistent pip config:**
   ```bash
   mkdir -p ~/.config/pip
   cat > ~/.config/pip/pip.conf << EOF
   [global]
   trusted-host = pypi.org files.pythonhosted.org
   EOF
   ```

## Import Problems

### "Claude projects directory not found"

**Symptom**: Import script can't find `~/.claude/projects`

**Solution**:
1. Open Claude Code and create at least one conversation
2. Check if directory exists: `ls ~/.claude/projects`
3. If using custom location, set environment variable:
   ```bash
   export LOGS_DIR="/path/to/your/claude/projects"
   ```

### "VOYAGE_KEY environment variable not set"

**Symptom**: Import fails with missing API key error

**Solution**:
1. Get free API key from [voyageai.com](https://www.voyageai.com/)
2. Add to `.env` file in project root:
   ```
   VOYAGE_KEY=your-api-key-here
   ```
3. Or set temporarily: `export VOYAGE_KEY="your-api-key"`

### Import finds 0 messages in conversations

**Symptom**: Import runs but processes 0 chunks

**Possible causes**:
1. JSONL files are empty or corrupted
2. JQ parsing issues (for Docker-based imports)
3. Conversation format has changed

**Solution**:
1. Check if JSONL files have content:
   ```bash
   head ~/.claude/projects/*/[conversation-id].jsonl
   ```
2. Use streaming importer for large files:
   ```bash
   python scripts/import-conversations-voyage-streaming.py
   ```
3. Check for parsing errors in logs

### Import script dependencies not found

**Symptom**: `ModuleNotFoundError: No module named 'qdrant_client'`

**Solution**:
1. Activate virtual environment:
   ```bash
   source mcp-server/venv/bin/activate  # macOS/Linux
   # or
   mcp-server\venv\Scripts\activate     # Windows
   ```
2. Install dependencies:
   ```bash
   pip install -r scripts/requirements.txt
   ```

## MCP Connection Issues

### MCP tools not appearing in Claude Code

**Symptom**: Reflection tools don't show up after setup

**Solution**:
1. Verify MCP was added correctly:
   ```bash
   claude mcp list
   ```
2. If not listed, re-add with correct path:
   ```bash
   claude mcp add claude-self-reflect "/full/path/to/mcp-server/run-mcp.sh" \
     -e VOYAGE_KEY="your-key" \
     -e QDRANT_URL="http://localhost:6333"
   ```
3. Restart Claude Code completely (quit and reopen)

### "Connection refused" errors

**Symptom**: MCP server can't connect to Qdrant

**Solution**:
1. Check Qdrant is running:
   ```bash
   curl http://localhost:6333/health
   ```
2. If not, start Qdrant:
   ```bash
   docker run -d --name qdrant -p 6333:6333 qdrant/qdrant:latest
   ```
3. Check for port conflicts: `lsof -i :6333`

### MCP server crashes on startup

**Symptom**: MCP connects then immediately disconnects

**Solution**:
1. Check MCP server logs
2. Verify Python environment:
   ```bash
   cd mcp-server
   source venv/bin/activate
   python -m src
   ```
3. Common issues:
   - Missing environment variables
   - Python dependency conflicts
   - Incorrect file permissions

## Embedding Mode Issues

### Switching Between Local and Cloud Embeddings

**Symptom**: Changed PREFER_LOCAL_EMBEDDINGS but search returns no/wrong results

**Problem**: Embedding modes use separate collections with different dimensions

**Solution**:
1. Understand that switching is a major operation:
   - Local uses 384-dimensional vectors
   - Cloud uses 1024-dimensional vectors
   - Collections are completely separate

2. To properly switch modes:
   ```bash
   # Stop everything
   docker compose down
   claude mcp remove claude-self-reflect
   
   # Update .env
   # For local: PREFER_LOCAL_EMBEDDINGS=true
   # For cloud: PREFER_LOCAL_EMBEDDINGS=false
   
   # Restart services
   docker compose up -d
   claude mcp add claude-self-reflect "/path/to/mcp-server/run-mcp.sh"
   
   # Re-import all conversations
   python scripts/import-conversations-unified.py
   ```

3. See [Embedding Migration Guide](embedding-migration.md) for detailed steps

### Collections Not Found After Mode Switch

**Symptom**: Search returns no results after changing embedding mode

**Cause**: MCP is searching for collections with wrong suffix (_local vs _voyage)

**Solution**:
1. Verify which collections exist:
   ```bash
   python scripts/check-collections.py
   ```
2. Ensure MCP server was restarted after env change
3. Check Docker containers are using correct mode:
   ```bash
   docker compose exec watcher env | grep PREFER_LOCAL
   ```

## Search and Reflection Issues

### Search returns no results

**Symptom**: Asking about past conversations yields nothing

**Solution**:
1. Verify conversations were imported:
   ```bash
   python scripts/check-collections.py
   ```
2. Check collection naming matches
3. Try different search queries
4. Lower similarity threshold if needed

### Search results are irrelevant

**Symptom**: Results don't match the query intent

**Solution**:
1. Ensure using correct embedding model (voyage-3.5-lite)
2. Check for embedding dimension mismatches
3. Consider adjusting:
   - Similarity threshold
   - Memory decay settings
   - Search query phrasing

### Memory decay not working as expected

**Symptom**: Old conversations appear with same weight as recent

**Solution**:
1. Verify decay is enabled in configuration
2. Check decay calculation in search results
3. Default half-life is 90 days - adjust if needed

## Performance Issues

### Import is very slow

**Symptom**: Import takes hours for large conversation history

**Solution**:
1. Use streaming importer:
   ```bash
   python scripts/import-conversations-voyage-streaming.py
   ```
2. Import in batches with `--limit` flag
3. Check network speed for API calls
4. Monitor rate limiting

### Search queries are slow

**Symptom**: Reflection takes >5 seconds

**Solution**:
1. Check Qdrant performance
2. Reduce number of collections searched
3. Optimize similarity threshold
4. Consider upgrading Qdrant resources

### High memory usage

**Symptom**: Python process uses excessive RAM

**Solution**:
1. Use streaming importer for large files
2. Reduce batch size in configuration
3. Monitor Docker memory limits
4. Process conversations in smaller chunks

## Error Messages

### "Collection already exists"

**Meaning**: Trying to create a duplicate collection in Qdrant

**Solution**:
- Safe to ignore - collection will be reused
- To reset: Delete collection via Qdrant API

### "Rate limit exceeded"

**Meaning**: Voyage AI API rate limit hit

**Solution**:
1. Wait for rate limit to reset
2. Reduce batch size
3. Add delays between requests
4. Upgrade API plan if needed

### "Dimension mismatch"

**Meaning**: Embedding dimensions don't match collection settings

**Solution**:
1. Ensure using consistent embedding model
2. Check collection was created with correct dimensions (1024)
3. May need to recreate collection

### "Invalid API key"

**Meaning**: Voyage API key is incorrect or expired

**Solution**:
1. Verify key in `.env` file
2. Check for extra spaces or quotes
3. Regenerate key at [voyageai.com](https://www.voyageai.com/)

## Still Having Issues?

If your issue isn't covered here:

1. **Check Logs**: Review detailed error messages
2. **Run Diagnostics**: `claude-self-reflect doctor`
3. **Search Issues**: [GitHub Issues](https://github.com/ramakay/claude-self-reflect/issues)
4. **Ask Community**: [GitHub Discussions](https://github.com/ramakay/claude-self-reflect/discussions)
5. **File Bug Report**: Include:
   - Error messages
   - Steps to reproduce
   - System information
   - Relevant logs

## Debug Mode

For detailed debugging:

```bash
# Enable debug logging
export DEBUG=true

# Run with verbose output
python scripts/import-conversations-voyage.py --verbose
```

This provides additional information for troubleshooting complex issues.