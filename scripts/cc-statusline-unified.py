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


def get_session_health():
    """Get cached session health."""
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

        if issues > 0:
            return f"{emoji} Code: {grade} ({issues} issues)"
        else:
            return f"{emoji} Code: {grade} Clean"

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