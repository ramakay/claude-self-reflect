#!/usr/bin/env python3
"""
Ralph State File Manager - Schema and parsing for .ralph_state.md

This module provides:
1. State file schema definition
2. Parsing utilities to read/write state
3. Validation for state integrity
"""

import re
import json
from pathlib import Path
from dataclasses import dataclass, field, asdict
from typing import List, Dict, Optional
from datetime import datetime


@dataclass
class RalphState:
    """Schema for .ralph_state.md file content."""

    # Session metadata
    session_id: str = ""
    task: str = ""
    iteration: int = 0
    started_at: str = ""
    updated_at: str = ""

    # Current approach
    current_approach: str = ""

    # History tracking
    failed_approaches: List[str] = field(default_factory=list)
    successful_strategies: List[str] = field(default_factory=list)
    blocking_errors: List[str] = field(default_factory=list)

    # Progress tracking
    files_modified: List[str] = field(default_factory=list)
    learnings: List[str] = field(default_factory=list)

    # Next action
    next_action: str = ""

    # Completion tracking
    completion_promise: str = ""
    completion_promise_met: bool = False

    def to_markdown(self) -> str:
        """Convert state to markdown format for .ralph_state.md"""
        return f"""# Ralph Session State

## Metadata
- **Session ID:** {self.session_id}
- **Task:** {self.task}
- **Iteration:** {self.iteration}
- **Started:** {self.started_at}
- **Updated:** {self.updated_at}

## Current Approach
{self.current_approach}

## Completion Promise
`{self.completion_promise}`
Met: {self.completion_promise_met}

## Failed Approaches (DO NOT RETRY)
{self._list_to_md(self.failed_approaches)}

## Blocking Errors
{self._list_to_md(self.blocking_errors)}

## Successful Strategies
{self._list_to_md(self.successful_strategies)}

## Files Modified
{self._list_to_md(self.files_modified)}

## Learnings
{self._list_to_md(self.learnings)}

## Next Action
{self.next_action}
"""

    def _list_to_md(self, items: List[str]) -> str:
        """Convert list to markdown bullet points."""
        if not items:
            return "- (none yet)"
        return "\n".join(f"- {item}" for item in items)

    @classmethod
    def create_new(cls, task: str, completion_promise: str, session_id: str = None) -> 'RalphState':
        """Create a new state for a fresh Ralph session."""
        import uuid
        return cls(
            session_id=session_id or f"ralph_{datetime.now().strftime('%Y%m%d_%H%M%S')}_{uuid.uuid4().hex[:8]}",
            task=task,
            iteration=1,
            started_at=datetime.now().isoformat(),
            updated_at=datetime.now().isoformat(),
            completion_promise=completion_promise
        )

    @classmethod
    def from_markdown(cls, content: str) -> 'RalphState':
        """Parse markdown content into RalphState object."""
        state = cls()

        # Parse metadata
        if match := re.search(r'\*\*Session ID:\*\*\s*(.+)', content):
            state.session_id = match.group(1).strip()
        if match := re.search(r'\*\*Task:\*\*\s*(.+)', content):
            state.task = match.group(1).strip()
        if match := re.search(r'\*\*Iteration:\*\*\s*(\d+)', content):
            state.iteration = int(match.group(1))
        if match := re.search(r'\*\*Started:\*\*\s*(.+)', content):
            state.started_at = match.group(1).strip()
        if match := re.search(r'\*\*Updated:\*\*\s*(.+)', content):
            state.updated_at = match.group(1).strip()

        # Parse completion promise
        if match := re.search(r'## Completion Promise\n`(.+)`', content):
            state.completion_promise = match.group(1)
        if 'Met: True' in content:
            state.completion_promise_met = True

        # Parse current approach
        if match := re.search(r'## Current Approach\n(.+?)(?=\n##|\Z)', content, re.DOTALL):
            state.current_approach = match.group(1).strip()

        # Parse next action
        if match := re.search(r'## Next Action\n(.+?)(?=\n##|\Z)', content, re.DOTALL):
            state.next_action = match.group(1).strip()

        # Parse lists
        state.failed_approaches = cls._parse_list_section(content, "Failed Approaches")
        state.blocking_errors = cls._parse_list_section(content, "Blocking Errors")
        state.successful_strategies = cls._parse_list_section(content, "Successful Strategies")
        state.files_modified = cls._parse_list_section(content, "Files Modified")
        state.learnings = cls._parse_list_section(content, "Learnings")

        return state

    @staticmethod
    def _parse_list_section(content: str, section_name: str) -> List[str]:
        """Parse a markdown list section."""
        pattern = rf'## {section_name}[^\n]*\n((?:- .+\n?)+)'
        if match := re.search(pattern, content):
            items = []
            for line in match.group(1).strip().split('\n'):
                if line.startswith('- ') and line != '- (none yet)':
                    items.append(line[2:].strip())
            return items
        return []


