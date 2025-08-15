# MCP Server Setup Guide

## Overview

Claude Self Reflect provides two ways to run the MCP server:
1. **Docker MCP** (Recommended for most users)
2. **Local MCP** (For development and debugging)

## Quick Start (Docker - Recommended)

```bash
# 1. Start Docker services
docker compose --profile watch --profile mcp up -d

# 2. Configure Claude Code to use Docker MCP
claude mcp add claude-self-reflect \
    "/path/to/claude-self-reflect/mcp-server/run-mcp-docker.sh" \
    -e QDRANT_URL="http://localhost:6333"

# 3. Restart Claude Code
```

## Local MCP Setup (Development)

### Prerequisites

```bash
# 1. Create virtual environment
cd mcp-server
python3 -m venv venv
source venv/bin/activate

# 2. Install dependencies
pip install -e .

# 3. Pre-download models to avoid startup delays
cd ..
python scripts/setup-fastembed-cache.py
```

### Configure Claude Code

```bash
claude mcp add claude-self-reflect \
    "/path/to/claude-self-reflect/mcp-server/run-mcp.sh" \
    -e QDRANT_URL="http://localhost:6333"
```

## Troubleshooting

### MCP Server Hangs on "Fetching 5 files"

**Root Cause**: FastEmbed encounters file lock deadlocks when multiple parallel downloads try to write to the default `/tmp/fastembed_cache` directory. The downloads get stuck waiting for locks that never release.

**Solutions**:

1. **Use Docker MCP** (Fastest)
   ```bash
   # Switch to Docker-based MCP
   claude mcp remove claude-self-reflect
   claude mcp add claude-self-reflect "/path/to/run-mcp-docker.sh"
   ```

2. **Force Alternative Download Source**
   ```bash
   # Skip HuggingFace and use Qdrant's CDN
   export FASTEMBED_SKIP_HUGGINGFACE=true
   export HF_HUB_OFFLINE=1
   ```

3. **Pre-download Models**
   ```bash
   # Copy from Docker if available
   python scripts/setup-fastembed-cache.py --method docker
   
   # Or download directly (may use alternative sources)
   python scripts/setup-fastembed-cache.py --method download
   ```

### Embedding Fallback Chain

The MCP server automatically falls back through these options:
1. Local embeddings (FastEmbed) with 30s timeout
2. Voyage AI API (if API key configured)
3. Local embeddings as last resort (if Voyage fails)

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `PREFER_LOCAL_EMBEDDINGS` | `true` | Use local FastEmbed instead of Voyage AI |
| `FASTEMBED_DOWNLOAD_TIMEOUT` | `30` | Seconds to wait for model download |
| `FASTEMBED_SKIP_HUGGINGFACE` | `false` | Skip HuggingFace, use alternative sources |
| `VOYAGE_KEY` | - | Voyage AI API key for cloud embeddings |
| `QDRANT_URL` | `http://localhost:6333` | Qdrant vector database URL |

## When to Use Each Approach

### Use Docker MCP When:
- You want a reliable, production-ready setup
- You're not actively developing the MCP server code
- You want isolation from your local Python environment
- You're experiencing download or dependency issues

### Use Local MCP When:
- You're developing/debugging the MCP server
- You need direct access to logs and debugging tools
- You want faster iteration on code changes
- You have specific Python environment requirements

## Architecture Decisions

### Why Both Options?

1. **Docker MCP**: Provides consistency, isolation, and reliability. Models are pre-cached in the container image, avoiding download issues.

2. **Local MCP**: Essential for development. Direct file access, easier debugging, and immediate code changes without rebuilding containers.

### Download Reliability

The FastEmbed library attempts to download models from multiple sources:
1. HuggingFace Hub (can hang due to network issues)
2. Qdrant CDN (fallback, usually more reliable)

Our solution:
- **Timeout mechanism**: 30-second timeout on downloads
- **Automatic fallback**: Switch to Voyage AI if local fails
- **Pre-download script**: Cache models before first run
- **Environment control**: Skip problematic sources

## Best Practices

1. **For New Users**: Start with Docker MCP - it just works
2. **For Developers**: Use local MCP with pre-downloaded models
3. **For Production**: Docker with health checks and monitoring
4. **For CI/CD**: Docker with fixed versions and cached layers

## Common Issues

### "Failed to initialize any embedding model"
- Check if Qdrant is running: `docker ps | grep qdrant`
- Verify environment variables are set correctly
- Try the fallback: set `PREFER_LOCAL_EMBEDDINGS=false`

### "Model initialization timed out"
- Network issues with HuggingFace
- Set `FASTEMBED_SKIP_HUGGINGFACE=true`
- Or use Docker MCP instead

### "No embedding model initialized"
- Both local and Voyage AI failed
- Check Voyage API key if using cloud embeddings
- Ensure FastEmbed is installed: `pip install fastembed`

## Testing Your Setup

```bash
# Test MCP tools are available
echo '{"jsonrpc":"2.0","method":"tools/list","id":1}' | /path/to/run-mcp.sh

# Or in Claude Code
mcp__claude-self-reflect__reflect_on_past
```