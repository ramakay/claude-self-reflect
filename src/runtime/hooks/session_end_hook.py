#!/usr/bin/env python3
"""
Ralph SessionEnd Hook - Stores session narrative to CSR.

Triggered at session end. Parses .ralph_state.md, determines outcome,
and stores narrative with metadata for future sessions.

Enhanced Features (v7.1+):
- Structured status block extraction
- Rich metadata storage (work type, confidence, error signatures)
- Output trend tracking for circuit breaker patterns

Input (stdin): JSON with session_id, transcript_path, reason
Output: None (cannot block session end)

Attribution:
    Status block format inspired by https://github.com/frankbria/ralph-claude-code
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


def extract_status_block(content: str) -> dict:
    """Extract ---RALPH_STATUS--- block if present.

    Format:
    ---RALPH_STATUS---
    STATUS: IN_PROGRESS | COMPLETE | BLOCKED
    WORK_TYPE: IMPLEMENTATION | TESTING | DOCUMENTATION
    EXIT_SIGNAL: true | false
    ---END_RALPH_STATUS---
    """
    import re
    match = re.search(
        r'---RALPH_STATUS---\n(.+?)\n---END_RALPH_STATUS---',
        content, re.DOTALL
    )
    if not match:
        return {}

    status = {}
    for line in match.group(1).strip().split('\n'):
        if ':' in line:
            key, val = line.split(':', 1)
            key = key.strip().lower().replace(' ', '_')
            val = val.strip()
            # Convert boolean strings
            if val.lower() in ('true', 'false'):
                val = val.lower() == 'true'
            # Convert numeric strings
            elif val.isdigit():
                val = int(val)
            status[key] = val
    return status


def get_project_root() -> Path:
    """Dynamically determine project root (works for any installation)."""
    # This file is at: <project_root>/src/runtime/hooks/session_end_hook.py
    return Path(__file__).parent.parent.parent.parent


def store_session_narrative(state, session_id: str, reason: str) -> bool:
    """Store session narrative to CSR with rich metadata."""
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

        # Get enhanced fields if available (new RalphState fields)
        work_type = getattr(state, 'work_type', '') or 'UNKNOWN'
        exit_confidence = getattr(state, 'exit_confidence', 0)
        error_signatures = getattr(state, 'error_signatures', {})
        # Handle output_declining as either method or boolean safely
        output_declining_attr = getattr(state, 'output_declining', None)
        output_declining = output_declining_attr() if callable(output_declining_attr) else False

        # Generate narrative with enhanced metadata
        narrative = f"""# Ralph Session Complete

## Metadata
- Session ID: {state.session_id}
- End Reason: {reason}
- Timestamp: {datetime.now().isoformat()}
- Total Iterations: {state.iteration}
- Work Type: {work_type}
- Exit Confidence: {exit_confidence}%
- Output Trend: {"DECLINING" if output_declining else "STABLE"}

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

## Error Signatures (Deduplicated)
{chr(10).join(f'- `{sig}` (x{count})' for sig, count in error_signatures.items()) or '- (none)'}

## Key Learnings
{chr(10).join(f'- {l}' for l in state.learnings) or '- (none recorded)'}

## Files Modified
{chr(10).join(f'- {f}' for f in state.files_modified) or '- (none recorded)'}
"""

        # Store with outcome-aware tags
        # IMPORTANT: __csr_hook_auto__ is a distinct signature that identifies
        # hook-stored reflections vs manually-stored ones during development
        tags = [
            "__csr_hook_auto__",  # Hook signature - don't manually use this tag!
            "ralph_session",
            f"session_{state.session_id}",
            f"outcome_{outcome.lower()}",
            f"iterations_{state.iteration}",
            f"work_type_{work_type.lower()}"
        ]

        # Rich metadata for better search filtering
        metadata = {
            "outcome": outcome,
            "iterations": state.iteration,
            "work_type": work_type,
            "exit_confidence": exit_confidence,
            "output_declining": output_declining,
            "error_signatures": list(error_signatures.keys()),
            "failed_approaches": state.failed_approaches,
            "successful_strategies": state.successful_strategies,
            "learnings": state.learnings,
            "files_modified": state.files_modified,
        }

        # Note: CSR store_reflection may not support metadata yet,
        # but we include the rich info in the narrative for searchability
        # Use hook-specific collection so random agents don't pollute it
        client.store_reflection(
            content=narrative,
            tags=tags,
            collection="csr_hook_sessions_local"  # Separate from user reflections
        )

        logger.info(f"Stored session narrative: {outcome}, {state.iteration} iterations, confidence={exit_confidence}%")

        # If successful, also store the winning strategy separately
        if outcome == "COMPLETED" and state.successful_strategies:
            success_summary = f"""Successful Ralph approach for '{state.task[:100]}':
Approach: {state.current_approach}
Key strategies: {', '.join(state.successful_strategies[:5])}
Exit confidence: {exit_confidence}%
Iterations: {state.iteration}
"""
            client.store_reflection(
                content=success_summary,
                tags=["__csr_hook_auto__", "ralph_success", "winning_strategy", f"work_type_{work_type.lower()}"],
                collection="csr_hook_sessions_local"
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
