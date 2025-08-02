#!/bin/bash
# Test script to verify the importcompact implementation

echo "🧪 IMPORTCOMPACT IMPLEMENTATION TEST"
echo "===================================="
echo ""

# Check if command file exists
echo "1. Checking command file existence..."
if [ -f "$HOME/.claude/commands/importcompact.md" ]; then
    echo "✅ Command file exists at ~/.claude/commands/importcompact.md"
else
    echo "❌ Command file NOT FOUND at ~/.claude/commands/importcompact.md"
    exit 1
fi

# Check frontmatter
echo ""
echo "2. Checking command frontmatter..."
if grep -q "argument-hint:" "$HOME/.claude/commands/importcompact.md"; then
    echo "✅ Argument hint is defined"
else
    echo "⚠️  No argument hint defined"
fi

if grep -q "allowed-tools:.*Task" "$HOME/.claude/commands/importcompact.md"; then
    echo "✅ Task tool is allowed (needed for --auto mode)"
else
    echo "❌ Task tool not allowed - auto mode won't work!"
fi

# Check agent existence
echo ""
echo "3. Checking import-orchestrator agent..."
if [ -f "$HOME/projects/claude-self-reflect/.claude/agents/import-orchestrator.md" ]; then
    echo "✅ Import orchestrator agent exists"
else
    echo "❌ Import orchestrator agent NOT FOUND"
fi

# Check import script
echo ""
echo "4. Checking import script..."
if [ -f "$HOME/projects/claude-self-reflect/scripts/import-immediate.py" ]; then
    echo "✅ Import script exists"
    
    # Check if config path is fixed
    if grep -q "default_state_file = os.path.join" "$HOME/projects/claude-self-reflect/scripts/import-conversations-unified.py"; then
        echo "✅ Config path has been fixed"
    else
        echo "⚠️  Config path might still use /config"
    fi
else
    echo "❌ Import script NOT FOUND"
fi

echo ""
echo "===================================="
echo "📋 IMPLEMENTATION SUMMARY:"
echo ""
echo "To use the command after restarting Claude Code:"
echo ""
echo "1. Manual mode (default):"
echo "   /importcompact focus on authentication"
echo "   # Wait for import..."
echo "   /compact focus on authentication"
echo ""
echo "2. Auto mode (one-step):"
echo "   /importcompact --auto focus on authentication"
echo "   # Everything happens automatically!"
echo ""
echo "The --auto flag triggers the import-orchestrator agent"
echo "which handles the compact step automatically."
echo "===================================="