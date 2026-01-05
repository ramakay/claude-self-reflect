#!/bin/bash
# =============================================================================
# Ralph Memory Integration - Hook Installation Script
# =============================================================================
# Installs Ralph memory hooks into Claude Code's hook system.
#
# Usage:
#   ./install_hooks.sh           # Install hooks
#   ./install_hooks.sh --check   # Check installation status
#   ./install_hooks.sh --remove  # Remove hooks
# =============================================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
CLAUDE_HOOKS_DIR="$HOME/.claude/hooks"
CLAUDE_SETTINGS="$HOME/.claude/settings.json"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log_info() { echo -e "${GREEN}[INFO]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }

check_installation() {
    echo "=========================================="
    echo "Ralph Memory Integration - Status Check"
    echo "=========================================="

    local all_ok=true

    # Check hooks directory
    if [ -d "$CLAUDE_HOOKS_DIR" ]; then
        log_info "✓ Hooks directory exists: $CLAUDE_HOOKS_DIR"
    else
        log_warn "○ Hooks directory not found: $CLAUDE_HOOKS_DIR"
        all_ok=false
    fi

    # Check for our hooks symlinks or copies
    if [ -f "$CLAUDE_HOOKS_DIR/ralph-session-start.py" ] || [ -L "$CLAUDE_HOOKS_DIR/ralph-session-start.py" ]; then
        log_info "✓ SessionStart hook installed"
    else
        log_warn "○ SessionStart hook not installed"
        all_ok=false
    fi

    if [ -f "$CLAUDE_HOOKS_DIR/ralph-session-end.py" ] || [ -L "$CLAUDE_HOOKS_DIR/ralph-session-end.py" ]; then
        log_info "✓ SessionEnd hook installed"
    else
        log_warn "○ SessionEnd hook not installed"
        all_ok=false
    fi

    # Check settings.json for hook configuration
    if [ -f "$CLAUDE_SETTINGS" ]; then
        if grep -q "ralph" "$CLAUDE_SETTINGS" 2>/dev/null; then
            log_info "✓ Settings.json contains Ralph configuration"
        else
            log_warn "○ Settings.json does not contain Ralph configuration"
            all_ok=false
        fi
    else
        log_warn "○ Settings.json not found"
        all_ok=false
    fi

    # Check source hooks exist
    if [ -f "$PROJECT_ROOT/src/runtime/hooks/session_start_hook.py" ]; then
        log_info "✓ Source hooks available"
    else
        log_error "✗ Source hooks missing!"
        all_ok=false
    fi

    echo ""
    if $all_ok; then
        log_info "All hooks properly installed"
    else
        log_warn "Some hooks are missing. Run: $0 to install"
    fi
}

install_hooks() {
    echo "=========================================="
    echo "Ralph Memory Integration - Installing"
    echo "=========================================="

    # Create hooks directory if needed
    mkdir -p "$CLAUDE_HOOKS_DIR"
    log_info "Created hooks directory: $CLAUDE_HOOKS_DIR"

    # Create symlinks to our hooks
    ln -sf "$PROJECT_ROOT/src/runtime/hooks/session_start_hook.py" "$CLAUDE_HOOKS_DIR/ralph-session-start.py"
    ln -sf "$PROJECT_ROOT/src/runtime/hooks/session_end_hook.py" "$CLAUDE_HOOKS_DIR/ralph-session-end.py"
    log_info "Created hook symlinks"

    # Create or update settings.json with hook configuration
    if [ ! -f "$CLAUDE_SETTINGS" ]; then
        echo '{}' > "$CLAUDE_SETTINGS"
    fi

    # Use Python to safely merge hook configuration
    python3 << PYTHON
import json
from pathlib import Path

settings_path = Path("$CLAUDE_SETTINGS")
project_root = "$PROJECT_ROOT"

# Load existing settings
try:
    settings = json.loads(settings_path.read_text())
except:
    settings = {}

# Ensure hooks section exists
if 'hooks' not in settings:
    settings['hooks'] = {}

# Add Ralph hooks if not present
ralph_hooks = {
    "SessionStart": [{
        "matcher": "startup|resume",
        "hooks": [{
            "type": "command",
            "command": f"{project_root}/venv/bin/python3 {project_root}/src/runtime/hooks/session_start_hook.py 2>/dev/null || true"
        }]
    }],
    "SessionEnd": [{
        "hooks": [{
            "type": "command",
            "command": f"{project_root}/venv/bin/python3 {project_root}/src/runtime/hooks/session_end_hook.py 2>/dev/null || true"
        }]
    }]
}

# Merge (don't overwrite existing hooks)
for hook_type, hook_configs in ralph_hooks.items():
    if hook_type not in settings['hooks']:
        settings['hooks'][hook_type] = []

    # Check if Ralph hook already exists
    existing = settings['hooks'][hook_type]
    ralph_cmd = f"{project_root}/src/runtime/hooks"

    has_ralph = any(
        ralph_cmd in str(h.get('hooks', []))
        for h in existing
    )

    if not has_ralph:
        settings['hooks'][hook_type].extend(hook_configs)
        print(f"  Added {hook_type} hook")
    else:
        print(f"  {hook_type} hook already exists")

# Write back
settings_path.write_text(json.dumps(settings, indent=2))
print("Settings updated successfully")
PYTHON

    log_info "Hook configuration added to settings.json"

    echo ""
    log_info "Installation complete!"
    echo ""
    echo "To verify: $0 --check"
    echo ""
    echo "NOTE: The hooks will activate when:"
    echo "  1. You start a Ralph loop with /ralph-wiggum:ralph-loop"
    echo "  2. The hooks detect .claude/ralph-loop.local.md"
    echo "  3. Session events (start/end) trigger memory operations"
}

remove_hooks() {
    echo "=========================================="
    echo "Ralph Memory Integration - Removing"
    echo "=========================================="

    # Remove symlinks
    rm -f "$CLAUDE_HOOKS_DIR/ralph-session-start.py"
    rm -f "$CLAUDE_HOOKS_DIR/ralph-session-end.py"
    log_info "Removed hook symlinks"

    # Remove from settings.json
    if [ -f "$CLAUDE_SETTINGS" ]; then
        python3 << PYTHON
import json
from pathlib import Path

settings_path = Path("$CLAUDE_SETTINGS")
project_root = "$PROJECT_ROOT"

try:
    settings = json.loads(settings_path.read_text())
except:
    exit(0)

if 'hooks' not in settings:
    exit(0)

# Remove Ralph-related hooks
for hook_type in ['SessionStart', 'SessionEnd']:
    if hook_type in settings['hooks']:
        settings['hooks'][hook_type] = [
            h for h in settings['hooks'][hook_type]
            if project_root not in str(h)
        ]
        if not settings['hooks'][hook_type]:
            del settings['hooks'][hook_type]

if not settings['hooks']:
    del settings['hooks']

settings_path.write_text(json.dumps(settings, indent=2))
print("Settings updated")
PYTHON
        log_info "Removed hook configuration from settings.json"
    fi

    echo ""
    log_info "Removal complete!"
}

# =============================================================================
# MAIN
# =============================================================================
case "${1:-}" in
    --check)
        check_installation
        ;;
    --remove)
        remove_hooks
        ;;
    *)
        install_hooks
        ;;
esac
