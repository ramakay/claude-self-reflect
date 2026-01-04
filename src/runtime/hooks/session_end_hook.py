#!/usr/bin/env python3
"""
Ralph SessionEnd Hook - Stores session narrative to CSR.

Triggered at session end. Parses .ralph_state.md, determines outcome,
and stores narrative with metadata for future sessions.

Input (stdin): JSON with session_id, transcript_path, reason
Output: None (cannot block session end)
"""

import sys
import json
import logging
from pathlib import Path
from datetime import datetime

# Add project root to path
sys.path.insert(0, str(Path(__file__).parent.parent.parent.parent))

from src.runtime.hooks.ralph_state import load_state, is_ralph_session, load_ralph_session_state

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)


def get_project_root() -> Path:
    """Dynamically determine project root (works for any installation)."""
    # This file is at: <project_root>/src/runtime/hooks/session_end_hook.py
    return Path(__file__).parent.parent.parent.parent


def store_session_narrative(state, session_id: str, reason: str) -> bool:
    """Store session narrative to CSR."""
    try:
        # Import CSR standalone client (dynamic path for any installation)
        project_root = get_project_root()
        mcp_server_path = project_root / "mcp-server" / "src"
        if str(mcp_server_path) not in sys.path:
            sys.path.insert(0, str(mcp_server_path))
        from standalone_client import CSRStandaloneClient
        client = CSRStandaloneClient()

        # Determine outcome
        if state.completion_promise_met:
            outcome = "COMPLETED"
        elif reason in ('clear', 'logout'):
            outcome = "ABANDONED"
        else:
            outcome = "INCOMPLETE"

        # Generate narrative
        narrative = f"""# Ralph Session Complete

## Metadata
- Session ID: {state.session_id}
- End Reason: {reason}
- Timestamp: {datetime.now().isoformat()}
- Total Iterations: {state.iteration}

## Task
{state.task}

## Outcome: {outcome}
Completion Promise: `{state.completion_promise}`
Promise Met: {state.completion_promise_met}

## Final Approach
{state.current_approach}

## What Worked
{chr(10).join(f'- {s}' for s in state.successful_strategies) or '- (none recorded)'}

## What Failed (Don't Retry These)
{chr(10).join(f'- {f}' for f in state.failed_approaches) or '- (none recorded)'}

## Blocking Errors Encountered
{chr(10).join(f'- {e}' for e in state.blocking_errors) or '- (none recorded)'}

## Key Learnings
{chr(10).join(f'- {l}' for l in state.learnings) or '- (none recorded)'}

## Files Modified
{chr(10).join(f'- {f}' for f in state.files_modified) or '- (none recorded)'}
"""

        # Store with outcome-aware tags
        tags = [
            "ralph_session",
            f"session_{state.session_id}",
            f"outcome_{outcome.lower()}",
            f"iterations_{state.iteration}"
        ]

        client.store_reflection(content=narrative, tags=tags)

        logger.info(f"Stored session narrative: {outcome}, {state.iteration} iterations")

        # If successful, also store the winning strategy separately
        if outcome == "COMPLETED" and state.successful_strategies:
            success_summary = f"""Successful Ralph approach for '{state.task[:100]}':
Approach: {state.current_approach}
Key strategies: {', '.join(state.successful_strategies[:5])}
"""
            client.store_reflection(
                content=success_summary,
                tags=["ralph_success", "winning_strategy"]
            )

        return True

    except ImportError:
        logger.warning("CSR standalone client not available")
        return False
    except Exception as e:
        logger.error(f"Error storing narrative: {e}")
        return False


def cleanup_session_files():
    """Clean up temporary session files."""
    files_to_remove = [
        Path('.ralph_past_sessions.md'),
        Path('.ralph_memories.md')
    ]

    for f in files_to_remove:
        if f.exists():
            try:
                f.unlink()
                logger.info(f"Cleaned up: {f}")
            except Exception as e:
                logger.warning(f"Could not remove {f}: {e}")


def main():
    """Main hook entry point."""
    # Read hook input from stdin
    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        input_data = {}

    session_id = input_data.get('session_id', 'unknown')
    reason = input_data.get('reason', 'other')

    logger.info(f"SessionEnd hook triggered: reason={reason}")

    # Check if this is a Ralph session
    if not is_ralph_session():
        sys.exit(0)

    # Load state (supports both ralph-wiggum and custom formats)
    state = load_ralph_session_state()
    if not state:
        logger.warning("Could not load Ralph state for narrative storage")
        sys.exit(0)

    # Store narrative to CSR
    store_session_narrative(state, session_id, reason)

    # Note: Don't clean up .ralph_state.md - it may be needed for resume
    # Only clean up helper files
    cleanup_session_files()

    sys.exit(0)


if __name__ == '__main__':
    main()
