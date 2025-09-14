#!/usr/bin/env python3
"""
Claude Code Statusline Health Monitor
Shows real-time session code quality in the CC statusline.
"""

import json
import sys
from pathlib import Path
from datetime import datetime, timedelta
import os

# Add scripts directory to path
sys.path.append(str(Path(__file__).parent))

try:
    from session_quality_tracker import SessionQualityTracker
except ImportError:
    # Fallback if not in venv
    print("⚠️ Session health unavailable")
    sys.exit(0)


def get_session_health():
    """Get current session health for statusline display."""
    try:
        tracker = SessionQualityTracker()

        # Quick analysis (uses cached patterns)
        analysis = tracker.analyze_session_quality()

        if analysis['status'] != 'success':
            return "📊 No active session"

        summary = analysis['summary']
        grade = summary['quality_grade']
        score = summary['avg_quality_score']
        issues = summary['total_issues']

        # Color coding based on grade
        if grade in ['A+', 'A']:
            emoji = '🟢'
        elif grade in ['B', 'C']:
            emoji = '🟡'
        else:
            emoji = '🔴'

        # Compact format for statusline
        if issues > 0:
            return f"{emoji} {grade} ({score:.0%}) | {issues} issues"
        else:
            return f"{emoji} {grade} ({score:.0%})"

    except Exception as e:
        # Silent failure for statusline
        return "📊 Health check failed"


def main():
    """Main entry point for CC statusline."""
    health = get_session_health()
    print(health)


if __name__ == "__main__":
    main()