# Ralph Memory Integration - Safe Implementation Plan

**Status:** READY FOR IMPLEMENTATION
**Created:** 2026-01-04
**Branch:** `feat/ralph-csr-integration`
**Safety:** Full backup + automatic rollback on test failure

---

## PHASE 0: PRE-FLIGHT SAFETY CHECKS

### 0.1 Verify Current System State

Run these commands BEFORE starting implementation:

```bash
# Verify git is clean (save any work first)
cd /Users/ramakrishnanannaswamy/projects/claude-self-reflect
git status

# Verify Docker containers are healthy
docker ps --filter "name=claude" --format "table {{.Names}}\t{{.Status}}"

# Verify Qdrant is accessible
curl -s http://localhost:6333/collections | jq '.result.collections | length' || echo "QDRANT NOT ACCESSIBLE"
```

**Expected Output:**
- Git: Clean working directory (or only untracked planning files)
- Docker: All claude-* containers showing "Up" or "healthy"
- Qdrant: Returns a number (collection count)

**If any check fails, DO NOT PROCEED. Fix issues first.**

---

## PHASE 1: CREATE BACKUPS

### 1.1 Create Backup Directory

```bash
BACKUP_DIR="$HOME/.claude-self-reflect/backups/$(date +%Y%m%d_%H%M%S)_pre_ralph_memory"
mkdir -p "$BACKUP_DIR"
echo "Backup directory: $BACKUP_DIR"
```

### 1.2 Backup Docker Volumes

```bash
# Stop services gracefully to ensure consistent backup
echo "Stopping services for backup..."
docker stop claude-reflection-batch-watcher claude-reflection-batch-monitor 2>/dev/null || true
sleep 2

# Backup Qdrant data volume
echo "Backing up Qdrant data..."
docker run --rm \
  -v qdrant_data:/data:ro \
  -v "$BACKUP_DIR":/backup \
  alpine tar czf /backup/qdrant_data.tar.gz -C /data .

# Backup CSR config directory
echo "Backing up CSR config..."
tar czf "$BACKUP_DIR/csr_config.tar.gz" -C "$HOME/.claude-self-reflect" config 2>/dev/null || echo "No config to backup"

# Backup batch queue and state
tar czf "$BACKUP_DIR/csr_batch_queue.tar.gz" -C "$HOME/.claude-self-reflect" batch_queue 2>/dev/null || echo "No batch_queue to backup"
tar czf "$BACKUP_DIR/csr_batch_state.tar.gz" -C "$HOME/.claude-self-reflect" batch_state 2>/dev/null || echo "No batch_state to backup"

# Restart services
echo "Restarting services..."
docker start claude-reflection-batch-watcher claude-reflection-batch-monitor 2>/dev/null || true

echo "Backup complete at: $BACKUP_DIR"
ls -lh "$BACKUP_DIR"
```

### 1.3 Backup Git State

```bash
# Save current commit hash for easy rollback
echo "$(git rev-parse HEAD)" > "$BACKUP_DIR/git_head.txt"
echo "$(git branch --show-current)" > "$BACKUP_DIR/git_branch.txt"

# Create a backup branch
git stash --include-untracked -m "Pre-Ralph-Memory backup $(date +%Y%m%d_%H%M%S)" 2>/dev/null || echo "No changes to stash"
git branch "backup/pre-ralph-memory-$(date +%Y%m%d_%H%M%S)" 2>/dev/null || echo "Backup branch exists"
```

### 1.4 Verify Backups

```bash
echo "=== BACKUP VERIFICATION ==="
echo "Backup directory: $BACKUP_DIR"
echo ""
echo "Files:"
ls -lh "$BACKUP_DIR"
echo ""
echo "Qdrant backup size: $(du -h "$BACKUP_DIR/qdrant_data.tar.gz" 2>/dev/null | cut -f1 || echo 'N/A')"
echo "Git HEAD: $(cat "$BACKUP_DIR/git_head.txt" 2>/dev/null || echo 'N/A')"
echo ""
echo "Backup complete. Safe to proceed with implementation."
```

---

## PHASE 2: CREATE FEATURE BRANCH

### 2.1 Create and Switch to Feature Branch

```bash
cd /Users/ramakrishnanannaswamy/projects/claude-self-reflect

# Ensure we're on main and up to date
git checkout main
git pull origin main

# Create feature branch
git checkout -b feat/ralph-csr-integration

# Verify branch
git branch --show-current
```

**Expected Output:** `feat/ralph-csr-integration`

---

## PHASE 3: IMPLEMENTATION (Execute Tasks in Order)

### TASK 1: Create Directory Structure

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

### TASK 2: Create Ralph State Module

**File:** `src/runtime/hooks/ralph_state.py`

Create this file with the full implementation from the main implementation plan.

<details>
<summary>Click to expand full code (~150 lines)</summary>

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

</details>

**Verification:**
```bash
python -c "from src.runtime.hooks.ralph_state import RalphState; print(RalphState.create_new('test', 'done').to_markdown())"
```

