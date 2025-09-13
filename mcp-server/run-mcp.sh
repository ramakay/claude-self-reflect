#!/bin/bash
# Run the Python MCP server using FastMCP

# CRITICAL: Capture the original working directory before changing it
# This is where Claude Code is actually running from
export MCP_CLIENT_CWD="$PWD"

# Get the directory of this script
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"

# Navigate to the mcp-server directory
cd "$SCRIPT_DIR"

# CRITICAL: Load .env file from project root if it exists
# This ensures the MCP server uses the same settings as other scripts
if [ -f "../.env" ]; then
    echo "[DEBUG] Loading .env file from project root" >&2
    set -a  # Export all variables
    source ../.env
    set +a  # Stop exporting
else
    echo "[DEBUG] No .env file found, using defaults" >&2
fi

# Set smart defaults if not already set
# These match what the CLI setup wizard uses
if [ -z "$QDRANT_URL" ]; then
    export QDRANT_URL="http://localhost:6333"
    echo "[DEBUG] Using default QDRANT_URL: $QDRANT_URL" >&2
fi

if [ -z "$PREFER_LOCAL_EMBEDDINGS" ]; then
    export PREFER_LOCAL_EMBEDDINGS="true"
    echo "[DEBUG] Using default PREFER_LOCAL_EMBEDDINGS: true (privacy-first)" >&2
fi

if [ -z "$ENABLE_MEMORY_DECAY" ]; then
    export ENABLE_MEMORY_DECAY="false"
    echo "[DEBUG] Using default ENABLE_MEMORY_DECAY: false" >&2
fi

# Check if virtual environment exists
if [ ! -d "venv" ]; then
    echo "Creating virtual environment..."
    python3 -m venv venv
    source venv/bin/activate
    pip install -e .
else
    source venv/bin/activate
fi

# CRITICAL FIX: Pass through environment variables from Claude Code
# These environment variables are set by `claude mcp add -e KEY=value`
# Export them so the Python process can access them
if [ ! -z "$VOYAGE_KEY" ]; then
    export VOYAGE_KEY="$VOYAGE_KEY"
fi

if [ ! -z "$VOYAGE_KEY_2" ]; then
    export VOYAGE_KEY_2="$VOYAGE_KEY_2"
fi

if [ ! -z "$PREFER_LOCAL_EMBEDDINGS" ]; then
    export PREFER_LOCAL_EMBEDDINGS="$PREFER_LOCAL_EMBEDDINGS"
fi

if [ ! -z "$QDRANT_URL" ]; then
    export QDRANT_URL="$QDRANT_URL"
fi

if [ ! -z "$ENABLE_MEMORY_DECAY" ]; then
    export ENABLE_MEMORY_DECAY="$ENABLE_MEMORY_DECAY"
fi

if [ ! -z "$DECAY_WEIGHT" ]; then
    export DECAY_WEIGHT="$DECAY_WEIGHT"
fi

if [ ! -z "$DECAY_SCALE_DAYS" ]; then
    export DECAY_SCALE_DAYS="$DECAY_SCALE_DAYS"
fi

if [ ! -z "$EMBEDDING_MODEL" ]; then
    export EMBEDDING_MODEL="$EMBEDDING_MODEL"
fi

# The embedding manager now handles cache properly in a controlled directory
# Set to 'false' if you want to use HuggingFace instead of Qdrant CDN
if [ -z "$FASTEMBED_SKIP_HUGGINGFACE" ]; then
    export FASTEMBED_SKIP_HUGGINGFACE=true
fi

# Debug: Show what environment variables are being passed
echo "[DEBUG] Environment variables for MCP server:" >&2
echo "[DEBUG] VOYAGE_KEY: ${VOYAGE_KEY:+set}" >&2
echo "[DEBUG] PREFER_LOCAL_EMBEDDINGS: ${PREFER_LOCAL_EMBEDDINGS:-not set}" >&2
echo "[DEBUG] QDRANT_URL: ${QDRANT_URL:-not set}" >&2
echo "[DEBUG] ENABLE_MEMORY_DECAY: ${ENABLE_MEMORY_DECAY:-not set}" >&2

# Quick connectivity check for Qdrant
echo "[DEBUG] Checking Qdrant connectivity at $QDRANT_URL..." >&2
if command -v curl &> /dev/null; then
    # Check root endpoint instead of /health which doesn't exist in Qdrant
    if curl -s -f -m 2 "$QDRANT_URL/" > /dev/null 2>&1; then
        echo "[DEBUG] ✅ Qdrant is reachable at $QDRANT_URL" >&2
    else
        echo "[WARNING] ⚠️  Cannot reach Qdrant at $QDRANT_URL" >&2
        echo "[WARNING] Common fixes:" >&2
        echo "[WARNING]   1. Start Qdrant: docker compose up -d qdrant" >&2
        echo "[WARNING]   2. Check if port is different (e.g., 59999)" >&2
        echo "[WARNING]   3. Update .env file with correct QDRANT_URL" >&2
        echo "[WARNING] Continuing anyway - some features may not work..." >&2
    fi
fi

# Run the MCP server
exec python -m src