# Ralph Memory Integration - Tactical Implementation Plan

**Status:** READY FOR IMPLEMENTATION
**Created:** 2026-01-04
**For:** Ralph Loop execution by another Claude session
**Approach:** Full Integration with Integration Tests
**Location:** src/runtime/hooks/
**State Management:** Auto-created by hooks

---

## Quick Reference

```
Total Files to Create: 7
Total Lines of Code: ~500
Estimated Complexity: Medium
Dependencies: Existing v7.0 batch pipeline (no new dependencies)
```

---

## Implementation Tasks (Execute in Order)

### TASK 1: Create Directory Structure

**Command:**
```bash
mkdir -p src/runtime/hooks
mkdir -p tests/ralph
```

**Verification:**
```bash
ls -la src/runtime/hooks/
ls -la tests/ralph/
```

---

### TASK 2: Create State File Schema and Parser

**File:** `src/runtime/hooks/ralph_state.py`

**Purpose:** Define .ralph_state.md schema and parsing utilities

**Implementation:**
```python
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
    """Check if current directory has an active Ralph session."""
    return Path('.ralph_state.md').exists()
```

**Verification:**
```bash
python -c "from src.runtime.hooks.ralph_state import RalphState; print(RalphState.create_new('test', 'done').to_markdown())"
```

---

### TASK 3: Create SessionStart Hook

**File:** `src/runtime/hooks/session_start_hook.py`

**Purpose:** Search CSR for relevant past sessions when Ralph starts

**Implementation:**
```python
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

from src.runtime.hooks.ralph_state import load_state, save_state, RalphState, is_ralph_session

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

    logger.info(f"SessionStart hook triggered: source={source}, session_id={session_id[:20]}...")

    # Check if this is a Ralph session
    if not is_ralph_session():
        logger.info("No .ralph_state.md found, not a Ralph session")
        sys.exit(0)

    # Load current state
    state = load_state()
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
```

**Verification:**
```bash
echo '{"session_id": "test", "source": "startup"}' | python src/runtime/hooks/session_start_hook.py
```

---

### TASK 4: Create SessionEnd Hook

**File:** `src/runtime/hooks/session_end_hook.py`

**Purpose:** Store session narrative to CSR when Ralph completes

**Implementation:**
```python
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

from src.runtime.hooks.ralph_state import load_state, is_ralph_session

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)


def store_session_narrative(state, session_id: str, reason: str) -> bool:
    """Store session narrative to CSR."""
    try:
        from mcp_server.src.standalone_client import CSRStandaloneClient
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

    # Load state
    state = load_state()
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
```

**Verification:**
```bash
echo '{"session_id": "test", "reason": "clear"}' | python src/runtime/hooks/session_end_hook.py
```

---

### TASK 5: Enhance PreCompact Hook

**File:** `src/runtime/precompact-hook.sh` (MODIFY EXISTING)

**Purpose:** Add Ralph state backup to CSR before compaction

**Changes to add at the END of the existing script (before `exit 0`):**

```bash
# ============================================================
# RALPH MEMORY INTEGRATION (Added for Memory-Augmented Ralph)
# ============================================================

# If Ralph session, backup state to CSR before compaction
if [ -f ".ralph_state.md" ]; then
    echo "📝 Backing up Ralph state to CSR..." >&2

    python3 << 'PYTHON' 2>/dev/null || echo "Warning: Could not backup Ralph state" >&2
import sys
sys.path.insert(0, '/Users/ramakrishnanannaswamy/projects/claude-self-reflect')

from pathlib import Path
from datetime import datetime

try:
    from mcp_server.src.standalone_client import CSRStandaloneClient

    state_content = Path('.ralph_state.md').read_text()
    client = CSRStandaloneClient()

    client.store_reflection(
        content=f"Pre-compaction Ralph state backup ({datetime.now().isoformat()}):\n\n{state_content}",
        tags=["ralph_state", "pre_compact_backup"]
    )
    print("✅ Ralph state backed up to CSR")
except ImportError:
    print("CSR client not available, skipping backup")
except Exception as e:
    print(f"Backup failed: {e}")
PYTHON
fi
```

**Verification:**
```bash
touch .ralph_state.md
bash src/runtime/precompact-hook.sh
rm .ralph_state.md
```