def load_state(path: Path = None) -> Optional[RalphState]:
    """Load state from .ralph_state.md file."""
    path = path or Path('.ralph_state.md')
    if not path.exists():
        return None
    return RalphState.from_markdown(path.read_text())


def save_state(state: RalphState, path: Path = None) -> None:
    """Save state to .ralph_state.md file."""
    path = path or Path('.ralph_state.md')
    state.updated_at = datetime.now().isoformat()
    path.write_text(state.to_markdown())


def is_ralph_session() -> bool:
    """Check if current directory has an active Ralph session.

    Checks for both:
    - .claude/ralph-loop.local.md (ralph-wiggum plugin)
    - .ralph_state.md (our custom state file)
    """
    return (
        Path('.claude/ralph-loop.local.md').exists() or
        Path('.ralph_state.md').exists()
    )


def get_ralph_state_path() -> Optional[Path]:
    """Get the path to the active Ralph state file.

    Priority:
    1. .claude/ralph-loop.local.md (ralph-wiggum plugin)
    2. .ralph_state.md (custom state)
    """
    ralph_wiggum_path = Path('.claude/ralph-loop.local.md')
    custom_path = Path('.ralph_state.md')

    if ralph_wiggum_path.exists():
        return ralph_wiggum_path
    if custom_path.exists():
        return custom_path
    return None


def parse_ralph_wiggum_state(path: Path) -> Optional[RalphState]:
    """Parse ralph-wiggum's .claude/ralph-loop.local.md format.

    The format is:
    ---
    active: true
    iteration: 1
    max_iterations: 50
    completion_promise: "COMPLETE"
    started_at: "2026-01-04T04:25:46Z"
    ---

    Task description follows...
    """
    content = path.read_text()

    state = RalphState()

    # Parse YAML frontmatter
    import re
    frontmatter_match = re.search(r'^---\n(.+?)\n---\n(.+)', content, re.DOTALL)
    if not frontmatter_match:
        return None

    frontmatter = frontmatter_match.group(1)
    task_content = frontmatter_match.group(2).strip()

    # Parse frontmatter fields
    if match := re.search(r'iteration:\s*(\d+)', frontmatter):
        state.iteration = int(match.group(1))
    if match := re.search(r'max_iterations:\s*(\d+)', frontmatter):
        # Store for reference but not in RalphState dataclass
        pass
    if match := re.search(r'completion_promise:\s*["\']?(.+?)["\']?\s*$', frontmatter, re.MULTILINE):
        state.completion_promise = match.group(1).strip('"\'')
    if match := re.search(r'started_at:\s*["\']?(.+?)["\']?\s*$', frontmatter, re.MULTILINE):
        state.started_at = match.group(1).strip('"\'')

    # Task is the content after frontmatter
    state.task = task_content[:500]  # First 500 chars as task summary

    # Generate session ID from file
    state.session_id = f"ralph_wiggum_{state.started_at.replace(':', '').replace('-', '')[:15]}"

    return state


def load_ralph_session_state() -> Optional[RalphState]:
    """Load Ralph state from whichever format is available.

    Automatically detects and parses:
    - .claude/ralph-loop.local.md (ralph-wiggum format)
    - .ralph_state.md (our custom format)
    """
    path = get_ralph_state_path()
    if not path:
        return None

    if path.name == 'ralph-loop.local.md':
        return parse_ralph_wiggum_state(path)
    else:
        return load_state(path)
