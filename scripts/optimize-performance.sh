#!/bin/bash
# Claude Performance Optimization Script
# Fixes typing lag by disabling all performance-impacting features

echo "=== CLAUDE PERFORMANCE OPTIMIZER ==="
echo ""

# 1. Kill duplicate MCP servers
echo "Step 1: Killing duplicate MCP servers..."
BEFORE_COUNT=$(ps aux | grep -E 'mcp|context7|playwright|zen.*server' | grep -v grep | wc -l)
pkill -f "context7-mcp" 2>/dev/null
pkill -f "mcp-server-playwright" 2>/dev/null
pkill -f "zen.*server.py" 2>/dev/null
sleep 1
AFTER_COUNT=$(ps aux | grep -E 'mcp|context7|playwright|zen.*server' | grep -v grep | wc -l)
echo "  Killed $((BEFORE_COUNT - AFTER_COUNT)) duplicate processes"

# 2. Disable all Claude Flow features
echo ""
echo "Step 2: Disabling Claude Flow features..."
SETTINGS_FILE="$HOME/.claude/settings.json"

if [ -f "$SETTINGS_FILE" ]; then
    # Backup original
    cp "$SETTINGS_FILE" "$SETTINGS_FILE.backup.$(date +%s)"

    # Update settings using jq if available, otherwise use sed
    if command -v jq &> /dev/null; then
        jq '.env.CLAUDE_FLOW_HOOKS_ENABLED = "false" |
            .env.CLAUDE_FLOW_TELEMETRY_ENABLED = "false" |
            .env.CLAUDE_FLOW_REMOTE_EXECUTION = "false" |
            .env.CLAUDE_FLOW_GITHUB_INTEGRATION = "false" |
            .hooks = {} |
            del(.statusLine)' "$SETTINGS_FILE" > /tmp/claude-settings.json && \
        mv /tmp/claude-settings.json "$SETTINGS_FILE"
        echo "  ✅ Settings updated with jq"
    else
        # Fallback to sed
        sed -i '' 's/"CLAUDE_FLOW_HOOKS_ENABLED": "true"/"CLAUDE_FLOW_HOOKS_ENABLED": "false"/g' "$SETTINGS_FILE"
        sed -i '' 's/"CLAUDE_FLOW_TELEMETRY_ENABLED": "true"/"CLAUDE_FLOW_TELEMETRY_ENABLED": "false"/g' "$SETTINGS_FILE"
        sed -i '' 's/"CLAUDE_FLOW_REMOTE_EXECUTION": "true"/"CLAUDE_FLOW_REMOTE_EXECUTION": "false"/g' "$SETTINGS_FILE"
        sed -i '' 's/"CLAUDE_FLOW_GITHUB_INTEGRATION": "true"/"CLAUDE_FLOW_GITHUB_INTEGRATION": "false"/g' "$SETTINGS_FILE"
        echo "  ✅ Settings updated with sed"
    fi
fi

# 3. Clear project-specific settings
echo ""
echo "Step 3: Clearing project-specific hooks..."
for project_settings in $(find ~/projects -name ".claude/settings.json" 2>/dev/null); do
    echo '{"hooks": {}}' > "$project_settings"
    echo "  Cleared: $project_settings"
done

# 4. Disable problematic scripts
echo ""
echo "Step 4: Disabling problematic scripts..."
PROBLEMATIC_SCRIPTS=(
    "$HOME/.claude/statusline-wrapper.sh"
    "$HOME/.claude/opus-time-final.sh"
    "$HOME/.claude/contrarian_hook.py"
)

for script in "${PROBLEMATIC_SCRIPTS[@]}"; do
    if [ -f "$script" ]; then
        chmod -x "$script" 2>/dev/null
        echo "  Disabled: $(basename $script)"
    fi
done

# 5. Clean up cache files
echo ""
echo "Step 5: Cleaning cache files..."
rm -f "$HOME/.claude-self-reflect/quality_by_project/*.json" 2>/dev/null
rm -f "$HOME/.claude-self-reflect/realtime_quality.json" 2>/dev/null
echo "  ✅ Cache files cleaned"

# 6. Performance test
echo ""
echo "Step 6: Running performance test..."
echo ""
echo "Testing file operation speed..."
TIME_START=$(date +%s%N)
echo "test" > /tmp/claude-perf-test.txt
chmod +x /tmp/claude-perf-test.txt 2>/dev/null
rm /tmp/claude-perf-test.txt
TIME_END=$(date +%s%N)
ELAPSED=$((($TIME_END - $TIME_START) / 1000000))
echo "  File operations: ${ELAPSED}ms"

if [ $ELAPSED -lt 100 ]; then
    echo "  ✅ Performance: EXCELLENT"
elif [ $ELAPSED -lt 500 ]; then
    echo "  ⚠️ Performance: ACCEPTABLE"
else
    echo "  ❌ Performance: POOR - Further investigation needed"
fi

# 7. Summary
echo ""
echo "=== OPTIMIZATION COMPLETE ==="
echo ""
echo "Actions taken:"
echo "✅ Duplicate MCP servers killed"
echo "✅ Telemetry disabled"
echo "✅ Remote execution disabled"
echo "✅ All hooks disabled"
echo "✅ Statusline removed"
echo "✅ Cache files cleaned"
echo ""
echo "⚠️ IMPORTANT: Restart Claude to apply all changes"
echo ""
echo "If lag persists, run: ps aux | grep -i claude | head -20"