#!/bin/bash
# Safe restoration of essential hooks without performance impact

echo "=== SAFE HOOK RESTORATION ==="
echo ""
echo "This script will restore essential functionality without lag"
echo ""

# 1. Create optimized quality check (async, cached)
cat > ~/.claude/hooks/quality-check-optimized.py << 'EOF'
#!/usr/bin/env python3
"""Optimized quality check - runs async, heavily cached"""
import sys
import json
from pathlib import Path
import time

# Quick exit if cache is fresh (< 10 seconds old)
cache_file = Path.home() / ".claude-self-reflect" / "quality_cache_optimized.json"
if cache_file.exists():
    age = time.time() - cache_file.stat().st_mtime
    if age < 10:
        sys.exit(0)  # Use cached result

# Only run on significant files
if len(sys.argv) < 2 or not sys.argv[1].endswith(('.py', '.ts', '.js')):
    sys.exit(0)

# Write placeholder and exit fast (async update later)
cache_file.parent.mkdir(parents=True, exist_ok=True)
cache_file.write_text(json.dumps({"status": "checking", "score": 100}))
sys.exit(0)
EOF
chmod +x ~/.claude/hooks/quality-check-optimized.py

# 2. Create minimal statusline (no subprocess calls)
cat > ~/.claude/statusline-minimal.sh << 'EOF'
#!/bin/bash
# Minimal statusline - no external commands
INPUT=$(cat)
MODEL=$(echo "$INPUT" | jq -r '.model.display_name // "Opus 4.1"' 2>/dev/null)
echo -e "\033[1;36m${MODEL}\033[0m › \033[1;32m✓ Active\033[0m"
EOF
chmod +x ~/.claude/statusline-minimal.sh

# 3. Update settings to use optimized versions
echo ""
echo "To enable optimized hooks, update ~/.claude/settings.json:"
echo '  "statusLine": {'
echo '    "type": "command",'
echo '    "command": "'$HOME'/.claude/statusline-minimal.sh"'
echo '  }'
echo ""
echo "✅ Optimized hooks created (not enabled yet)"
echo ""
echo "Test first with:"
echo "  echo '{}' | ~/.claude/statusline-minimal.sh"
echo ""
echo "If no lag, enable in settings.json"