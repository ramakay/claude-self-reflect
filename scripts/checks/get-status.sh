#!/bin/bash
# Direct status script for claude-self-reflect

cd /Users/ramakrishnanannaswamy/projects/claude-self-reflect

# Activate virtual environment and run status
if [ -f ".venv/bin/activate" ]; then
    source .venv/bin/activate
elif [ -f "venv/bin/activate" ]; then
    source venv/bin/activate
fi

python mcp-server/src/status.py 2>/dev/null