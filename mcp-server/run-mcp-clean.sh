#!/bin/bash
# MCP Server Runner with Clean Output
# This wrapper filters INFO messages from stderr to prevent Claude Desktop from showing them as errors

# Get the directory of this script
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Activate virtual environment if it exists
if [ -d "$SCRIPT_DIR/venv" ]; then
    source "$SCRIPT_DIR/venv/bin/activate"
elif [ -d "$SCRIPT_DIR/../venv" ]; then
    source "$SCRIPT_DIR/../venv/bin/activate"
fi

# Run the MCP server and filter stderr
# INFO messages go to /dev/null, actual errors go to stderr
python -u -m src 2>&1 | while IFS= read -r line; do
    # Check if the line contains INFO level logging
    if [[ "$line" == *"INFO"* ]] && [[ "$line" == *"Starting MCP server"* ]]; then
        # Silently ignore the startup message
        continue
    elif [[ "$line" == *"INFO"* ]]; then
        # Send other INFO messages to stdout (they won't show as errors)
        echo "$line"
    else
        # Send actual errors/warnings to stderr
        echo "$line" >&2
    fi
done