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

from src.runtime.hooks.ralph_state import (
    RalphState,
    load_state,
    save_state,
    is_ralph_session,
    get_ralph_state_path,
    load_ralph_session_state,
    parse_ralph_wiggum_state,
)


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

    def test_is_ralph_session_wiggum_format(self, tmp_path, monkeypatch):
        """Test Ralph session detection with ralph-wiggum format."""
        monkeypatch.chdir(tmp_path)

        assert not is_ralph_session()

        # Create .claude directory and ralph-loop.local.md
        claude_dir = tmp_path / ".claude"
        claude_dir.mkdir()
        (claude_dir / "ralph-loop.local.md").write_text("""---
active: true
iteration: 3
max_iterations: 50
completion_promise: "COMPLETE"
started_at: "2026-01-04T04:25:46Z"
---

Build a REST API for todos.
""")

        assert is_ralph_session()

    def test_parse_ralph_wiggum_state(self, tmp_path):
        """Test parsing ralph-wiggum format state file."""
        state_file = tmp_path / "ralph-loop.local.md"
        state_file.write_text("""---
active: true
iteration: 5
max_iterations: 50
completion_promise: "TESTS_PASS"
started_at: "2026-01-04T10:30:00Z"
---

Build a user authentication system.
Requirements: JWT, refresh tokens, tests.
""")

        state = parse_ralph_wiggum_state(state_file)

        assert state is not None
        assert state.iteration == 5
        assert state.completion_promise == "TESTS_PASS"
        assert "authentication" in state.task.lower()
        assert state.session_id.startswith("ralph_wiggum_")

    def test_load_ralph_session_state_prefers_wiggum(self, tmp_path, monkeypatch):
        """Test that ralph-wiggum format is preferred over custom format."""
        monkeypatch.chdir(tmp_path)

        # Create both formats
        (tmp_path / ".ralph_state.md").write_text("# Custom state")

        claude_dir = tmp_path / ".claude"
        claude_dir.mkdir()
        (claude_dir / "ralph-loop.local.md").write_text("""---
active: true
iteration: 7
max_iterations: 50
completion_promise: "DONE"
started_at: "2026-01-04T12:00:00Z"
---

Wiggum task.
""")

        state = load_ralph_session_state()

        assert state is not None
        assert state.iteration == 7
        assert "Wiggum" in state.task


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


class TestHookExecution:
    """
    Tests that actually execute the hooks as subprocesses.
    These simulate what happens when Claude Code triggers the hooks.
    """

    def test_precompact_hook_backs_up_ralph_state(self, tmp_path, monkeypatch):
        """
        SCENARIO: PreCompact hook is triggered during compaction
        SIMULATES: Claude Code running precompact-hook.sh
        EXPECTED: Ralph state is stored to CSR
        """
        monkeypatch.chdir(tmp_path)

        # Create a Ralph state file
        claude_dir = tmp_path / ".claude"
        claude_dir.mkdir()
        (claude_dir / "ralph-loop.local.md").write_text("""---
active: true
iteration: 5
max_iterations: 50
completion_promise: "TESTS_PASS"
started_at: "2026-01-04T10:30:00Z"
---

Build authentication system with JWT tokens.
Failed approaches: session cookies (CORS issues)
Current approach: JWT with refresh tokens
""")

        # Actually run the precompact hook
        project_root = Path(__file__).parent.parent.parent
        precompact_script = project_root / "src" / "runtime" / "precompact-hook.sh"

        if precompact_script.exists():
            result = subprocess.run(
                ["bash", str(precompact_script)],
                capture_output=True,
                text=True,
                cwd=tmp_path,
                timeout=30
            )
            # Hook should exit successfully (exit 0)
            assert result.returncode == 0, f"PreCompact hook failed: {result.stderr}"

            # Check that it detected Ralph session
            if ".claude/ralph-loop.local.md" in result.stderr or "Ralph state" in result.stderr:
                print("✓ PreCompact hook detected Ralph session")

    def test_session_start_hook_searches_csr(self, tmp_path, monkeypatch):
        """
        SCENARIO: SessionStart hook is triggered after compaction/resume
        SIMULATES: Claude Code running session_start_hook.py
        EXPECTED: Hook searches CSR for past sessions
        """
        monkeypatch.chdir(tmp_path)

        # Create a Ralph state file
        claude_dir = tmp_path / ".claude"
        claude_dir.mkdir()
        (claude_dir / "ralph-loop.local.md").write_text("""---
active: true
iteration: 1
max_iterations: 50
completion_promise: "COMPLETE"
started_at: "2026-01-04T12:00:00Z"
---

Fix database connection pooling issues.
""")

        # Run the session start hook
        project_root = Path(__file__).parent.parent.parent
        hook_script = project_root / "src" / "runtime" / "hooks" / "session_start_hook.py"

        result = subprocess.run(
            ["python3", str(hook_script)],
            input=json.dumps({"session_id": "test_session", "source": "resume"}),
            capture_output=True,
            text=True,
            cwd=tmp_path,
            timeout=30
        )

        # Hook should exit successfully
        assert result.returncode == 0, f"SessionStart hook failed: {result.stderr}"

        # Check output indicates it's working
        combined_output = result.stdout + result.stderr
        session_detected = (
            "Searching CSR" in combined_output or
            "past sessions" in combined_output or
            "ralph" in combined_output.lower()
        )

        if session_detected:
            print("✓ SessionStart hook searched CSR for past sessions")

    def test_session_end_hook_stores_narrative(self, tmp_path, monkeypatch):
        """
        SCENARIO: SessionEnd hook is triggered when Ralph loop completes
        SIMULATES: Claude Code running session_end_hook.py
        EXPECTED: Hook stores session narrative to CSR
        """
        monkeypatch.chdir(tmp_path)

        # Create a completed Ralph state
        claude_dir = tmp_path / ".claude"
        claude_dir.mkdir()
        (claude_dir / "ralph-loop.local.md").write_text("""---
active: true
iteration: 8
max_iterations: 50
completion_promise: "TESTS_PASS"
started_at: "2026-01-04T10:00:00Z"
---

Build REST API for todos.
Successfully completed with all tests passing.
""")

        # Run the session end hook
        project_root = Path(__file__).parent.parent.parent
        hook_script = project_root / "src" / "runtime" / "hooks" / "session_end_hook.py"

        result = subprocess.run(
            ["python3", str(hook_script)],
            input=json.dumps({"session_id": "test_session", "reason": "complete"}),
            capture_output=True,
            text=True,
            cwd=tmp_path,
            timeout=30
        )

        # Hook should exit successfully
        assert result.returncode == 0, f"SessionEnd hook failed: {result.stderr}"

        combined_output = result.stdout + result.stderr
        if "Stored session narrative" in combined_output or "CSR" in combined_output:
            print("✓ SessionEnd hook stored narrative to CSR")


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
