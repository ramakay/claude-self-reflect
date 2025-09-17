#!/bin/bash
# Run the Python MCP server using FastMCP

# CRITICAL: Capture the original working directory before changing it
# This is where Claude Code is actually running from
export MCP_CLIENT_CWD="$PWD"

# Get the directory of this script
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"

# Navigate to the mcp-server directory
cd "$SCRIPT_DIR"

# CRITICAL: Environment variables priority:
# 1. Command-line args from Claude Code (already in environment)
# 2. .env file (only for missing values)
# 3. Defaults (as fallback)

# Store any command-line provided values BEFORE loading .env
CMDLINE_VOYAGE_KEY="${VOYAGE_KEY:-}"
CMDLINE_PREFER_LOCAL="${PREFER_LOCAL_EMBEDDINGS:-}"
CMDLINE_QDRANT_URL="${QDRANT_URL:-}"

# CRITICAL: If local mode is explicitly requested, skip VOYAGE_KEY from .env
if [ "$CMDLINE_PREFER_LOCAL" = "true" ]; then
    echo "[DEBUG] Local mode explicitly requested - will skip VOYAGE_KEY from .env" >&2
    # Save current VOYAGE_KEY state
    SAVED_VOYAGE_KEY="${VOYAGE_KEY:-}"
fi

# Load .env file for any missing values
if [ -f "../.env" ]; then
    echo "[DEBUG] Loading .env file from project root" >&2
    set -a  # Export all variables
    source ../.env
    set +a  # Stop exporting
else
    echo "[DEBUG] No .env file found, using defaults" >&2
fi

# CRITICAL: Handle local mode by clearing VOYAGE_KEY if local was explicitly requested
if [ "$CMDLINE_PREFER_LOCAL" = "true" ]; then
    unset VOYAGE_KEY
    echo "[DEBUG] Local mode: VOYAGE_KEY cleared to force local embeddings" >&2
elif [ "${CMDLINE_VOYAGE_KEY+x}" ]; then
    # Restore command-line VOYAGE_KEY if it was explicitly set
    export VOYAGE_KEY="$CMDLINE_VOYAGE_KEY"
    if [ -z "$VOYAGE_KEY" ]; then
        echo "[DEBUG] VOYAGE_KEY explicitly set to empty (forcing local mode)" >&2
    else
        echo "[DEBUG] Using command-line VOYAGE_KEY" >&2
    fi
fi

if [ ! -z "$CMDLINE_PREFER_LOCAL" ]; then
    export PREFER_LOCAL_EMBEDDINGS="$CMDLINE_PREFER_LOCAL"
    echo "[DEBUG] Using command-line PREFER_LOCAL_EMBEDDINGS: $PREFER_LOCAL_EMBEDDINGS" >&2
fi

if [ ! -z "$CMDLINE_QDRANT_URL" ]; then
    export QDRANT_URL="$CMDLINE_QDRANT_URL"
    echo "[DEBUG] Using command-line QDRANT_URL: $QDRANT_URL" >&2
fi

# Set smart defaults ONLY if still not set
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
# BUT: Don't export VOYAGE_KEY if we're in local mode
# Note: VOYAGE_KEY might have been unset earlier for local mode, so skip this entirely

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