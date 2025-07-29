#!/bin/bash
# PreCompact hook for Claude Self-Reflect
# Place this in ~/.claude/hooks/precompact or source it from there

# Configuration
CLAUDE_REFLECT_DIR="${CLAUDE_REFLECT_DIR:-$HOME/claude-self-reflect}"
VENV_PATH="${VENV_PATH:-$CLAUDE_REFLECT_DIR/.venv}"
IMPORT_TIMEOUT="${IMPORT_TIMEOUT:-30}"

# Check if Claude Self-Reflect is installed
if [ ! -d "$CLAUDE_REFLECT_DIR" ]; then
    echo "Claude Self-Reflect not found at $CLAUDE_REFLECT_DIR" >&2
    exit 0  # Exit gracefully
fi

# Check if virtual environment exists
if [ ! -d "$VENV_PATH" ]; then
    echo "Virtual environment not found at $VENV_PATH" >&2
    exit 0  # Exit gracefully
fi

# Run quick import with timeout
echo "Updating conversation memory..." >&2
timeout $IMPORT_TIMEOUT bash -c "
    source '$VENV_PATH/bin/activate' 2>/dev/null
    python '$CLAUDE_REFLECT_DIR/scripts/import-latest.py' 2>&1 | \
        grep -E '(Quick import completed|Imported|Warning)' >&2
" || {
    echo "Quick import timed out after ${IMPORT_TIMEOUT}s" >&2
}

# Always exit successfully to not block compacting
exit 0