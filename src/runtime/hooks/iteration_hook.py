#!/usr/bin/env python3
"""
Ralph Iteration Hook - Fires BEFORE each Ralph loop iteration.

This hook:
1. Reads current .ralph_state.md (or creates if missing)
2. Searches CSR for this session's previous iterations
3. Generates a "DO NOT RETRY" list from failed approaches
4. Outputs context injection for the next iteration

Triggered by: ralph-wiggum stop hook (needs integration)
Output: Printed to stdout, captured by stop hook for injection

v7.1.9 - Iteration-level memory implementation
"""

import sys
import json
from pathlib import Path
from datetime import datetime

sys.path.insert(0, str(Path(__file__).parent.parent.parent.parent))

from src.runtime.hooks.ralph_state import (
    load_state, save_state, RalphState, is_ralph_session, get_ralph_state_path
)


def get_iteration_context() -> str:
    """Generate context injection for next iteration."""

    state = load_state() or RalphState()

    # Build DO NOT RETRY list
    do_not_retry = []
    if state.failed_approaches:
        do_not_retry = state.failed_approaches[-10:]  # Last 10

    # Build error signatures seen
    error_sigs = list(state.error_signatures.keys())[:5]

    # Build successful patterns
    successes = state.successful_strategies[-5:] if state.successful_strategies else []

    context = f"""
## Ralph Loop Memory (Iteration {state.iteration})

### DO NOT RETRY (Failed in Previous Iterations)
{chr(10).join(f'- {a}' for a in do_not_retry) or '- (none yet)'}

### Error Signatures Seen (Deduplicated)
{chr(10).join(f'- `{e}`' for e in error_sigs) or '- (none yet)'}

### What Has Worked
{chr(10).join(f'- {s}' for s in successes) or '- (none yet)'}

### Current State
- Iteration: {state.iteration}
- Exit Confidence: {state.exit_confidence}%
- Work Type: {state.work_type or 'UNKNOWN'}

**IMPORTANT**: Before starting work, check `.ralph_state.md` for full context.
Update it with your learnings before completing this iteration.
"""
    return context.strip()


def persist_iteration_learning(
    approach: str,
    outcome: str,  # SUCCESS | FAILURE | PARTIAL
    error: str = None,
    learning: str = None
):
    """Persist a single iteration's learning to state file and CSR."""

    state = load_state() or RalphState()

    if outcome == "FAILURE":
        if approach not in state.failed_approaches:
            state.failed_approaches.append(approach)
        if error:
            state.add_error(error)
    elif outcome == "SUCCESS":
        if approach not in state.successful_strategies:
            state.successful_strategies.append(approach)

    if learning:
        state.learnings.append(learning)

    state.iteration += 1
    save_state(state)

    # Also store to CSR for cross-session retrieval
    try:
        project_root = Path(__file__).parent.parent.parent.parent
        mcp_server_path = project_root / "mcp-server" / "src"
        if str(mcp_server_path) not in sys.path:
            sys.path.insert(0, str(mcp_server_path))
        from standalone_client import CSRStandaloneClient
        client = CSRStandaloneClient()

        reflection = f"""Ralph Iteration {state.iteration}:
Approach: {approach}
Outcome: {outcome}
Error: {error or 'None'}
Learning: {learning or 'None'}
"""
        client.store_reflection(
            content=reflection,
            tags=[
                "ralph_iteration",
                f"session_{state.session_id}",
                f"iteration_{state.iteration}",
                f"outcome_{outcome.lower()}"
            ]
        )
    except Exception as e:
        print(f"Warning: Could not store to CSR: {e}", file=sys.stderr)


if __name__ == '__main__':
    if len(sys.argv) > 1 and sys.argv[1] == '--persist':
        # Called with: --persist "approach" "outcome" ["error"] ["learning"]
        approach = sys.argv[2] if len(sys.argv) > 2 else "unknown"
        outcome = sys.argv[3] if len(sys.argv) > 3 else "PARTIAL"
        error = sys.argv[4] if len(sys.argv) > 4 else None
        learning = sys.argv[5] if len(sys.argv) > 5 else None
        persist_iteration_learning(approach, outcome, error, learning)
        print(f"Persisted: {approach} -> {outcome}")
    else:
        # Default: output context for injection
        print(get_iteration_context())
