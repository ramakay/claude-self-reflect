#!/bin/bash
# Script to cleanup duplicate MCP processes while keeping one of each type

echo "🔍 Analyzing MCP processes..."

# Function to kill all but the newest process of a type
cleanup_duplicates() {
    local pattern="$1"
    local name="$2"

    # Get all PIDs, keep newest (highest PID), kill the rest
    pids=$(pgrep -f "$pattern" | sort -n | head -n -1)

    if [ -n "$pids" ]; then
        count=$(echo "$pids" | wc -l | tr -d ' ')
        echo "  Found $((count + 1)) $name processes, keeping newest, killing $count duplicates"
        for pid in $pids; do
            echo "    Killing PID $pid"
            kill -TERM "$pid" 2>/dev/null || true
        done
    else
        total=$(ps aux | grep -E "$pattern" | grep -v grep | wc -l)
        echo "  Found $total $name process(es) - OK"
    fi
}

# Count before cleanup
echo "📊 Before cleanup:"
echo "  Total MCP processes: $(ps aux | grep -E 'mcp|run-mcp' | grep -v grep | wc -l)"
echo ""

echo "🧹 Cleaning duplicates..."
cleanup_duplicates "zen-mcp-server/server.py" "zen-mcp"
cleanup_duplicates "mcp-server-playwright" "playwright"
cleanup_duplicates "context7-mcp" "context7"
cleanup_duplicates "memento-mcp" "memento"
cleanup_duplicates "blender-mcp" "blender"
cleanup_duplicates "mantis-mcp" "mantis"

echo ""
echo "📊 After cleanup:"
echo "  Total MCP processes: $(ps aux | grep -E 'mcp|run-mcp' | grep -v grep | wc -l)"

echo ""
echo "✅ Cleanup complete!"
echo ""
echo "⚠️  If typing is still slow:"
echo "  1. Restart Claude Code completely"
echo "  2. Run: claude mcp restart"