#!/bin/bash
# Example: Parse current project and specific project status from claude-self-reflect

# Get the current project name from working directory
CURRENT_PROJECT=$(basename "$(pwd)")
echo "Current project: $CURRENT_PROJECT"

# Get full status JSON
STATUS=$(claude-self-reflect status)

# Get overall status
OVERALL_PERCENT=$(echo "$STATUS" | jq -r '.overall.percentage')
OVERALL_COUNT=$(echo "$STATUS" | jq -r '"\(.overall.indexed)/\(.overall.total)"')

echo "📊 Overall indexing: ${OVERALL_PERCENT}% (${OVERALL_COUNT})"
echo ""

# Function to get project status
get_project_status() {
    local project_name="$1"
    local project_data=$(echo "$STATUS" | jq -r --arg proj "$project_name" '.projects[$proj] // null')
    
    if [ "$project_data" = "null" ]; then
        echo "❌ Project '$project_name' not found in indexing data"
        return 1
    fi
    
    local percentage=$(echo "$project_data" | jq -r '.percentage')
    local indexed=$(echo "$project_data" | jq -r '.indexed')
    local total=$(echo "$project_data" | jq -r '.total')
    
    # Choose appropriate emoji based on percentage
    local status_emoji
    if (( $(echo "$percentage >= 100" | bc -l) )); then
        status_emoji="✅"
    elif (( $(echo "$percentage >= 80" | bc -l) )); then
        status_emoji="🟢"
    elif (( $(echo "$percentage >= 50" | bc -l) )); then
        status_emoji="🟡"
    elif (( $(echo "$percentage > 0" | bc -l) )); then
        status_emoji="🔴"
    else
        status_emoji="⚪"
    fi
    
    echo "$status_emoji $project_name: ${percentage}% (${indexed}/${total})"
}

# Parse current project status
echo "🎯 Current project status:"
get_project_status "$CURRENT_PROJECT"
echo ""

# Parse specific projects
echo "🔍 Specific project statuses:"
get_project_status "procsolve"
get_project_status "claude-self-reflect"
get_project_status "zenmcp"
get_project_status "claude"
echo ""

# Show projects that need attention (less than 50% indexed with more than 10 files)
echo "⚠️  Projects needing attention (< 50% with 10+ files):"
echo "$STATUS" | jq -r '.projects | to_entries[] | select(.value.percentage < 50 and .value.total >= 10) | "🔴 \(.key): \(.value.percentage)% (\(.value.indexed)/\(.value.total))"'
echo ""

# Show top 5 most complete projects
echo "🏆 Top 5 most indexed projects:"
echo "$STATUS" | jq -r '.projects | to_entries | sort_by(.value.percentage) | reverse | .[0:5][] | "✅ \(.key): \(.value.percentage)% (\(.value.indexed)/\(.value.total))"'