---

### TASK 3: Create SessionStart Hook

**File:** `src/runtime/hooks/session_start_hook.py`

Create this file with the full implementation from the main implementation plan (~100 lines).

---

### TASK 4: Create SessionEnd Hook

**File:** `src/runtime/hooks/session_end_hook.py`

Create this file with the full implementation from the main implementation plan (~120 lines).

---

### TASK 5: Enhance PreCompact Hook

**File:** `src/runtime/precompact-hook.sh` (MODIFY EXISTING)

Add the Ralph memory integration section before `exit 0`.

---

### TASK 6: Create Stuck Detection Prompt

**File:** `src/runtime/hooks/stuck_detection_prompt.md`

---

### TASK 7: Create Claude Settings Template

**File:** `docs/planning/ralph-hooks-settings.json`

---

### TASK 8: Create Integration Tests

**File:** `tests/ralph/test_ralph_integration.py`

---

### TASK 9: Create Package Init Files

**Files:**
- `src/runtime/hooks/__init__.py`
- `tests/ralph/__init__.py`

---

## PHASE 4: RUN TESTS WITH AUTOMATIC ROLLBACK

### 4.1 Create Test Runner with Rollback Script

**File:** `scripts/ralph/test_with_rollback.sh`

```bash
#!/bin/bash
# Test runner with automatic rollback on failure

set -e  # Exit on any error

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BACKUP_DIR="$1"

if [ -z "$BACKUP_DIR" ]; then
    echo "Usage: $0 <backup_directory>"
    exit 1
fi

if [ ! -d "$BACKUP_DIR" ]; then
    echo "ERROR: Backup directory not found: $BACKUP_DIR"
    exit 1
fi

echo "=========================================="
echo "Running Ralph Memory Integration Tests"
echo "Backup: $BACKUP_DIR"
echo "=========================================="

cd "$PROJECT_ROOT"

# Function to rollback
rollback() {
    echo ""
    echo "=========================================="
    echo "TESTS FAILED - INITIATING ROLLBACK"
    echo "=========================================="

    # 1. Rollback git changes
    echo "Rolling back git changes..."
    git checkout main
    git stash pop 2>/dev/null || true

    # 2. Rollback Docker volumes
    echo "Rolling back Docker volumes..."
    docker stop claude-reflection-batch-watcher claude-reflection-batch-monitor claude-reflection-qdrant 2>/dev/null || true

    # Restore Qdrant data
    docker run --rm \
      -v qdrant_data:/data \
      -v "$BACKUP_DIR":/backup \
      alpine sh -c "rm -rf /data/* && tar xzf /backup/qdrant_data.tar.gz -C /data"

    # Restore CSR config
    if [ -f "$BACKUP_DIR/csr_config.tar.gz" ]; then
        tar xzf "$BACKUP_DIR/csr_config.tar.gz" -C "$HOME/.claude-self-reflect/"
    fi

    # Restart services
    docker start claude-reflection-qdrant 2>/dev/null || true
    sleep 5
    docker start claude-reflection-batch-watcher claude-reflection-batch-monitor 2>/dev/null || true

    echo ""
    echo "ROLLBACK COMPLETE"
    echo "System restored to pre-implementation state"
    exit 1
}

# Trap errors and rollback
trap rollback ERR

# Run tests
echo ""
echo "Running unit tests..."
pytest tests/ralph/test_ralph_integration.py -v --tb=short

echo ""
echo "Running integration validation..."

# Test 1: State module works
python -c "from src.runtime.hooks.ralph_state import RalphState; state = RalphState.create_new('test', 'done'); assert state.task == 'test'"
echo "✓ State module works"

# Test 2: SessionStart hook exits cleanly
echo '{}' | timeout 5 python src/runtime/hooks/session_start_hook.py && echo "✓ SessionStart hook works"

# Test 3: SessionEnd hook exits cleanly
echo '{}' | timeout 5 python src/runtime/hooks/session_end_hook.py && echo "✓ SessionEnd hook works"

# Test 4: PreCompact hook still works
touch /tmp/.ralph_state.md
(cd /tmp && bash "$PROJECT_ROOT/src/runtime/precompact-hook.sh") && echo "✓ PreCompact hook works"
rm /tmp/.ralph_state.md

# Test 5: Qdrant still accessible
curl -s http://localhost:6333/collections > /dev/null && echo "✓ Qdrant accessible"

echo ""
echo "=========================================="
echo "ALL TESTS PASSED"
echo "=========================================="
echo ""
echo "Safe to keep changes. Next steps:"
echo "1. Review changes: git diff main"
echo "2. Commit: git add -A && git commit -m 'feat: add Ralph memory integration hooks'"
echo "3. Push: git push -u origin feat/ralph-csr-integration"
```

**Make executable:**
```bash
chmod +x scripts/ralph/test_with_rollback.sh
```

### 4.2 Run Tests with Automatic Rollback

