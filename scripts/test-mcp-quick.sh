#!/bin/bash
# Quick test to verify MCP server doesn't hang

set -e

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
PROJECT_ROOT="$( cd "$SCRIPT_DIR/.." && pwd )"
MCP_DIR="$PROJECT_ROOT/mcp-server"

echo "=== Quick MCP Robustness Test ==="
echo

# Test 1: Check if server starts without hanging
echo "Test 1: Starting MCP server (5 second timeout)..."
echo "-----------------------------------------------"

cd "$MCP_DIR"

# Send a simple initialization request
TEST_REQUEST='{"jsonrpc":"2.0","method":"initialize","params":{"protocolVersion":"1.0.0","clientInfo":{"name":"test","version":"1.0"}},"id":1}'

# Use timeout with the MCP server
if command -v timeout >/dev/null 2>&1; then
    echo "$TEST_REQUEST" | timeout 5 ./run-mcp.sh 2>&1 | grep -E "Starting MCP|Embedding|Model|ERROR|Failed" || true
elif command -v gtimeout >/dev/null 2>&1; then
    echo "$TEST_REQUEST" | gtimeout 5 ./run-mcp.sh 2>&1 | grep -E "Starting MCP|Embedding|Model|ERROR|Failed" || true
else
    # Fallback: run in background and kill after delay
    echo "$TEST_REQUEST" | ./run-mcp.sh 2>&1 | grep -E "Starting MCP|Embedding|Model|ERROR|Failed" &
    PID=$!
    sleep 5
    if kill -0 $PID 2>/dev/null; then
        echo "✗ Server appears to be hanging!"
        kill $PID
        exit 1
    fi
    wait $PID 2>/dev/null || true
fi

echo
echo "Test 2: Check cache directory..."
echo "---------------------------------"

CACHE_DIR="$MCP_DIR/.fastembed-cache"
if [ -d "$CACHE_DIR" ]; then
    echo "✓ Cache directory exists: $CACHE_DIR"
    
    # Check for model files
    if [ -d "$CACHE_DIR/models--qdrant--all-MiniLM-L6-v2-onnx" ]; then
        echo "✓ Model cache found"
        
        # Check size
        if command -v du >/dev/null 2>&1; then
            SIZE=$(du -sh "$CACHE_DIR" | cut -f1)
            echo "  Cache size: $SIZE"
        fi
    else
        echo "  Model not yet cached (will download on first real use)"
    fi
    
    # Check for lock files
    LOCK_COUNT=$(find "$CACHE_DIR" -name "*.lock" 2>/dev/null | wc -l)
    if [ "$LOCK_COUNT" -gt 0 ]; then
        echo "⚠ Found $LOCK_COUNT lock files (should be cleaned on startup)"
    fi
else
    echo "Cache directory not yet created: $CACHE_DIR"
    echo "(Will be created on first use)"
fi

echo
echo "Test 3: Check environment settings..."
echo "-------------------------------------"

# Check key environment variables
echo "PREFER_LOCAL_EMBEDDINGS: ${PREFER_LOCAL_EMBEDDINGS:-not set (defaults to true)}"
echo "FASTEMBED_SKIP_HUGGINGFACE: ${FASTEMBED_SKIP_HUGGINGFACE:-not set (defaults to true in run-mcp.sh)}"
echo "VOYAGE_KEY: ${VOYAGE_KEY:+[SET]}"

echo
echo "=== Test Complete ==="
echo
echo "If the server started without hanging (no 'Fetching 5 files: 0%' stuck message),"
echo "then the robustness fix is working correctly!"
echo
echo "To test with Claude Code:"
echo "1. Remove old MCP: claude mcp remove claude-self-reflect"
echo "2. Add updated MCP: claude mcp add claude-self-reflect \"$MCP_DIR/run-mcp.sh\""
echo "3. Restart Claude Code"
echo "4. Test tools: Use reflection tools in Claude Code"