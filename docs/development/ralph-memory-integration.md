# Ralph Loop Memory Integration

## Overview

The Ralph Wiggum technique helps Claude maintain context across long coding sessions. With CSR (Claude Self-Reflect) integration, Ralph loops gain **cross-session memory**—state is preserved across context compactions and retrievable in future sessions.

## How It Works

### Automatic Integration

Once you have:
1. **CSR installed and running** (Qdrant container)
2. **ralph-wiggum plugin installed** (Claude Code plugin)

The memory integration works **automatically**. No additional setup required.

### Hook Triggers

| Hook | When It Fires | What It Does |
|------|---------------|--------------|
| **SessionStart** | New session begins | Searches CSR for past Ralph sessions, injects context |
| **PreCompact** | Before context compaction | Backs up current Ralph state to CSR |
| **SessionEnd** | Session ends | Stores session narrative for future reference |

### The Memory Flow

```
Session 1 (Long Task)
├── Start: SessionStart searches CSR for past learnings
├── Work: Claude works on task, updates .claude/ralph-loop.local.md
├── COMPACTION: PreCompact backs up state to CSR before context is lost
└── End: SessionEnd stores narrative

Session 2 (After Compaction)
├── Start: SessionStart finds Session 1's backup in CSR
├── Injection: Past learnings injected into .ralph_past_sessions.md
└── Continue: Claude can reference what worked/failed before
```

## Installation

### Prerequisites
- CSR running with Qdrant: `docker compose up -d qdrant`
- ralph-wiggum plugin installed in Claude Code

### Install Hooks
```bash
./scripts/ralph/install_hooks.sh
```

### Verify Installation
```bash
./scripts/ralph/install_hooks.sh --check
```

Expected output:
```
✓ Hooks directory exists: ~/.claude/hooks
✓ SessionStart hook installed
✓ SessionEnd hook installed
✓ Settings.json contains Ralph configuration
✓ Source hooks available
All hooks properly installed
```

## Usage

### Start a Ralph Loop
```
/ralph-wiggum:ralph-loop "Your task" --completion-promise "Task complete"
```

### What Happens Automatically

1. **At Session Start**: CSR is searched for past Ralph sessions
   - Past learnings are injected into `.ralph_past_sessions.md`
   - You can reference what worked/failed in previous sessions

2. **Before Compaction**: Current state is backed up
   - Iteration count preserved
   - Failed approaches recorded
   - Successful strategies saved

3. **At Session End**: Full narrative stored
   - Task description
   - Outcome (completed/abandoned/incomplete)
   - What worked and what failed

## Files Created

| File | Purpose |
|------|---------|
| `.claude/ralph-loop.local.md` | Current Ralph state (managed by plugin) |
| `.ralph_past_sessions.md` | Injected context from past sessions |

## Troubleshooting

### Hooks Not Firing
```bash
# Check hook configuration
cat ~/.claude/settings.json | grep -A5 ralph

# Reinstall hooks
./scripts/ralph/install_hooks.sh --remove
./scripts/ralph/install_hooks.sh
```

### CSR Connection Issues
```bash
# Check Qdrant is running
docker ps | grep qdrant

# Start if needed
docker compose up -d qdrant

# Test connection
python -c "
import sys
sys.path.insert(0, 'mcp-server/src')
from standalone_client import CSRStandaloneClient
print(CSRStandaloneClient().test_connection())
"
```

### No Past Sessions Found
This is normal for first-time use. Past sessions will appear after:
1. Completing at least one Ralph session
2. Having a compaction event occur
3. Starting a new session on a similar task

## Architecture

### Key Components

```
src/runtime/hooks/
├── ralph_state.py      # State parsing (both formats)
├── session_start_hook.py  # Search CSR, inject context
└── session_end_hook.py    # Store narrative

src/runtime/
└── precompact-hook.sh  # Backup state before compaction

mcp-server/src/
└── standalone_client.py  # CSR client for hooks

scripts/ralph/
├── install_hooks.sh    # Install to ~/.claude/settings.json
├── backup_and_restore.sh  # Backup/rollback utility
└── test_with_rollback.sh  # Tests with auto-rollback
```

### State File Formats

The hooks support two formats:

**ralph-wiggum plugin format** (`.claude/ralph-loop.local.md`):
```yaml
---
active: true
iteration: 5
max_iterations: 50
completion_promise: "Tests pass"
started_at: "2026-01-04T10:00:00Z"
---
Task description here
```

**Custom format** (`.ralph_state.md`):
```markdown
# Ralph Session State
## Metadata
- **Session ID:** ralph_123
- **Task:** Build feature X
- **Iteration:** 5
```

## Testing

### Run Unit Tests
```bash
python -m pytest tests/ralph/test_ralph_integration.py -v
```

### Run Compaction Scenario Tests
```bash
python -m pytest tests/ralph/test_ralph_integration.py -v -k "Compaction"
```

### Manual Hook Test
```bash
# Test SessionStart
echo '{"session_id": "test", "source": "startup"}' | \
  python3 src/runtime/hooks/session_start_hook.py

# Test PreCompact
./src/runtime/precompact-hook.sh

# Test SessionEnd
echo '{"session_id": "test", "reason": "clear"}' | \
  python3 src/runtime/hooks/session_end_hook.py
```

## Real-World Usage Examples

### Example 1: Long Refactoring Task

```
# Start the loop with a descriptive task
/ralph-wiggum:ralph-loop "Refactor authentication to use JWT tokens"

# After several iterations, compaction occurs...
# New session starts with past context injected:

# Check what was learned:
cat .ralph_past_sessions.md

# Output shows:
# ## Past Session 1 (Score: 0.93)
# - Task: Refactor authentication to use JWT tokens
# - What Failed: Direct token replacement broke session middleware
# - What Worked: Gradual migration with feature flag
```

### Example 2: Debugging Across Sessions

```
# Session 1: Start debugging
/ralph-wiggum:ralph-loop "Fix memory leak in worker process"

# Work for 30 minutes, context compacts...

# Session 2: New session starts
# .ralph_past_sessions.md shows:
# - Tried: Heap profiling (inconclusive)
# - Tried: GC logging (found closure issue)
# - Next: Check event listener cleanup

# You continue from where you left off!
```

### Example 3: Building a Feature Over Days

```
# Day 1 Morning: Start feature
/ralph-wiggum:ralph-loop "Add real-time notifications with WebSockets"

# Context compacts multiple times during the day...

# Day 2: Start new session
# CSR retrieves yesterday's learnings:
# - WebSocket connection pooling works
# - Redis pub/sub for cross-instance messaging
# - Still TODO: Reconnection logic

# You have full context of what worked!
```

## Best Practices

1. **Let compaction happen naturally** - Don't force exits, let the loop run
2. **Check `.ralph_past_sessions.md`** - Reference past learnings before repeating approaches
3. **Use descriptive completion promises** - Makes future searches more relevant
4. **Tag sessions appropriately** - Helps CSR find related sessions

## Privacy Note

Ralph session data is stored locally in your Qdrant instance. No data is sent externally unless you configure a cloud Qdrant instance.
