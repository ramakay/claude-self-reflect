#!/usr/bin/env python3
"""
Ralph SessionStart Hook - Searches CSR for relevant past sessions.

Triggered at session start. Uses existing CSR infrastructure to search
for relevant past Ralph sessions and injects context.

Enhanced Features (v7.1+):
- Error-centric search (find solutions to current blockers)
- Anti-pattern injection (surface failed approaches first)
- Winning strategy prioritization
- Multi-signal search (task + errors + patterns)

Input (stdin): JSON with session_id, transcript_path, source
Output (stdout): Context message for Claude
Exit code: 0 = success, 2 = blocking error

Attribution:
    Search patterns inspired by https://github.com/frankbria/ralph-claude-code
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


def get_project_root() -> Path:
    """Dynamically determine project root (works for any installation)."""
    # This file is at: <project_root>/src/runtime/hooks/session_start_hook.py
    return Path(__file__).parent.parent.parent.parent


def _error_signature(error: str) -> str:
    """Extract error signature for deduplication (matches RalphState method)."""
    import re
    sig = re.sub(r'line \d+', 'line N', error)
    sig = re.sub(r'/[\w/.-]+/', '/.../', sig)
    sig = re.sub(r'\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}', 'TIMESTAMP', sig)
    return sig[:100]


def search_past_sessions(task: str, errors: list = None, limit: int = 3) -> dict:
    """Enhanced search: task + errors + anti-patterns."""
    results = {
        'similar_tasks': [],
        'similar_errors': [],
        'anti_patterns': [],
        'winning_strategies': []
    }

    try:
        # Import CSR standalone client (dynamic path for any installation)
        project_root = get_project_root()
        mcp_server_path = project_root / "mcp-server" / "src"
        if str(mcp_server_path) not in sys.path:
            sys.path.insert(0, str(mcp_server_path))
        from standalone_client import CSRStandaloneClient
        client = CSRStandaloneClient()

        # 1. Task-based search (existing behavior)
        task_results = client.search(
            query=f"ralph session: {task}",
            limit=2,
            min_score=0.5
        )
        results['similar_tasks'] = task_results

        # 2. NEW: Error-based search (if we have current errors)
        if errors:
            for error in errors[:2]:  # Top 2 errors only
                sig = _error_signature(error)
                error_results = client.search(
                    query=f"error blocked solved: {sig}",
                    limit=1,
                    min_score=0.6
                )
                results['similar_errors'].extend(error_results)

        # 3. NEW: Anti-patterns search (failed approaches from incomplete sessions)
        anti_results = client.search(
            query=f"failed approach don't retry: {task}",
            limit=2,
            min_score=0.5
        )
        # Filter for incomplete/abandoned sessions
        results['anti_patterns'] = [
            r for r in anti_results
            if r.get('metadata', {}).get('outcome') in ('INCOMPLETE', 'ABANDONED')
        ]

        # 4. NEW: Winning strategies (successful sessions)
        winners = client.search(
            query=f"successful solution completed: {task}",
            limit=1,
            min_score=0.6
        )
        results['winning_strategies'] = [
            r for r in winners
            if r.get('metadata', {}).get('outcome') == 'COMPLETED'
        ]

        return results

    except ImportError:
        logger.warning("CSR standalone client not available, skipping memory search")
        return results
    except Exception as e:
        logger.error(f"Error searching CSR: {e}")
        return results


def format_past_sessions(results: dict) -> str:
    """Format search results with anti-patterns FIRST for fast loop efficiency."""
    # Handle legacy list format
    if isinstance(results, list):
        results = {'similar_tasks': results}

    has_content = any([
        results.get('anti_patterns'),
        results.get('winning_strategies'),
        results.get('similar_errors'),
        results.get('similar_tasks')
    ])

    if not has_content:
        return ""

    output = ["# Ralph Memory (from CSR)\n"]
    output.append("Use these insights to avoid repeating mistakes.\n")

    # 1. ANTI-PATTERNS FIRST (most important for fast loops)
    if results.get('anti_patterns'):
        output.append("## DON'T RETRY THESE")
        for r in results['anti_patterns']:
            # Extract failed approaches from metadata
            failed = r.get('metadata', {}).get('failed_approaches', [])
            if failed:
                for f in failed[:3]:
                    output.append(f"- {f}")
            else:
                # Fallback to content preview
                content = r.get('content', r.get('preview', ''))[:150]
                output.append(f"- {content}...")

    # 2. WINNING STRATEGIES (proven approaches)
    if results.get('winning_strategies'):
        output.append("\n## PROVEN APPROACHES")
        for r in results['winning_strategies']:
            strategies = r.get('metadata', {}).get('successful_strategies', [])
            if strategies:
                for s in strategies[:3]:
                    output.append(f"- {s}")
            else:
                content = r.get('content', r.get('preview', ''))[:150]
                output.append(f"- {content}...")

    # 3. PAST ERROR SOLUTIONS
    if results.get('similar_errors'):
        output.append("\n## PAST ERROR SOLUTIONS")
        for r in results['similar_errors']:
            score = r.get('score', 0)
            content = r.get('content', r.get('preview', ''))[:200]
            output.append(f"- (score: {score:.2f}) {content}...")

    # 4. SIMILAR TASKS (context, less actionable)
    if results.get('similar_tasks'):
        output.append("\n## RELATED SESSIONS")
        for i, r in enumerate(results['similar_tasks'][:2], 1):
            score = r.get('score', 0)
            outcome = r.get('metadata', {}).get('outcome', 'Unknown')
            content = r.get('content', r.get('preview', ''))[:150]
            output.append(f"- [{outcome}] (score: {score:.2f}) {content}...")

            # Extract key learnings
            if learnings := r.get('metadata', {}).get('learnings'):
                for learning in learnings[:2]:
                    output.append(f"  - Learning: {learning}")

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

    # Search for past sessions with similar task AND current errors
    logger.info(f"Searching CSR for past sessions related to: {state.task[:50]}...")
    results = search_past_sessions(
        task=state.task,
        errors=state.blocking_errors if hasattr(state, 'blocking_errors') else None
    )

    # Count total results
    total_results = sum([
        len(results.get('similar_tasks', [])),
        len(results.get('similar_errors', [])),
        len(results.get('anti_patterns', [])),
        len(results.get('winning_strategies', []))
    ])

    if total_results > 0:
        # Write context file for Claude to read
        context = format_past_sessions(results)
        past_sessions_path = Path('.ralph_past_sessions.md')
        past_sessions_path.write_text(context)

        # Log breakdown
        logger.info(f"Found {total_results} relevant results:")
        logger.info(f"  - Anti-patterns: {len(results.get('anti_patterns', []))}")
        logger.info(f"  - Winning strategies: {len(results.get('winning_strategies', []))}")
        logger.info(f"  - Error matches: {len(results.get('similar_errors', []))}")
        logger.info(f"  - Similar tasks: {len(results.get('similar_tasks', []))}")

        print(f"# Loaded {total_results} relevant past sessions")
        print(f"# See .ralph_past_sessions.md for details")
    else:
        logger.info("No relevant past sessions found")

    sys.exit(0)


if __name__ == '__main__':
    main()