```bash
# Replace with your actual backup directory from Phase 1
BACKUP_DIR="$HOME/.claude-self-reflect/backups/YYYYMMDD_HHMMSS_pre_ralph_memory"

./scripts/ralph/test_with_rollback.sh "$BACKUP_DIR"
```

---

## PHASE 5: COMMIT AND PUSH (Only if Tests Pass)

### 5.1 Review Changes

```bash
git status
git diff --stat
```

### 5.2 Commit Changes

```bash
git add -A
git commit -m "feat: add Ralph memory integration hooks

- Add ralph_state.py for state file schema and parsing
- Add session_start_hook.py for CSR memory search at session start
- Add session_end_hook.py for narrative storage at session end
- Enhance precompact-hook.sh with Ralph state backup
- Add stuck_detection_prompt.md for Stop hook integration
- Add integration tests for full lifecycle validation
- Add Claude settings template for hook configuration

Closes: Memory-Augmented Ralph Loop Plan Section 13"
```

### 5.3 Push to Remote

```bash
git push -u origin feat/ralph-csr-integration
```

---

## ROLLBACK PROCEDURES

### Manual Rollback (if needed after automatic rollback fails)

#### Git Rollback

```bash
# Option 1: Discard all changes and return to main
git checkout main
git branch -D feat/ralph-csr-integration

# Option 2: Return to specific commit
ORIGINAL_COMMIT=$(cat "$BACKUP_DIR/git_head.txt")
git reset --hard "$ORIGINAL_COMMIT"
```

#### Docker Volume Rollback

```bash
# Stop services
docker stop claude-reflection-batch-watcher claude-reflection-batch-monitor claude-reflection-qdrant

# Restore Qdrant data
docker run --rm \
  -v qdrant_data:/data \
  -v "$BACKUP_DIR":/backup \
  alpine sh -c "rm -rf /data/* && tar xzf /backup/qdrant_data.tar.gz -C /data"

# Restore config files
tar xzf "$BACKUP_DIR/csr_config.tar.gz" -C "$HOME/.claude-self-reflect/" 2>/dev/null || true
tar xzf "$BACKUP_DIR/csr_batch_queue.tar.gz" -C "$HOME/.claude-self-reflect/" 2>/dev/null || true
tar xzf "$BACKUP_DIR/csr_batch_state.tar.gz" -C "$HOME/.claude-self-reflect/" 2>/dev/null || true

# Restart services
docker start claude-reflection-qdrant
sleep 5
docker start claude-reflection-batch-watcher claude-reflection-batch-monitor
```

#### Verify Rollback Success

```bash
# Check services are healthy
docker ps --filter "name=claude" --format "table {{.Names}}\t{{.Status}}"

# Verify Qdrant data
curl -s http://localhost:6333/collections | jq '.result.collections'

# Verify git state
git log --oneline -3
git status
```

---

## FILE SUMMARY

| File | Purpose | New/Modified |
|------|---------|--------------|
| `src/runtime/hooks/ralph_state.py` | State schema and parsing | NEW |
| `src/runtime/hooks/session_start_hook.py` | Search CSR at session start | NEW |
| `src/runtime/hooks/session_end_hook.py` | Store narrative at session end | NEW |
| `src/runtime/hooks/__init__.py` | Package init | NEW |
| `src/runtime/precompact-hook.sh` | Ralph state backup | MODIFIED |
| `src/runtime/hooks/stuck_detection_prompt.md` | Stop hook prompt | NEW |
| `docs/planning/ralph-hooks-settings.json` | Claude settings template | NEW |
| `tests/ralph/test_ralph_integration.py` | Integration tests | NEW |
| `tests/ralph/__init__.py` | Test package init | NEW |
| `scripts/ralph/test_with_rollback.sh` | Test runner with rollback | NEW |

---

## EXECUTION CHECKLIST

Use this checklist to track progress:

```
[ ] PHASE 0: Pre-flight checks passed
[ ] PHASE 1.1: Backup directory created
[ ] PHASE 1.2: Docker volumes backed up
[ ] PHASE 1.3: Git state backed up
[ ] PHASE 1.4: Backups verified
[ ] PHASE 2: Feature branch created
[ ] TASK 1: Directory structure created
[ ] TASK 2: ralph_state.py created
[ ] TASK 3: session_start_hook.py created
[ ] TASK 4: session_end_hook.py created
[ ] TASK 5: precompact-hook.sh enhanced
[ ] TASK 6: stuck_detection_prompt.md created
[ ] TASK 7: ralph-hooks-settings.json created
[ ] TASK 8: test_ralph_integration.py created
[ ] TASK 9: __init__.py files created
[ ] PHASE 4: Tests passed with automatic rollback protection
[ ] PHASE 5.1: Changes reviewed
[ ] PHASE 5.2: Changes committed
[ ] PHASE 5.3: Pushed to remote
```

---

**Status:** READY FOR IMPLEMENTATION
**Safety Level:** HIGH (Full backup + automatic rollback)
**Estimated Time:** 1-2 Ralph loop iterations
**Rollback Time:** ~2 minutes if needed