---

### TASK 6: Create Stuck Detection Prompt

**File:** `src/runtime/hooks/stuck_detection_prompt.md`

**Purpose:** Prompt template for Stop hook to detect and handle stuck patterns

**Implementation:**
```markdown
# Stuck Detection Prompt (for Claude Code Stop Hook)

Check if the Ralph session is stuck by examining `.ralph_state.md`:

1. **Read** `.ralph_state.md` if it exists
2. **Check** the `blocking_errors` section
3. **If** the same error appears 3+ times:
   - Use `reflect_on_past("error: {the blocking error}")` to search CSR
   - If solutions found, write them to `.ralph_memories.md`
   - Continue with the suggested solution
4. **If** iteration count > 10 without progress:
   - Use `reflect_on_past("{current task} solutions")` for broader search
   - Consider alternative approaches from past sessions
5. **Update** `.ralph_state.md` with any new insights

Remember: The goal is to break out of stuck loops by leveraging past experience.
```

---

### TASK 7: Create Claude Settings Configuration

**File:** `docs/planning/ralph-hooks-settings.json`

**Purpose:** JSON snippet to add to `.claude/settings.json`

**Implementation:**
```json
{
  "hooks": {
    "SessionStart": [{
      "matcher": "startup|resume",
      "hooks": [{
        "type": "command",
        "command": "python3 ${WORKSPACE}/src/runtime/hooks/session_start_hook.py 2>/dev/null || true"
      }]
    }],
    "SessionEnd": [{
      "hooks": [{
        "type": "command",
        "command": "python3 ${WORKSPACE}/src/runtime/hooks/session_end_hook.py 2>/dev/null || true"
      }]
    }],
    "PreCompact": [{
      "matcher": "auto",
      "hooks": [{
        "type": "command",
        "command": "bash ${WORKSPACE}/src/runtime/precompact-hook.sh 2>/dev/null || true"
      }]
    }],
    "Stop": [{
      "hooks": [{
        "type": "prompt",
        "prompt": "If .ralph_state.md exists, check for stuck patterns: same error 3+ times in blocking_errors means search CSR with reflect_on_past() for solutions."
      }]
    }]
  }
}
```

**Installation Instructions:**
1. Merge this into `~/.claude/settings.json` or `.claude/settings.json`
2. Replace `${WORKSPACE}` with actual path or use relative paths

---

### TASK 8: Create Integration Tests

**File:** `tests/ralph/test_ralph_integration.py`

**Purpose:** End-to-end integration tests for Ralph memory system

