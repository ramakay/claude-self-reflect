#!/bin/bash

# Claude Self Reflect - Session Start Hook
# Automatically brings current project to 100% indexing on session start
# This hook is triggered when a Claude session starts

set -e

# Get the current working directory (project path)
PROJECT_PATH=$(pwd)
PROJECT_NAME=$(basename "$PROJECT_PATH")

# Python script path
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CSR_ROOT="$(dirname "$SCRIPT_DIR")"
export CSR_ROOT

# Check if we should be quiet (suppress non-essential output)
QUIET_MODE="${CSR_QUIET_MODE:-true}"

# Log function - only logs if not in quiet mode
log() {
    if [[ "$QUIET_MODE" != "true" ]]; then
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1" >&2
    fi
}

log "Session start hook triggered for project: $PROJECT_NAME"

# Check if we're in a Claude-tracked project
if [ ! -d "$HOME/.claude/projects" ]; then
    log "Claude projects directory not found. Skipping indexing."
    # Still need to output JSON for Claude
    echo '{"decision": "approve", "reason": "Not a Claude project"}'
    exit 0
fi

# Activate virtual environment if it exists
if [ -f "$CSR_ROOT/venv/bin/activate" ]; then
    source "$CSR_ROOT/venv/bin/activate"
elif [ -f "$CSR_ROOT/.venv/bin/activate" ]; then
    source "$CSR_ROOT/.venv/bin/activate"
else
    log "Warning: Virtual environment not found"
fi

# Add scripts directory to Python path
export PYTHONPATH="$CSR_ROOT/scripts:$PYTHONPATH"

# Check current project indexing status
log "Checking indexing status for $PROJECT_NAME..."

# Make batch size configurable via environment variable
export CSR_BATCH_SIZE_THRESHOLD="${CSR_BATCH_SIZE_THRESHOLD:-10}"

# Use the external Python script for cleaner code
# Always allow the progress bar to be shown (it handles its own display logic)
if ! python3 "$CSR_ROOT/hooks/session_index_runner.py" "$PROJECT_PATH" "$CSR_ROOT" 2>&1; then
    log "Warning: Session index runner encountered an error, but continuing session"
fi

log "Session start indexing check complete"

# Output JSON response for Claude hooks
# Escape JSON string properly
ESCAPED_PROJECT_NAME=$(echo "$PROJECT_NAME" | sed 's/\\/\\\\/g; s/"/\\"/g; s/\t/\\t/g; s/\n/\\n/g; s/\r/\\r/g')
cat <<EOF
{
  "decision": "approve",
  "reason": "Indexing check completed for $ESCAPED_PROJECT_NAME"
}
EOF