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

# Run the MCP server
exec python -m src