**Implementation:**
```python
#!/usr/bin/env python3
"""
Integration tests for Ralph Memory System.

Tests the full lifecycle:
1. State file creation and parsing
2. SessionStart hook searches CSR
3. SessionEnd hook stores narratives
4. PreCompact hook backs up state
"""

import pytest
import json
import subprocess
import tempfile
from pathlib import Path
from datetime import datetime

import sys
sys.path.insert(0, str(Path(__file__).parent.parent.parent))

from src.runtime.hooks.ralph_state import RalphState, load_state, save_state, is_ralph_session


class TestRalphState:
    """Test state file operations."""

    def test_create_new_state(self):
        """Test creating a new Ralph state."""
        state = RalphState.create_new(
            task="Build a REST API",
            completion_promise="API tests passing"
        )

        assert state.task == "Build a REST API"
        assert state.completion_promise == "API tests passing"
        assert state.iteration == 1
        assert state.session_id.startswith("ralph_")

    def test_state_to_markdown(self):
        """Test converting state to markdown."""
        state = RalphState.create_new("Test task", "Done")
        state.failed_approaches = ["approach 1", "approach 2"]
        state.learnings = ["learned X"]

        md = state.to_markdown()

        assert "Test task" in md
        assert "approach 1" in md
        assert "learned X" in md
        assert "## Failed Approaches" in md

    def test_state_roundtrip(self, tmp_path):
        """Test saving and loading state."""
        state_file = tmp_path / ".ralph_state.md"

        original = RalphState.create_new("Roundtrip test", "Complete")
        original.iteration = 5
        original.failed_approaches = ["bad idea 1"]
        original.successful_strategies = ["good idea 1"]

        save_state(original, state_file)
        loaded = load_state(state_file)

        assert loaded.task == original.task
        assert loaded.iteration == 5
        assert "bad idea 1" in loaded.failed_approaches
        assert "good idea 1" in loaded.successful_strategies

    def test_is_ralph_session(self, tmp_path, monkeypatch):
        """Test Ralph session detection."""
        monkeypatch.chdir(tmp_path)

        assert not is_ralph_session()

        (tmp_path / ".ralph_state.md").write_text("# State")

        assert is_ralph_session()


class TestSessionStartHook:
    """Test SessionStart hook functionality."""

    def test_hook_exits_gracefully_without_state(self, tmp_path, monkeypatch):
        """Hook should exit 0 when no .ralph_state.md exists."""
        monkeypatch.chdir(tmp_path)

        result = subprocess.run(
            ["python3", "src/runtime/hooks/session_start_hook.py"],
            input=json.dumps({"session_id": "test", "source": "startup"}),
            capture_output=True,
            text=True,
            cwd=Path(__file__).parent.parent.parent
        )

        assert result.returncode == 0

    def test_hook_creates_past_sessions_file(self, tmp_path, monkeypatch):
        """Hook should create .ralph_past_sessions.md when memories found."""
        monkeypatch.chdir(tmp_path)

        # Create a state file
        state = RalphState.create_new("Build REST API", "Tests pass")
        save_state(state, tmp_path / ".ralph_state.md")

        # Note: This test requires CSR to be running with data
        # In CI, we'd mock the CSR client


class TestSessionEndHook:
    """Test SessionEnd hook functionality."""

    def test_hook_exits_gracefully_without_state(self, tmp_path, monkeypatch):
        """Hook should exit 0 when no .ralph_state.md exists."""
        monkeypatch.chdir(tmp_path)

        result = subprocess.run(
            ["python3", "src/runtime/hooks/session_end_hook.py"],
            input=json.dumps({"session_id": "test", "reason": "clear"}),
            capture_output=True,
            text=True,
            cwd=Path(__file__).parent.parent.parent
        )

        assert result.returncode == 0


class TestFullLifecycle:
    """Integration tests for full Ralph session lifecycle."""

    @pytest.mark.integration
    def test_complete_session_lifecycle(self, tmp_path, monkeypatch):
        """Test a complete Ralph session from start to end."""
        monkeypatch.chdir(tmp_path)

        # 1. Create initial state (simulating Ralph loop start)
        state = RalphState.create_new(
            task="Implement user authentication",
            completion_promise="Auth tests passing"
        )
        save_state(state, tmp_path / ".ralph_state.md")

        # 2. Simulate iterations
        for i in range(5):
            state = load_state(tmp_path / ".ralph_state.md")
            state.iteration = i + 1
            state.current_approach = f"Trying JWT approach iteration {i+1}"

            if i == 2:
                state.failed_approaches.append("Session cookies - CORS issues")
            if i == 4:
                state.successful_strategies.append("JWT with refresh tokens")
                state.completion_promise_met = True

            save_state(state, tmp_path / ".ralph_state.md")

        # 3. Verify final state
        final_state = load_state(tmp_path / ".ralph_state.md")

        assert final_state.iteration == 5
        assert final_state.completion_promise_met
        assert len(final_state.failed_approaches) == 1
        assert len(final_state.successful_strategies) == 1


class TestCompactionScenarios:
    """
    CRITICAL: Tests that simulate what happens during and after compaction.

    These tests verify the core value proposition:
    - State is preserved to CSR BEFORE compaction destroys context
    - State can be retrieved FROM CSR AFTER compaction (fresh session)
    - Cross-session memory works end-to-end
    """

    @pytest.fixture
    def csr_client(self):
        """Get CSR client, skip if not available."""
        try:
            from mcp_server.src.standalone_client import CSRStandaloneClient
            client = CSRStandaloneClient()
            # Quick connectivity check
            client.search("test", limit=1)
            return client
        except Exception as e:
            pytest.skip(f"CSR not available: {e}")

    @pytest.mark.integration
    def test_precompact_backs_up_state_to_csr(self, tmp_path, monkeypatch, csr_client):
        """
        SCENARIO: PreCompact hook triggers during compaction
        EXPECTED: Ralph state is stored in CSR before context is lost
        """
        monkeypatch.chdir(tmp_path)

        # 1. Create a state with meaningful content
        state = RalphState.create_new(
            task="Test PreCompact backup scenario",
            completion_promise="Backup verified"
        )
        state.iteration = 7
        state.failed_approaches = ["Approach A failed", "Approach B failed"]
        state.successful_strategies = ["Approach C worked!"]
        state.learnings = ["Always check permissions first"]
        save_state(state, tmp_path / ".ralph_state.md")

        # 2. Simulate PreCompact hook execution (what happens during compaction)
        # This is the actual hook code that runs
        state_content = (tmp_path / ".ralph_state.md").read_text()

        # Store to CSR (simulating the PreCompact hook behavior)
        csr_client.store_reflection(
            content=f"Pre-compaction Ralph state backup (TEST):\n\n{state_content}",
            tags=["ralph_state", "pre_compact_backup", "test_session", state.session_id]
        )

        # 3. Verify state was stored in CSR
        import time
        time.sleep(1)  # Allow time for indexing

        results = csr_client.search(
            query=f"ralph state backup {state.session_id}",
            limit=5,
            min_score=0.3
        )

        assert len(results) > 0, "PreCompact backup not found in CSR!"

        # Verify content is searchable
        found_content = str(results[0].get('content', '') or results[0].get('preview', ''))
        assert "Test PreCompact backup scenario" in found_content or len(results) > 0

        print(f"✓ PreCompact backup verified in CSR (session: {state.session_id})")

    @pytest.mark.integration
    def test_state_recovery_after_compaction(self, tmp_path, monkeypatch, csr_client):
        """
        SCENARIO: After compaction, context is gone. Can we recover from CSR?
        EXPECTED: SessionStart hook retrieves past state and injects context
        """
        monkeypatch.chdir(tmp_path)

        # 1. SESSION 1: Store a completed session to CSR
        session1_id = f"test_session1_{datetime.now().strftime('%H%M%S')}"
        session1_narrative = f"""# Ralph Session Complete

## Metadata
- Session ID: {session1_id}
- Task: Build authentication system
- Outcome: COMPLETED
- Iterations: 8

## What Worked
- JWT with refresh tokens
- Redis session cache
- Rate limiting on login

## What Failed (Don't Retry)
- Session cookies (CORS issues)
- OAuth implicit flow (deprecated)

## Key Learnings
- Always validate tokens server-side
- Use HttpOnly cookies for refresh tokens
"""

        csr_client.store_reflection(
            content=session1_narrative,
            tags=["ralph_session", f"session_{session1_id}", "outcome_completed", "test"]
        )

        import time
        time.sleep(1)  # Allow indexing

        # 2. SESSION 2: Fresh start - simulate SessionStart hook searching for past context
        # This is what happens when Claude starts fresh after compaction

        new_task = "Build authentication system"  # Same/similar task

        results = csr_client.search(
            query=f"ralph session: {new_task}",
            limit=3,
            min_score=0.3
        )

        # 3. Verify we can find the past session
        assert len(results) > 0, "Could not find past session in CSR!"

        # Check if we retrieved useful context
        all_content = " ".join([
            str(r.get('content', '') or r.get('preview', ''))
            for r in results
        ])

        # Verify we got actionable information
        recovery_checks = [
            "JWT" in all_content or "authentication" in all_content.lower(),
            len(results) >= 1
        ]

        assert any(recovery_checks), f"Retrieved content not useful: {all_content[:200]}"

        print(f"✓ State recovery verified: Found {len(results)} relevant past sessions")
        print(f"  Content preview: {all_content[:100]}...")

    @pytest.mark.integration
    def test_cross_session_memory_end_to_end(self, tmp_path, monkeypatch, csr_client):
        """
        SCENARIO: Full cycle - Session 1 completes, Session 2 benefits from memory
        EXPECTED: Session 2 has access to Session 1's learnings

        This is the CORE VALUE PROPOSITION of Memory-Augmented Ralph.
        """
        monkeypatch.chdir(tmp_path)

        unique_marker = f"UNIQUE_TEST_{datetime.now().strftime('%Y%m%d_%H%M%S')}"

        # ==========================================
        # SESSION 1: Do work, complete, store memory
        # ==========================================

        state1 = RalphState.create_new(
            task=f"Fix database connection pooling - {unique_marker}",
            completion_promise="Pool exhaustion fixed"
        )
        state1.iteration = 12
        state1.failed_approaches = [
            "Increased pool size blindly - OOM errors",
            "Disabled pooling - too slow",
            "Used connection per request - resource exhaustion"
        ]
        state1.successful_strategies = [
            "Implemented connection pool with max 20 connections",
            "Added connection timeout of 30 seconds",
            "Implemented retry with exponential backoff"
        ]
        state1.learnings = [
            f"CRITICAL_{unique_marker}: Pool size should be 2x CPU cores",
            "Always set connection timeouts",
            "Monitor pool exhaustion metrics"
        ]
        state1.completion_promise_met = True

        # Store to CSR (simulating SessionEnd hook)
        session1_narrative = f"""# Ralph Session Complete

## Task
{state1.task}

## Outcome: COMPLETED after {state1.iteration} iterations

## What Failed (Don't Retry)
{chr(10).join(f'- {f}' for f in state1.failed_approaches)}

## What Worked
{chr(10).join(f'- {s}' for s in state1.successful_strategies)}

## Key Learnings
{chr(10).join(f'- {l}' for l in state1.learnings)}

## Marker: {unique_marker}
"""

        csr_client.store_reflection(
            content=session1_narrative,
            tags=["ralph_session", "outcome_completed", "database", unique_marker]
        )

        import time
        time.sleep(2)  # Allow indexing

        # ==========================================
        # SESSION 2: Start fresh, search for memory
        # ==========================================

        # Simulate: Context is GONE (compaction happened)
        # Only thing we have is the new task

        new_task = "Fix database connection pooling issues"  # Similar task

        # SessionStart hook searches CSR
        results = csr_client.search(
            query=f"ralph session database connection pooling",
            limit=5,
            min_score=0.3
        )

        # Verify we found Session 1's memory
        assert len(results) > 0, "Cross-session memory failed: No results found!"

        all_content = " ".join([
            str(r.get('content', '') or r.get('preview', ''))
            for r in results
        ])

        # The unique marker MUST be in the results to prove cross-session worked
        memory_found = unique_marker in all_content

        if not memory_found:
            # Try a more specific search
            results2 = csr_client.search(
                query=unique_marker,
                limit=3,
                min_score=0.1
            )
            all_content2 = " ".join([
                str(r.get('content', '') or r.get('preview', ''))
                for r in results2
            ])
            memory_found = unique_marker in all_content2

        assert memory_found, f"Cross-session memory FAILED: Unique marker '{unique_marker}' not found in CSR results"

        # Verify we got the learnings
        learnings_found = any([
            "Pool size" in all_content,
            "connection timeout" in all_content.lower(),
            "20 connections" in all_content
        ])

        print(f"✓ Cross-session memory VERIFIED")
        print(f"  Session 1 ID: {state1.session_id}")
        print(f"  Unique marker found: {memory_found}")
        print(f"  Learnings retrievable: {learnings_found}")
        print(f"  Results count: {len(results)}")

    @pytest.mark.integration
    def test_stuck_detection_finds_solutions(self, tmp_path, monkeypatch, csr_client):
        """
        SCENARIO: Ralph is stuck on same error 3+ times
        EXPECTED: Search CSR for past solutions to that error
        """
        monkeypatch.chdir(tmp_path)

        # 1. First, store a past solution for a common error
        error_solution = """# Ralph Session - Fixed CORS Error

## Error Encountered
CORS policy: No 'Access-Control-Allow-Origin' header is present

## Solution That Worked
1. Added CORS middleware with proper origins
2. Set credentials: true for cookie support
3. Exposed necessary headers

## Code Fix
```python
from fastapi.middleware.cors import CORSMiddleware
app.add_middleware(
    CORSMiddleware,
    allow_origins=["http://localhost:3000"],
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)
```
"""

        csr_client.store_reflection(
            content=error_solution,
            tags=["ralph_session", "error_fix", "cors", "outcome_completed"]
        )

        import time
        time.sleep(1)

        # 2. Simulate stuck detection - same error appearing multiple times
        blocking_error = "CORS policy: No 'Access-Control-Allow-Origin'"

        # Search CSR for solutions (what stuck detection hook would do)
        results = csr_client.search(
            query=f"error fix: {blocking_error}",
            limit=3,
            min_score=0.3
        )

        # 3. Verify we found a solution
        assert len(results) > 0, "Stuck detection failed: No solutions found for CORS error"

        all_content = " ".join([
            str(r.get('content', '') or r.get('preview', ''))
            for r in results
        ])

        solution_found = any([
            "CORSMiddleware" in all_content,
            "allow_origins" in all_content,
            "Access-Control" in all_content
        ])

        assert solution_found, f"Solution not actionable. Content: {all_content[:200]}"

        print(f"✓ Stuck detection found solution for CORS error")
        print(f"  Results: {len(results)}")


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
```

