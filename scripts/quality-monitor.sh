#!/bin/bash
# Quality monitoring script that runs periodically to update quality metrics

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
QUALITY_FILE="$HOME/.claude-self-reflect/session_quality.json"
LOG_FILE="$HOME/.claude-self-reflect/quality-monitor.log"

# Ensure directories exist
mkdir -p "$HOME/.claude-self-reflect/quality-reports"

# Function to log with timestamp
log_message() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1" >> "$LOG_FILE"
}

# Function to update quality metrics
update_quality() {
    log_message "Starting quality update..."

    # Activate virtual environment
    cd "$PROJECT_ROOT"
    source venv/bin/activate 2>/dev/null || {
        log_message "ERROR: Failed to activate venv"
        return 1
    }

    # Run quality tracker
    python scripts/session_quality_tracker.py > /dev/null 2>&1

    if [ $? -eq 0 ]; then
        # Extract grade and issues from the JSON
        if [ -f "$QUALITY_FILE" ]; then
            GRADE=$(jq -r '.summary.quality_grade // "?"' "$QUALITY_FILE" 2>/dev/null)
            ISSUES=$(jq -r '.summary.total_issues // 0' "$QUALITY_FILE" 2>/dev/null)

            # Adjust grade based on issues (same logic as statusline)
            if [ "$ISSUES" -gt 50 ]; then
                if [[ "$GRADE" == "A+" || "$GRADE" == "A" ]]; then
                    GRADE="B"
                fi
            fi
            if [ "$ISSUES" -gt 100 ]; then
                GRADE="C"
            fi

            log_message "Quality updated: Grade $GRADE with $ISSUES issues"

            # Generate report if quality dropped
            PREV_GRADE_FILE="$HOME/.claude-self-reflect/.last_grade"
            if [ -f "$PREV_GRADE_FILE" ]; then
                PREV_GRADE=$(cat "$PREV_GRADE_FILE")
                if [[ "$GRADE" < "$PREV_GRADE" ]]; then
                    log_message "Quality dropped from $PREV_GRADE to $GRADE - generating report"
                    python scripts/quality-report.py > /dev/null 2>&1
                fi
            fi
            echo "$GRADE" > "$PREV_GRADE_FILE"
        fi
    else
        log_message "ERROR: Quality tracker failed"
    fi
}

# Main monitoring loop
if [ "$1" == "--once" ]; then
    # Run once and exit
    update_quality
else
    # Run continuously every 30 minutes
    log_message "Starting quality monitor (30-minute interval)"
    while true; do
        update_quality
        sleep 1800  # 30 minutes
    done
fi