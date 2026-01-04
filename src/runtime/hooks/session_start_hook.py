#!/usr/bin/env python3
"""
Ralph SessionStart Hook - Searches CSR for relevant past sessions.

Triggered at session start. Uses existing CSR infrastructure to search
for relevant past Ralph sessions and injects context.

Input (stdin): JSON with session_id, transcript_path, source
Output (stdout): Context message for Claude
Exit code: 0 = success, 2 = blocking error
"""

import sys
import json
import logging
from pathlib import Path
from datetime import datetime

# Add project root to path
sys.path.insert(0, str(Path(__file__).parent.parent.parent.parent))

from src.runtime.hooks.ralph_state import (
    load_state,
    save_state,
    RalphState,
    is_ralph_session,
    load_ralph_session_state,
)

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)


def search_past_sessions(task: str, limit: int = 3) -> list:
    """Search CSR for past Ralph sessions with similar tasks."""
    try:
        # Import CSR standalone client
        from mcp_server.src.standalone_client import CSRStandaloneClient
        client = CSRStandaloneClient()

        # Search for past Ralph sessions
        results = client.search(
            query=f"ralph session: {task}",
            limit=limit,
            min_score=0.5
        )

        # Filter for successful sessions
        successful = [
            r for r in results
            if r.get('metadata', {}).get('outcome') == 'COMPLETED'
        ]

        return successful or results[:limit]

    except ImportError:
        logger.warning("CSR standalone client not available, skipping memory search")
        return []
    except Exception as e:
        logger.error(f"Error searching CSR: {e}")
        return []


def format_past_sessions(results: list) -> str:
    """Format search results as markdown context."""
    if not results:
        return ""

    output = ["# Relevant Past Ralph Sessions (from CSR)\n"]
    output.append("Use these insights to avoid repeating mistakes and leverage successful approaches.\n")

    for i, r in enumerate(results, 1):
        score = r.get('score', 0)
        content = r.get('content', r.get('preview', ''))[:500]
        outcome = r.get('metadata', {}).get('outcome', 'Unknown')

        output.append(f"\n## Past Session {i} (Score: {score:.2f}, Outcome: {outcome})")
        output.append(f"{content}...")

        # Extract key learnings if available
        if learnings := r.get('metadata', {}).get('learnings'):
            output.append("\n**Key Learnings:**")
            for learning in learnings[:3]:
                output.append(f"- {learning}")

    return "\n".join(output)


def main():
    """Main hook entry point."""
    # Read hook input from stdin (official protocol)
    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        input_data = {}

    session_id = input_data.get('session_id', '')
    source = input_data.get('source', 'startup')

    logger.info(f"SessionStart hook triggered: source={source}, session_id={session_id[:20] if session_id else 'none'}...")

    # Check if this is a Ralph session
    if not is_ralph_session():
        logger.info("No Ralph session detected (checked .claude/ralph-loop.local.md and .ralph_state.md)")
        sys.exit(0)

    # Load current state (supports both ralph-wiggum and custom formats)
    state = load_ralph_session_state()
    if not state:
        logger.warning("Could not load Ralph state")
        sys.exit(0)

    # Search for past sessions with similar task
    logger.info(f"Searching CSR for past sessions related to: {state.task[:50]}...")
    results = search_past_sessions(state.task)

    if results:
        # Write context file for Claude to read
        context = format_past_sessions(results)
        past_sessions_path = Path('.ralph_past_sessions.md')
        past_sessions_path.write_text(context)

        logger.info(f"Found {len(results)} relevant past sessions")
        print(f"# Loaded {len(results)} relevant past sessions")
        print(f"# See .ralph_past_sessions.md for details")
    else:
        logger.info("No relevant past sessions found")

    sys.exit(0)


if __name__ == '__main__':
    main()
