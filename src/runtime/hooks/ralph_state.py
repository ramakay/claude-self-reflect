#!/usr/bin/env python3
"""
Ralph State File Manager - Schema and parsing for .ralph_state.md

This module provides:
1. State file schema definition
2. Parsing utilities to read/write state
3. Validation for state integrity

Enhanced Features (v7.1+):
- Output decline detection (circuit breaker pattern)
- Error signature deduplication
- Confidence-based exit signals
- Work type tracking (IMPLEMENTATION/TESTING/DEBUGGING)

Attribution:
    Several patterns in this module were inspired by the excellent work in
    https://github.com/frankbria/ralph-claude-code - a community implementation
    of autonomous AI development loops. We gratefully acknowledge their
    contributions to the Ralph loop ecosystem.
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

    # Output tracking for decline detection (circuit breaker pattern)
    output_lengths: List[int] = field(default_factory=list)

    # Confidence-based exit scoring (0-100)
    exit_confidence: int = 0

    # Work type tracking for filtering
    work_type: str = ""  # IMPLEMENTATION, TESTING, DEBUGGING, DOCUMENTATION

    # Deduplicated error signatures for anti-pattern detection
    error_signatures: Dict[str, int] = field(default_factory=dict)  # sig -> count

    def _error_signature(self, error: str) -> str:
        """Extract error signature for deduplication (removes line numbers, paths)."""
        sig = re.sub(r'line \d+', 'line N', error)
        sig = re.sub(r'/[\w/.-]+/', '/.../', sig)
        sig = re.sub(r'\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}', 'TIMESTAMP', sig)
        return sig[:100]

    def add_error(self, error: str) -> None:
        """Add error with deduplication by signature."""
        sig = self._error_signature(error)
        prev_count = self.error_signatures.get(sig, 0)
        self.error_signatures[sig] = prev_count + 1
        # Only add to blocking_errors if this is a new signature (O(1) vs O(n²))
        if prev_count == 0:
            self.blocking_errors.append(error)

    def track_output(self, length: int) -> None:
        """Track output length for decline detection."""
        self.output_lengths.append(length)
        if len(self.output_lengths) > 10:
            self.output_lengths = self.output_lengths[-10:]

    def output_declining(self, threshold: float = 0.7) -> bool:
        """Check if output is declining (circuit breaker signal)."""
        if len(self.output_lengths) < 3:
            return False
        recent = self.output_lengths[-3:]
        avg_recent = sum(recent) / len(recent)
        earlier = self.output_lengths[:-3]
        if not earlier:
            return False
        avg_earlier = sum(earlier) / len(earlier)
        return avg_recent < (avg_earlier * threshold)

    def update_confidence(self, signals: dict) -> None:
        """Update exit confidence based on multiple signals."""
        score = 0
        if signals.get('all_tasks_complete'):
            score += 40
        if signals.get('tests_passing'):
            score += 20
        if signals.get('no_errors'):
            score += 20
        if signals.get('done_keyword'):
            score += 10
        if signals.get('consecutive_test_only', 0) >= 3:
            score += 10
        self.exit_confidence = min(100, score)

    def increment_iteration(self) -> None:
        """Increment iteration count and update timestamp."""
        self.iteration += 1
        self.updated_at = datetime.now().isoformat()

    def to_markdown(self) -> str:
        """Convert state to markdown format for .ralph_state.md"""
        # Format error signatures for display
        error_sig_display = ""
        if self.error_signatures:
            error_sig_display = "\n".join(
                f"- `{sig}` (x{count})" for sig, count in self.error_signatures.items()
            )
        else:
            error_sig_display = "- (none yet)"

        return f"""# Ralph Session State

## Metadata
- **Session ID:** {self.session_id}
- **Task:** {self.task}
- **Iteration:** {self.iteration}
- **Started:** {self.started_at}
- **Updated:** {self.updated_at}
- **Work Type:** {self.work_type or 'UNKNOWN'}
- **Exit Confidence:** {self.exit_confidence}%

## Current Approach
{self.current_approach}

## Completion Promise
`{self.completion_promise}`
Met: {self.completion_promise_met}

## Failed Approaches (DO NOT RETRY)
{self._list_to_md(self.failed_approaches)}

## Blocking Errors
{self._list_to_md(self.blocking_errors)}

## Error Signatures (Deduplicated)
{error_sig_display}

## Successful Strategies
{self._list_to_md(self.successful_strategies)}

## Files Modified
{self._list_to_md(self.files_modified)}

## Learnings
{self._list_to_md(self.learnings)}

## Output Tracking
- Recent lengths: {self.output_lengths[-5:] if self.output_lengths else []}
- Declining: {self.output_declining()}

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

        # NEW: Parse work type and exit confidence
        if match := re.search(r'\*\*Work Type:\*\*\s*(.+)', content):
            state.work_type = match.group(1).strip()
        if match := re.search(r'\*\*Exit Confidence:\*\*\s*(\d+)', content):
            state.exit_confidence = int(match.group(1))

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

        # NEW: Parse error signatures
        state.error_signatures = cls._parse_error_signatures(content)

        # NEW: Parse output lengths
        if match := re.search(r'Recent lengths:\s*\[([^\]]*)\]', content):
            try:
                lengths_str = match.group(1).strip()
                if lengths_str:
                    state.output_lengths = [int(x.strip()) for x in lengths_str.split(',') if x.strip()]
            except ValueError:
                pass

        return state

    @staticmethod
    def _parse_error_signatures(content: str) -> Dict[str, int]:
        """Parse error signatures section."""
        signatures = {}
        pattern = r'## Error Signatures[^\n]*\n((?:- .+\n?)+)'
        if match := re.search(pattern, content):
            for line in match.group(1).strip().split('\n'):
                # Format: - `signature` (xN)
                sig_match = re.search(r'- `(.+?)` \(x(\d+)\)', line)
                if sig_match:
                    signatures[sig_match.group(1)] = int(sig_match.group(2))
        return signatures

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
    """Check if current directory has an ACTIVE Ralph session.

    Checks for both:
    - .claude/ralph-loop.local.md (ralph-wiggum plugin)
    - .ralph_state.md (our custom state file)

    Returns False if file exists but active: false.
    """
    for path in [Path('.claude/ralph-loop.local.md'), Path('.ralph_state.md')]:
        if path.exists():
            try:
                content = path.read_text()
                # Check for active: false explicitly
                if 'active: false' in content:
                    return False
                # Check for active: true or assume active if file exists without active flag
                if 'active: true' in content or 'active:' not in content:
                    return True
            except Exception:
                pass
    return False


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
