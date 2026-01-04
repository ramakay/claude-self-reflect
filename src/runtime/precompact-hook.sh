#!/bin/bash
# PreCompact hook for Claude Self-Reflect
# Place this in ~/.claude/hooks/precompact or source it from there

# Determine project root dynamically from this script's location
# This file is at: <project_root>/src/runtime/precompact-hook.sh
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEFAULT_PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Configuration (allows override via environment)
CLAUDE_REFLECT_DIR="${CLAUDE_REFLECT_DIR:-$DEFAULT_PROJECT_ROOT}"
# Support both .venv and venv directories
if [ -d "$CLAUDE_REFLECT_DIR/.venv" ]; then
    VENV_PATH="${VENV_PATH:-$CLAUDE_REFLECT_DIR/.venv}"
elif [ -d "$CLAUDE_REFLECT_DIR/venv" ]; then
    VENV_PATH="${VENV_PATH:-$CLAUDE_REFLECT_DIR/venv}"
else
    VENV_PATH="${VENV_PATH:-$CLAUDE_REFLECT_DIR/.venv}"
fi
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
    python '$CLAUDE_REFLECT_DIR/src/runtime/import-latest.py' 2>&1 | \
        grep -E '(Quick import completed|Imported|Warning)' >&2
" || {
    echo "Quick import timed out after ${IMPORT_TIMEOUT}s" >&2
}

# ============================================================
# RALPH MEMORY INTEGRATION (Added for Memory-Augmented Ralph)
# ============================================================

# If Ralph session, backup state to CSR before compaction
# Check both ralph-wiggum format and custom format
RALPH_STATE_FILE=""
if [ -f ".claude/ralph-loop.local.md" ]; then
    RALPH_STATE_FILE=".claude/ralph-loop.local.md"
elif [ -f ".ralph_state.md" ]; then
    RALPH_STATE_FILE=".ralph_state.md"
fi

if [ -n "$RALPH_STATE_FILE" ]; then
    echo "📝 Backing up Ralph state to CSR..." >&2

    RALPH_STATE_FILE="$RALPH_STATE_FILE" CLAUDE_REFLECT_DIR="$CLAUDE_REFLECT_DIR" python3 << 'PYTHON' 2>/dev/null || echo "Warning: Could not backup Ralph state" >&2
import sys
import os
from pathlib import Path
from datetime import datetime

# Dynamic path resolution
project_root = os.environ.get('CLAUDE_REFLECT_DIR', str(Path.home() / 'claude-self-reflect'))
sys.path.insert(0, str(Path(project_root) / 'mcp-server' / 'src'))

try:
    from standalone_client import CSRStandaloneClient

    ralph_state_file = os.environ.get('RALPH_STATE_FILE', '.ralph_state.md')
    state_content = Path(ralph_state_file).read_text()
    client = CSRStandaloneClient()

    client.store_reflection(
        content=f"Pre-compaction Ralph state backup ({datetime.now().isoformat()}):\n\n{state_content}",
        tags=["ralph_state", "pre_compact_backup"]
    )
    print("✅ Ralph state backed up to CSR", file=sys.stderr)
except ImportError:
    print("CSR client not available, skipping backup", file=sys.stderr)
except Exception as e:
    print(f"Backup failed: {e}", file=sys.stderr)
PYTHON
fi

# Always exit successfully to not block compacting
exit 0