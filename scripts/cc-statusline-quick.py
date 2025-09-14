#!/usr/bin/env python3
"""
Claude Code Statusline Quick Health
Lightweight session health for CC statusline (cached results).
"""

import json
from pathlib import Path
from datetime import datetime, timedelta


def get_cached_health():
    """Get cached session health for ultra-fast statusline display."""
    cache_file = Path.home() / ".claude-self-reflect" / "session_quality.json"

    if not cache_file.exists():
        return "📊 No session data"

    try:
        # Check cache age
        mtime = datetime.fromtimestamp(cache_file.stat().st_mtime)
        age = datetime.now() - mtime

        # Use cache if less than 2 minutes old
        if age > timedelta(minutes=2):
            return "📊 Session data stale"

        with open(cache_file, 'r') as f:
            data = json.load(f)

        if data.get('status') != 'success':
            return "📊 No active session"

        summary = data['summary']
        grade = summary['quality_grade']
        score = summary['avg_quality_score']
        issues = summary['total_issues']

        # Color coding
        if grade in ['A+', 'A']:
            emoji = '🟢'
        elif grade in ['B', 'C']:
            emoji = '🟡'
        else:
            emoji = '🔴'

        # Compact display
        if issues > 0:
            return f"{emoji} {grade} ({score:.0%}) | {issues} issues"
        else:
            return f"{emoji} {grade} ({score:.0%})"

    except Exception:
        return "📊 Health unavailable"


if __name__ == "__main__":
    print(get_cached_health())