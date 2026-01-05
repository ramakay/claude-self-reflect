#!/usr/bin/env python3
"""
Ralph Iteration Hook - Fires at EACH iteration boundary via Stop hook.

This is the ONLY viable hook for iteration-level memory because:
- Stop hook fires after EACH Claude response (iteration boundary)
- SessionStart/SessionEnd fire once per terminal session (useless)

When triggered:
1. Store learnings from THIS iteration (with session_id + iteration tag)
2. Retrieve learnings from PREVIOUS iterations of same session
3. Output context for next iteration

Input (stdin): JSON with transcript, etc.
Output (stdout): Context message injected into next iteration
"""

import sys
import json
import logging
import re
from pathlib import Path
from datetime import datetime

# Add project root to path
sys.path.insert(0, str(Path(__file__).parent.parent.parent.parent))

from src.runtime.hooks.ralph_state import (
    is_ralph_session,
    load_ralph_session_state,
)

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)


def get_project_root() -> Path:
    return Path(__file__).parent.parent.parent.parent


def store_iteration_learnings(state, iteration: int) -> bool:
    """Store learnings from current iteration to CSR."""
    try:
        project_root = get_project_root()
        mcp_server_path = project_root / "mcp-server" / "src"
        if str(mcp_server_path) not in sys.path:
            sys.path.insert(0, str(mcp_server_path))
        from standalone_client import CSRStandaloneClient
        client = CSRStandaloneClient()

        # Get working directory (project name)
        cwd = Path.cwd()
        # SECURITY: Sanitize project name to prevent injection
        project_name = re.sub(r'[^a-zA-Z0-9_-]', '_', cwd.name)

        # Get learnings (may be empty)
        learnings = getattr(state, 'learnings', [])

        # Create iteration-specific content (store even if no learnings to track hook fired)
        content = f"""# Iteration {iteration} (Session: {state.session_id})

## Project
{project_name} ({cwd})

## Task
{state.task[:200] if state.task else '(no task)'}

## Learnings
{chr(10).join(f'- {l}' for l in learnings) if learnings else '(none this iteration)'}

## Current Approach
{state.current_approach or '(not set)'}

## Files Modified
{chr(10).join(f'- {f}' for f in state.files_modified) if state.files_modified else '(none)'}
"""

        # Store with iteration-specific tags including project
        tags = [
            "__csr_hook_auto__",  # Hook signature
            f"project_{project_name}",
            f"session_{state.session_id}",
            f"iteration_{iteration}",
            "ralph_iteration"
        ]

        client.store_reflection(
            content=content,
            tags=tags,
            collection="csr_hook_sessions_local"
        )

        logger.info(f"Stored iteration {iteration} for {project_name} session {state.session_id}")
        return True

    except Exception as e:
        logger.error(f"Error storing iteration learnings: {e}")
        return False


def retrieve_iteration_learnings(session_id: str, current_iteration: int) -> list:
    """Retrieve learnings from PREVIOUS iterations of this session."""
    try:
        project_root = get_project_root()
        mcp_server_path = project_root / "mcp-server" / "src"
        if str(mcp_server_path) not in sys.path:
            sys.path.insert(0, str(mcp_server_path))
        from standalone_client import CSRStandaloneClient
        client = CSRStandaloneClient()

        # Get learnings from this session (use hook collection)
        learnings = client.get_session_learnings(
            session_id,
            limit=20,
            collection="csr_hook_sessions_local"
        )

        # Filter to only previous iterations
        previous = [
            l for l in learnings
            if any(f"iteration_{i}" in l.get('tags', [])
                   for i in range(1, current_iteration))
        ]

        return previous

    except Exception as e:
        logger.error(f"Error retrieving iteration learnings: {e}")
        return []


def format_iteration_context(learnings: list, current_iteration: int) -> str:
    """Format previous iteration learnings for injection."""
    if not learnings:
        return ""

    output = [f"# Previous Iteration Learnings (for Iteration {current_iteration})"]
    output.append("")

    for l in learnings[:5]:  # Max 5 previous iterations
        content = l.get('content', '')[:500]
        tags = l.get('tags', [])
        iter_tag = [t for t in tags if t.startswith('iteration_')]
        iter_num = iter_tag[0].split('_')[1] if iter_tag else '?'
        output.append(f"## From Iteration {iter_num}")
        output.append(content)
        output.append("")

    return "\n".join(output)


def main():
    """Main hook entry point - fires at each iteration boundary."""
    # Read hook input
    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        input_data = {}

    # Check if Ralph loop active
    if not is_ralph_session():
        logger.info("No Ralph session - iteration hook skipped")
        sys.exit(0)

    # Load state
    state = load_ralph_session_state()
    if not state:
        logger.warning("Could not load Ralph state")
        sys.exit(0)

    iteration = state.iteration
    session_id = state.session_id

    logger.info(f"Iteration hook: session={session_id}, iteration={iteration}")

    # 1. Store learnings from THIS iteration
    store_iteration_learnings(state, iteration)

    # 2. Retrieve learnings from PREVIOUS iterations
    previous_learnings = retrieve_iteration_learnings(session_id, iteration)

    # 3. Output context for NEXT iteration
    if previous_learnings:
        context = format_iteration_context(previous_learnings, iteration + 1)
        # Write to file for Claude to read
        context_path = Path('.ralph_iteration_context.md')
        context_path.write_text(context)
        print(f"# Loaded {len(previous_learnings)} learnings from previous iterations")
        print(f"# See .ralph_iteration_context.md for details")
    else:
        logger.info("No previous iteration learnings found")


if __name__ == "__main__":
    main()
