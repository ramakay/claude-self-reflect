#!/bin/bash
# Run the Python MCP server using FastMCP

# CRITICAL: Capture the original working directory before changing it
# This is where Claude Code is actually running from
export MCP_CLIENT_CWD="$PWD"

# Get the directory of this script
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"

# Navigate to the mcp-server directory
cd "$SCRIPT_DIR"

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
echo "[DEBUG] Environment variables for MCP server:"
echo "[DEBUG] VOYAGE_KEY: ${VOYAGE_KEY:+set}"
echo "[DEBUG] PREFER_LOCAL_EMBEDDINGS: ${PREFER_LOCAL_EMBEDDINGS:-not set}"
echo "[DEBUG] QDRANT_URL: ${QDRANT_URL:-not set}"
echo "[DEBUG] ENABLE_MEMORY_DECAY: ${ENABLE_MEMORY_DECAY:-not set}"

# Run the MCP server
exec python -m src