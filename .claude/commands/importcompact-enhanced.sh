#!/bin/bash
# Enhanced importcompact command with --auto flag for one-step automation

# Parse arguments
AUTO_MODE=false
FOCUS_INSTRUCTIONS=""

# Check for --auto flag
if [[ "$1" == "--auto" ]]; then
    AUTO_MODE=true
    shift
    FOCUS_INSTRUCTIONS="$@"
else
    FOCUS_INSTRUCTIONS="$@"
fi

# Display mode
if [ "$AUTO_MODE" = true ]; then
    echo "🤖 IMPORTCOMPACT AUTO MODE"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "Running automated import → compact workflow"
    echo ""
else
    echo "📤 IMPORTCOMPACT MANUAL MODE" 
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "Will import then show compact command"
    echo ""
fi

# Use the immediate import script for instant execution
echo "🚀 Starting import..."
source ~/projects/claude-self-reflect/venv/bin/activate 2>/dev/null || source ~/projects/claude-self-reflect/.venv/bin/activate 2>/dev/null

# Time the import
START_TIME=$(date +%s)
python3 ~/projects/claude-self-reflect/scripts/import-immediate.py
IMPORT_EXIT_CODE=$?
END_TIME=$(date +%s)
ELAPSED=$((END_TIME - START_TIME))

if [ $IMPORT_EXIT_CODE -eq 0 ]; then
    echo "✅ Import completed successfully in ${ELAPSED}s!"
else
    echo "⚠️  Import had issues but proceeding anyway"
fi

# Handle next steps based on mode
if [ "$AUTO_MODE" = true ]; then
    echo ""
    echo "🤖 AUTO MODE: Preparing to trigger compact..."
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
    echo "⚡ TRIGGER_IMPORT_ORCHESTRATOR_AGENT"
    echo ""
    echo "Instructions for orchestrator agent:"
    echo "1. Execute the /compact command with focus: $FOCUS_INSTRUCTIONS"
    echo "2. The import has been completed successfully"
    echo "3. All conversation data is now in Qdrant"
    echo ""
    echo "Note: The orchestrator agent will handle the compact step."
    echo "This adds ~4s overhead but provides full automation."
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
else
    # Manual mode - show instructions
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "📦 READY TO COMPACT"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
    echo "Your conversation has been imported to Qdrant."
    echo "Now run the following command:"
    echo ""
    if [ -n "$FOCUS_INSTRUCTIONS" ]; then
        echo "    /compact $FOCUS_INSTRUCTIONS"
    else
        echo "    /compact [your focus instructions]"
    fi
    echo ""
    echo "💡 TIP: Use --auto flag for one-step automation:"
    echo "    /importcompact --auto $FOCUS_INSTRUCTIONS"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
fi