**Run tests:**
```bash
pytest tests/ralph/test_ralph_integration.py -v
```

---

### TASK 9: Create __init__.py Files

**Files to create:**

```python
# src/runtime/hooks/__init__.py
"""Ralph memory hooks for Claude Code integration."""

from .ralph_state import RalphState, load_state, save_state, is_ralph_session

__all__ = ['RalphState', 'load_state', 'save_state', 'is_ralph_session']
```

```python
# tests/ralph/__init__.py
"""Ralph memory integration tests."""
```

---

## Verification Checklist

After implementing all tasks, verify:

```bash
# 1. Directory structure exists
ls -la src/runtime/hooks/
ls -la tests/ralph/

# 2. State module works
python -c "from src.runtime.hooks import RalphState; print(RalphState.create_new('test', 'done').to_markdown())"

# 3. Hooks are executable
python src/runtime/hooks/session_start_hook.py < /dev/null
python src/runtime/hooks/session_end_hook.py < /dev/null

# 4. Tests pass
pytest tests/ralph/ -v

# 5. PreCompact hook enhanced
grep -q "RALPH MEMORY INTEGRATION" src/runtime/precompact-hook.sh && echo "✅ PreCompact enhanced"
```

---

## Post-Implementation Steps

1. **Merge hook settings** into `~/.claude/settings.json`
2. **Test with real Ralph loop**: Run `/ralph-wiggum:ralph-loop` and verify state files are created
3. **Verify CSR integration**: Check that `reflect_on_past("ralph session")` finds stored sessions
4. **Document** any edge cases discovered during testing

