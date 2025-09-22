#!/usr/bin/env python3
"""
Claude Code Unified Statusline
Cycles between indexing status and session health every 5 seconds.
"""

import json
import time
import logging
from pathlib import Path
from datetime import datetime, timedelta
import sys
import os

# Setup logger
logger = logging.getLogger(__name__)
logging.basicConfig(level=logging.INFO, format='%(message)s')

# Configuration
CYCLE_FILE = Path.home() / ".claude-self-reflect" / "statusline_cycle.json"
CYCLE_INTERVAL = 5  # seconds between cycles


def get_import_status():
    """Get current import/indexing status."""
    state_file = Path.home() / ".claude-self-reflect" / "config" / "imported-files.json"

    if not state_file.exists():
        return "📚 Import: Not configured"

    try:
        with open(state_file, 'r') as f:
            state = json.load(f)

        imported = len(state.get("imported_files", {}))

        # Count total JSONL files
        claude_dir = Path.home() / ".claude" / "projects"
        total = 0
        if claude_dir.exists():
            for project_dir in claude_dir.iterdir():
                if project_dir.is_dir():
                    total += len(list(project_dir.glob("*.jsonl")))

        if total == 0:
            return "📚 Import: No files found"

        percent = min(100, (imported / total * 100))

        # Color coding
        if percent >= 95:
            emoji = "✅"
        elif percent >= 50:
            emoji = "🔄"
        else:
            emoji = "⏳"

        return f"{emoji} Indexed: {percent:.0f}% ({imported}/{total})"

    except Exception:
        return "📚 Import: Error"


def categorize_issues(file_reports):
    """
    Categorize issues from AST analysis into critical/medium/low.
    """
    critical = 0
    medium = 0
    low = 0

    for file_path, report in file_reports.items():
        for rec in report.get('recommendations', []):
            if 'print statements' in rec or 'console.log' in rec.lower():
                # Extract count from "Replace N print statements"
                import re
                match = re.search(r'(\d+)', rec)
                if match:
                    low += int(match.group(1))
            elif 'anti-patterns' in rec:
                # Extract count from "Fix N anti-patterns"
                import re
                match = re.search(r'Fix (\d+)', rec)
                if match:
                    medium += int(match.group(1))

        # Check top_issues for severity classification
        for issue in report.get('top_issues', []):
            severity = issue.get('severity', 'medium')
            count = issue.get('count', 0)

            if severity == 'high':
                critical += count
            elif severity == 'medium':
                if 'print' in issue.get('id', '') or 'console' in issue.get('id', ''):
                    low += count
                else:
                    medium += count
            else:
                low += count

    return critical, medium, low


def get_quality_icon(critical=0, medium=0, low=0):
    """
    Determine quality icon based on issue severity counts.
    """
    # Icon selection based on highest severity present
    if critical > 0:
        if critical >= 10:
            return "🔴"  # Red circle - Critical issues need immediate attention
        else:
            return "🟠"  # Orange circle - Some critical issues
    elif medium > 0:
        if medium >= 50:
            return "🟡"  # Yellow circle - Many medium issues
        else:
            return "🟢"  # Green circle - Few medium issues
    elif low > 0:
        if low >= 100:
            return "⚪"  # White circle - Many minor issues (prints)
        else:
            return "✅"  # Check mark - Only minor issues
    else:
        return "✨"  # Sparkles - Perfect, no issues


def format_statusline_quality(critical=0, medium=0, low=0):
    """
    Format statusline with colored dot and labeled numbers.
    """
    icon = get_quality_icon(critical, medium, low)

    # Build count display with labels instead of colors for better compatibility
    counts = []
    if critical > 0:
        counts.append(f"C:{critical}")  # C for critical
    if medium > 0:
        counts.append(f"M:{medium}")    # M for medium
    if low > 0:
        counts.append(f"L:{low}")       # L for low

    if counts:
        return f"{icon} {' '.join(counts)}"
    else:
        return f"{icon}"  # Perfect - no counts needed


def get_session_health():
    """Get cached session health with icon-based quality display."""
    # First check realtime cache (highest priority)
    realtime_cache = Path.home() / ".claude-self-reflect" / "realtime_quality.json"

    if realtime_cache.exists():
        try:
            # Check realtime cache age
            mtime = datetime.fromtimestamp(realtime_cache.stat().st_mtime)
            age = datetime.now() - mtime

            if age < timedelta(minutes=5):  # Fresh realtime data
                with open(realtime_cache, 'r') as f:
                    realtime_data = json.load(f)

                if "session_aggregate" in realtime_data:
                    agg = realtime_data["session_aggregate"]
                    issues = agg.get("total_issues", {})
                    critical = issues.get("critical", 0)
                    medium = issues.get("medium", 0)
                    low = issues.get("low", 0)

                    # Include score in display if significantly below threshold
                    score = agg.get("average_score", 100)
                    if score < 70:
                        # Use red icon for scores below threshold
                        icon = "🔴"
                        return f"{icon} {score:.0f}%"
                    else:
                        return format_statusline_quality(critical, medium, low)
        except Exception:
            pass  # Fall back to session_quality.json

    # Fall back to session_quality.json
    cache_file = Path.home() / ".claude-self-reflect" / "session_quality.json"

    if not cache_file.exists():
        return "💻 Session: No data"

    try:
        # Check cache age
        mtime = datetime.fromtimestamp(cache_file.stat().st_mtime)
        age = datetime.now() - mtime

        if age > timedelta(minutes=5):
            return "💻 Session: Stale data"

        with open(cache_file, 'r') as f:
            data = json.load(f)

        if data.get('status') != 'success':
            return "💻 Session: Inactive"

        # Extract issue counts by severity
        file_reports = data.get('file_reports', {})
        critical, medium, low = categorize_issues(file_reports)

        # Use the icon-based display
        return format_statusline_quality(critical, medium, low)

    except Exception:
        return "💻 Session: Error"


def get_current_cycle():
    """Determine which metric to show based on cycle."""
    # Read or create cycle state
    cycle_state = {"last_update": 0, "current": "import"}

    if CYCLE_FILE.exists():
        try:
            with open(CYCLE_FILE, 'r') as f:
                cycle_state = json.load(f)
        except:
            pass

    # Check if it's time to cycle
    now = time.time()
    if now - cycle_state["last_update"] >= CYCLE_INTERVAL:
        # Toggle between import and health
        cycle_state["current"] = "health" if cycle_state["current"] == "import" else "import"
        cycle_state["last_update"] = now

        # Save state
        CYCLE_FILE.parent.mkdir(exist_ok=True)
        with open(CYCLE_FILE, 'w') as f:
            json.dump(cycle_state, f)

    return cycle_state["current"]


def main():
    """Main entry point for CC statusline."""
    # Check for forced mode
    if len(sys.argv) > 1:
        if sys.argv[1] == "--import":
            logger.info(get_import_status())
        elif sys.argv[1] == "--health":
            logger.info(get_session_health())
        else:
            logger.info("Usage: cc-statusline-unified.py [--import|--health]")
        return

    # Cycle between metrics
    current = get_current_cycle()

    if current == "import":
        logger.info(get_import_status())
    else:
        logger.info(get_session_health())


if __name__ == "__main__":
    main()