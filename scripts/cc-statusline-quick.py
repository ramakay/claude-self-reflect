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
    # First check realtime cache (highest priority)
    realtime_cache = Path.home() / ".claude-self-reflect" / "realtime_quality.json"

    if realtime_cache.exists():
        try:
            # Check realtime cache age
            mtime = datetime.fromtimestamp(realtime_cache.stat().st_mtime)
            age = datetime.now() - mtime

            if age < timedelta(minutes=2):  # Fresh realtime data
                with open(realtime_cache, 'r') as f:
                    realtime_data = json.load(f)

                if "session_aggregate" in realtime_data:
                    agg = realtime_data["session_aggregate"]
                    score = agg.get("average_score", 100)
                    issues = agg.get("total_issues", {})
                    total_issues = sum(issues.values())

                    # Determine emoji based on score
                    if score >= 90:
                        emoji = '✨' if total_issues == 0 else '🟢'
                    elif score >= 70:
                        emoji = '🟡'
                    else:
                        emoji = '🔴'

                    # Show score percentage when below threshold
                    if score < 70:
                        return f"{emoji} {score:.0f}% | {total_issues} issues"
                    elif total_issues > 0:
                        return f"{emoji} {total_issues} issues"
                    else:
                        return f"{emoji} (100%)"
        except Exception:
            pass  # Fall back to session_quality.json

    # Fall back to session_quality.json
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