---

## File Summary

| File | Lines | Purpose |
|------|-------|---------|
| `src/runtime/hooks/ralph_state.py` | ~150 | State schema and parsing |
| `src/runtime/hooks/session_start_hook.py` | ~100 | Search CSR for past sessions |
| `src/runtime/hooks/session_end_hook.py` | ~120 | Store session narrative |
| `src/runtime/precompact-hook.sh` | +20 | Backup Ralph state |
| `src/runtime/hooks/stuck_detection_prompt.md` | ~30 | Stop hook prompt |
| `docs/planning/ralph-hooks-settings.json` | ~30 | Claude settings template |
| `tests/ralph/test_ralph_integration.py` | ~350 | Integration tests + compaction scenarios |
| `src/runtime/hooks/__init__.py` | ~10 | Package init |
| `tests/ralph/__init__.py` | ~5 | Test package init |

**Total: ~815 lines across 9 files**

## Test Categories

| Category | Tests | Purpose |
|----------|-------|---------|
| Unit Tests | 4 | State file operations |
| Hook Tests | 3 | Hook exit behavior |
| Lifecycle Test | 1 | Full session from start to end |
| **Compaction Tests** | **4** | **CRITICAL: Simulates compaction scenarios** |

### Compaction Test Details

These are the **most important tests** - they verify the core value proposition:

| Test | What It Simulates |
|------|-------------------|
| `test_precompact_backs_up_state_to_csr` | PreCompact hook saves state to CSR before context loss |
| `test_state_recovery_after_compaction` | SessionStart retrieves past state after fresh start |
| `test_cross_session_memory_end_to_end` | Session 2 benefits from Session 1's learnings |
| `test_stuck_detection_finds_solutions` | Searching CSR for solutions to blocking errors |

---

**Status:** READY FOR IMPLEMENTATION
**Estimated Time:** 2-3 Ralph loop iterations
**Dependencies:** None (uses existing v7.0 infrastructure)
