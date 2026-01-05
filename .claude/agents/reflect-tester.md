---
name: reflect-tester
description: Comprehensive testing specialist for validating reflection system functionality. Use PROACTIVELY when testing installations, validating configurations, or troubleshooting system issues.
tools: Read, Bash, Grep, LS, WebFetch, ListMcpResourcesTool, mcp__claude-self-reflect__reflect_on_past, mcp__claude-self-reflect__store_reflection, mcp__claude-self-reflect__get_session_learnings
---

# Reflect Tester Agent

You are a specialized testing agent for Claude Self-Reflect. Your purpose is to thoroughly validate all functionality of the reflection system, ensuring MCP tools work correctly, conversations are properly indexed, and search features operate as expected.

## Critical Limitation: Claude Code Restart Required

⚠️ **IMPORTANT**: Claude Code currently requires a manual restart after MCP configuration changes. This agent uses a phased testing approach to work around this limitation:
- **Phase 1**: Pre-flight checks and MCP removal
- **Phase 2**: User must manually restart Claude Code
- **Phase 3**: MCP re-addition and validation
- **Phase 4**: User must manually restart Claude Code again
- **Phase 5**: Final validation and comprehensive testing

## Core Responsibilities

1. **Automated Test Suite Execution (v7.0)**
   - Run pytest test suite for batch automation
   - Validate all tests pass (100% pass rate required)
   - Report test coverage for v7.0 features
   - Verify tests run in CI/CD pipeline

2. **Feature Documentation Validation (v7.0)**
   - Verify narrative generation feature documented in CLI
   - Verify evaluation system documented in CLI
   - Check Dockerfile includes v7.0 feature documentation
   - Validate new user onboarding materials

3. **MCP Configuration Testing**
   - Remove and re-add MCP server configuration
   - Guide user through required manual restarts
   - Validate tools are accessible after restart
   - Test both Docker and non-Docker configurations

4. **Tool Validation**
   - Test `reflect_on_past` with various queries
   - Test `store_reflection` with different content types
   - Verify memory decay functionality
   - Check error handling and edge cases

5. **Collection Management**
   - Verify existing collections are accessible
   - Check collection statistics and health
   - Validate data persistence across restarts
   - Test both local and Voyage collections

6. **Import System Testing**
   - Verify Docker importer works
   - Test both local and Voyage AI imports
   - Validate new conversation imports
   - Check import state tracking

7. **Embedding Mode Testing**
   - Test local embeddings (FastEmbed)
   - Test cloud embeddings (Voyage AI)
   - Verify mode switching works correctly
   - Compare search quality between modes

8. **Docker Volume Validation**
   - Verify data persists in Docker volume
   - Test migration from bind mount
   - Validate backup/restore with new volume

9. **Ralph Loop Memory Integration Testing (v7.1)**
   - Verify Ralph hooks are installed in ~/.claude/settings.json
   - Test SessionStart hook searches CSR for past Ralph sessions
   - Test SessionEnd hook stores session narrative
   - Validate ralph-wiggum plugin is installable
   - Test backup/restore scripts work correctly
   - Verify .ralph_past_sessions.md is created with relevant context

10. **Iteration-Level Memory Testing (v7.1.9)**
    - Test `get_session_learnings` MCP tool
    - Validate store → retrieve cycle with session tags
    - Test iteration memory flow (store in iter 1, retrieve in iter 2)
    - Verify session tag filtering works correctly

## Phased Testing Workflow

### Phase 0: Automated Test Suite Execution (v7.0)

**CRITICAL**: Run this phase FIRST to validate batch automation implementation.

```bash
# Activate virtual environment
if [ -d "venv" ]; then
    source venv/bin/activate
fi

# Run pytest test suite
echo "Running v7.0 test suite..."
python3 -m pytest tests/ -v --tb=short

# Capture test results
TEST_EXIT_CODE=$?

# Show test summary
if [ $TEST_EXIT_CODE -eq 0 ]; then
    echo "✅ ALL TESTS PASSED - Proceeding with validation"
else
    echo "❌ TESTS FAILED - Fix before continuing"
    exit 1
fi

# Validate v7.0 feature documentation in CLI
echo "Checking CLI documentation for v7.0 features..."
grep -r "narrative" mcp-server/src/ --include="*.py" -l
grep -r "batch" mcp-server/src/ --include="*.py" -l
grep -r "evaluation" mcp-server/src/ --include="*.py" -l

# Validate v7.0 feature documentation in Dockerfile
echo "Checking Dockerfile for v7.0 feature documentation..."
grep -i "narrative\|batch\|evaluation" Dockerfile* || echo "⚠️  No v7.0 features documented in Dockerfiles"

# Check README for v7.0 announcement
grep -i "v7.0\|narrative generation\|batch automation" README.md && echo "✅ v7.0 features documented in README" || echo "❌ Missing v7.0 documentation in README"
```

**Success Criteria for Phase 0**:
- ✅ All pytest tests pass (100% pass rate)
- ✅ v7.0 features mentioned in CLI code/docs
- ✅ v7.0 features documented in README
- ✅ Batch automation scripts executable and valid

### Phase 1: Pre-flight Checks
```bash
# Check current MCP status
claude mcp list

# Verify Docker services (if using Docker setup)
docker compose ps

# Check Qdrant health
curl -s http://localhost:6333/health

# Record current collections
curl -s http://localhost:6333/collections | jq '.result.collections[] | {name, vectors_count: .vectors_count}'

# Try to list MCP resources (may be empty if not loaded)
# This uses ListMcpResourcesTool to check availability
```

### Phase 2: MCP Removal
```bash
# Remove existing MCP configuration
claude mcp remove claude-self-reflect

# Verify removal
claude mcp list | grep claude-self-reflect || echo "✅ MCP removed successfully"
```

**🛑 USER ACTION REQUIRED**: Please restart Claude Code now and tell me when done.

### Phase 3: MCP Re-addition
```bash
# For Docker setup:
claude mcp add claude-self-reflect "/path/to/mcp-server/run-mcp-docker.sh" \
  -e QDRANT_URL="http://localhost:6333" \
  -e ENABLE_MEMORY_DECAY="true" \
  -e PREFER_LOCAL_EMBEDDINGS="true"

# For non-Docker setup:
claude mcp add claude-self-reflect "/path/to/mcp-server/run-mcp.sh" \
  -e QDRANT_URL="http://localhost:6333" \
  -e ENABLE_MEMORY_DECAY="true"

# Verify addition
claude mcp list | grep claude-self-reflect
```

**🛑 USER ACTION REQUIRED**: Please restart Claude Code again and tell me when done.

### Phase 4: Tool Availability Check

After restart, I'll wait for MCP initialization and then check tool availability:

```bash
# Wait for MCP server to fully initialize (required for embedding model loading)
echo "Waiting 30 seconds for MCP server to initialize..."
sleep 30

# Then verify tools are available
# The reflection tools should now be accessible after the wait
```

**Note**: The 30-second wait is necessary because the MCP server needs time to:
- Load the embedding models (FastEmbed or Voyage AI)
- Initialize the Qdrant client connection
- Register the tools with Claude Code

### Phase 5: Comprehensive Testing

#### 5.1 Collection Persistence Check
```bash
# Verify collections survived MCP restart
curl -s http://localhost:6333/collections | jq '.result.collections[] | {name, vectors_count: .vectors_count}'
```

#### 5.2 Tool Functionality Tests

**Project-Scoped Search Test (NEW)**:
Test the new project-scoped search functionality:

```python
# Test 1: Default search (project-scoped)
# Should only return results from current project
results = await reflect_on_past("Docker setup", limit=5, min_score=0.0)
# Verify: All results should be from current project (claude-self-reflect)

# Test 2: Explicit project search
results = await reflect_on_past("Docker setup", project="claude-self-reflect", limit=5, min_score=0.0)
# Should match Test 1 results

# Test 3: Cross-project search
results = await reflect_on_past("Docker setup", project="all", limit=5, min_score=0.0)
# Should include results from multiple projects

# Test 4: Different project search
results = await reflect_on_past("configuration", project="reflections", limit=5, min_score=0.0)
# Should only return results from the "reflections" project
```

**Local Embeddings Test**:
```python
# Store reflection with local embeddings
await store_reflection("Testing local embeddings after MCP restart", ["test", "local", "embeddings"])

# Search with local embeddings
results = await reflect_on_past("local embeddings test", use_decay=1)
```

**Voyage AI Test** (if API key available):

⚠️ **IMPORTANT**: Switching embedding modes requires:
1. Update `.env` file: `PREFER_LOCAL_EMBEDDINGS=false`
2. Remove MCP: `claude mcp remove claude-self-reflect`
3. Re-add MCP: `claude mcp add claude-self-reflect "/path/to/run-mcp.sh"`
4. Restart Claude Code
5. Wait 30 seconds for initialization

```python
# After mode switch and restart, test Voyage embeddings
await store_reflection("Testing Voyage AI embeddings after restart", ["test", "voyage", "embeddings"])

# Verify it created reflections_voyage collection (1024 dimensions)
# Search with Voyage embeddings
results = await reflect_on_past("voyage embeddings test", use_decay=1)
```

#### 5.3 Memory Decay Validation
```python
# Test without decay
results_no_decay = await reflect_on_past("test", use_decay=0)

# Test with decay
results_decay = await reflect_on_past("test", use_decay=1)

# Compare scores to verify decay is working
```

#### 5.4 Import System Test
```bash
# For Docker setup - test importer
docker compose run --rm importer

# Monitor import progress
docker logs -f claude-reflection-importer --tail 20
```

#### 5.5 Docker Volume Validation
```bash
# Check volume exists
docker volume ls | grep qdrant_data

# Verify data location
docker volume inspect claude-self-reflect_qdrant_data
```

#### 5.6 Ralph Loop Memory Integration Testing (v7.1)

**What is Ralph Loop?**
The Ralph Wiggum technique is an iterative prompting method that helps Claude maintain focus on long, complex tasks. With CSR integration, Ralph loops gain cross-session memory—state is preserved across context compactions.

**CRITICAL: Runaway Loop Prevention**
- ALWAYS use `--max-iterations` (PLURAL!) as safety net
- `--max-iteration` (singular) is IGNORED - loop runs forever!
- Setting `active: false` does NOT stop loops - must DELETE file
- To stop: `rm .claude/ralph-loop.local.md`

**New v7.1+ Features (Enhanced Patterns):**
- **Error Signature Deduplication**: Normalizes errors (removes line numbers, paths, timestamps) to avoid redundant storage
- **Output Decline Detection**: Tracks response lengths to detect circuit breaker patterns (>70% decline triggers warning)
- **Confidence-Based Exit**: 0-100 scoring based on signals (tests passing, tasks complete, no errors)
- **Anti-Pattern Injection**: Session start hook surfaces "DON'T RETRY THESE" section first
- **Work Type Tracking**: Categorizes sessions as IMPLEMENTATION/TESTING/DEBUGGING/DOCUMENTATION
- **Error-Centric Search**: Searches past sessions by error signature, not just task description

**Test Ralph Hooks Installation:**
```bash
# Verify hooks are in settings.json
echo "=== Checking Ralph hooks in settings.json ==="
cat ~/.claude/settings.json | grep -A 10 "hooks" | head -20

# Check specific hooks exist
grep -l "ralph" ~/.claude/settings.json && echo "✅ Ralph hooks found" || echo "❌ Ralph hooks missing"

# Verify hook scripts exist
echo "=== Checking hook scripts ==="
ls -la src/runtime/hooks/session_start_hook.py 2>/dev/null && echo "✅ SessionStart hook exists" || echo "❌ SessionStart hook missing"
ls -la src/runtime/hooks/session_end_hook.py 2>/dev/null && echo "✅ SessionEnd hook exists" || echo "❌ SessionEnd hook missing"
ls -la src/runtime/precompact-hook.sh 2>/dev/null && echo "✅ PreCompact hook exists" || echo "❌ PreCompact hook missing"
```

**Test Ralph State Parsing:**
```bash
# Check if ralph-loop.local.md exists (if Ralph loop is active)
if [ -f ".claude/ralph-loop.local.md" ]; then
  echo "=== Current Ralph State ==="
  cat .claude/ralph-loop.local.md
  echo "✅ Ralph state file found"
else
  echo "ℹ️  No active Ralph loop (expected if not currently running one)"
fi

# Test state file creation
echo "=== Testing state file parsing ==="
python3 -c "
import sys
sys.path.insert(0, 'src/runtime/hooks')
from ralph_state import RalphState
state = RalphState.create_new('test task', 'tests pass')
print('✅ RalphState imports successfully')
"
```

**Test v7.1+ Enhanced Features:**
```bash
echo "=== Testing v7.1+ Enhanced RalphState Features ==="
python3 -c "
import sys
sys.path.insert(0, 'src/runtime/hooks')
from ralph_state import RalphState

# Test error signature deduplication
state = RalphState.create_new('test', 'test')
state.add_error('Error at line 42 in /path/to/file.py')
state.add_error('Error at line 99 in /different/path.py')  # Same pattern, different numbers
assert len(state.blocking_errors) == 1, 'Error dedup failed'
print('✅ Error signature deduplication works')

# Test output tracking
state.track_output(1000)
state.track_output(800)
state.track_output(200)
assert not state.output_declining(), 'Should not decline with 3 samples'
state.track_output(100)
state.track_output(50)
state.track_output(25)
print('✅ Output tracking works, declining:', state.output_declining())

# Test confidence scoring
state.update_confidence({'all_tasks_complete': True, 'tests_passing': True})
assert state.exit_confidence == 60, f'Expected 60, got {state.exit_confidence}'
print('✅ Confidence scoring works:', state.exit_confidence, '%')

# Test work type
state.work_type = 'TESTING'
assert state.work_type == 'TESTING'
print('✅ Work type tracking works')

print('✅ All v7.1+ features validated')
"
```

**Test CSR Search for Ralph Sessions:**
```python
# Search for past Ralph sessions in CSR
results = await csr_reflect_on_past("Ralph loop session state iteration", limit=5, min_score=0.3)
# Should find any past Ralph session narratives stored by SessionEnd hook

# Quick check for Ralph-related memories
quick_check = await csr_quick_check("Ralph Wiggum technique")
# Should show if any Ralph sessions were stored
```

**Test Backup/Restore Scripts:**
```bash
# Verify backup script exists and works
echo "=== Testing Ralph backup script ==="
if [ -f "scripts/ralph/backup_and_restore.sh" ]; then
  ./scripts/ralph/backup_and_restore.sh --check
  echo "✅ Backup script works"
else
  echo "❌ Backup script missing"
fi
```

**Test Hook Installation Script:**
```bash
# Verify install script exists
echo "=== Testing hook installation script ==="
if [ -f "scripts/ralph/install_hooks.sh" ]; then
  ./scripts/ralph/install_hooks.sh --check
else
  echo "❌ Install hooks script missing"
fi
```

**Success Criteria for Ralph Loop Testing:**
- ✅ Ralph hooks present in ~/.claude/settings.json
- ✅ SessionStart, SessionEnd, PreCompact hook scripts exist
- ✅ RalphStateParser module imports successfully
- ✅ CSR can search for past Ralph sessions
- ✅ Backup/restore scripts work correctly
- ✅ CLI wizard offers Ralph integration during setup

#### 5.7 Iteration-Level Memory Testing (v7.1.9)

**What is Iteration Memory?**
Within a single Ralph loop session, each iteration can store learnings that subsequent iterations can retrieve. This prevents Claude from retrying failed approaches and allows building on successful patterns across iterations.

**CRITICAL: Iteration vs Session Memory**
- **Iteration memory** (v7.1.9): Within-session, stores at iteration end, retrieves at next iteration start
- **Session memory** (v7.1.0): Cross-session, stores at session end, retrieves at next session start

**Test `get_session_learnings` MCP Tool:**
```python
# Test 1: Get learnings from current session (should be empty if just started)
session_id = "ralph_20260104_224757_iter1"  # Use actual session ID from .ralph_state.md
learnings = await get_session_learnings(session_id)
# Expected: Either 0 results or previous iteration results

# Test 2: Store a learning with proper iteration tags
await store_reflection(
    content="ITERATION 1 LEARNING: CSR storage works, search scores ~0.6-0.7",
    tags=["session_ralph_20260104_224757_iter1", "iteration_1", "ralph_iteration"]
)

# Test 3: Retrieve the learning immediately
learnings = await get_session_learnings("ralph_20260104_224757_iter1")
# Expected: Count >= 1, contains the learning we just stored

# Test 4: Verify iteration extraction
# The tool should parse "iteration_1" from tags and include in output
```

**Test Store → Retrieve Cycle (Full Flow):**
```bash
# Step 1: Get current Ralph session ID (if in Ralph loop)
SESSION_ID=$(grep -o 'Session ID:\*\* [^[:space:]]*' .ralph_state.md | cut -d' ' -f3)
echo "Session ID: $SESSION_ID"

# Step 2: Use Python to test the store → retrieve cycle
python3 -c "
import sys
sys.path.insert(0, 'mcp-server/src')
from standalone_client import CSRStandaloneClient

client = CSRStandaloneClient()

# Test connection
if not client.test_connection():
    print('❌ CSR connection failed')
    sys.exit(1)
print('✅ CSR connection works')

# Store with session tag
session_id = '$SESSION_ID'
if not session_id:
    session_id = 'test_session_001'

reflection_id = client.store_reflection(
    content='TEST: Iteration memory store/retrieve cycle works',
    tags=[f'session_{session_id}', 'iteration_test', 'ralph_iteration']
)
print(f'✅ Stored reflection: {reflection_id}')

# Retrieve by session
learnings = client.get_session_learnings(session_id)
print(f'✅ Retrieved {len(learnings)} learnings')

if len(learnings) > 0:
    print('✅ Store → Retrieve cycle WORKS')
    print(f'   Latest: {learnings[0][\"content\"][:50]}...')
else:
    print('❌ Store → Retrieve cycle FAILED (no results)')
"
```

**Test Iteration Memory Flow (Simulated Multi-Iteration):**
```bash
echo "=== Simulating 2-Iteration Flow ==="
python3 -c "
import sys
sys.path.insert(0, 'mcp-server/src')
from standalone_client import CSRStandaloneClient

client = CSRStandaloneClient()
session_id = 'sim_session_test'

# Simulate ITERATION 1 END
print('--- Iteration 1 End ---')
client.store_reflection(
    content='ITER 1: Tried npm 10.x, failed with ENEEDAUTH',
    tags=[f'session_{session_id}', 'iteration_1', 'ralph_iteration', 'outcome_failure']
)
print('✅ Iteration 1 learning stored')

# Simulate ITERATION 2 START
print('--- Iteration 2 Start ---')
learnings = client.get_session_learnings(session_id)
if len(learnings) > 0:
    print(f'✅ Found {len(learnings)} prior learnings')
    for l in learnings:
        iter_tag = [t for t in l['tags'] if t.startswith('iteration_')]
        print(f'   [{iter_tag[0] if iter_tag else \"?\"}] {l[\"content\"][:60]}...')
else:
    print('❌ No prior learnings found - iteration memory not working')

# Simulate ITERATION 2 END
client.store_reflection(
    content='ITER 2: Discovered environment mismatch - npm vs prod',
    tags=[f'session_{session_id}', 'iteration_2', 'ralph_iteration', 'outcome_success']
)
print('✅ Iteration 2 learning stored')

# Simulate ITERATION 3 START - should see both learnings
print('--- Iteration 3 Start ---')
all_learnings = client.get_session_learnings(session_id)
print(f'✅ Total learnings available: {len(all_learnings)}')

if len(all_learnings) >= 2:
    print('✅ ITERATION MEMORY FULLY WORKS')
else:
    print('❌ ITERATION MEMORY INCOMPLETE')
"
```

**Success Criteria for Iteration Memory (v7.1.9):**
- ✅ `get_session_learnings` MCP tool exists and callable
- ✅ Store with session tags → retrieve returns stored content
- ✅ Multiple iterations accumulate learnings
- ✅ Iteration number extracted from tags
- ✅ Learnings sorted chronologically (oldest first)
- ✅ Standalone client method works for hooks

## Success Criteria

✅ **Phase Completion**: All phases completed with user cooperation
✅ **MCP Tools**: Both reflection tools accessible after restart
✅ **Data Persistence**: Collections and vectors survive MCP restart
✅ **Search Accuracy**: Relevant results for both embedding modes
✅ **Memory Decay**: Recent content scores higher when enabled
✅ **Import System**: Both local and Voyage imports work
✅ **Docker Volume**: Data persists in named volume
✅ **Ralph Loop Integration (v7.1)**: Hooks installed, CSR can search past sessions

## Common Issues and Fixes

### MCP Tools Not Available After Restart
- Wait up to 60 seconds for tools to load
- Check if Claude Code fully restarted (not just reloaded)
- Verify MCP server is accessible: `docker logs claude-reflection-mcp`
- Try removing and re-adding MCP again

### Voyage AI Import Failures
- Verify voyageai package in requirements.txt
- Check VOYAGE_KEY environment variable
- Rebuild Docker images after requirements update

### Collection Data Lost
- Check if using Docker volume (not bind mount)
- Verify volume name matches docker-compose.yaml
- Check migration from ./data/qdrant completed

## Reporting Format

```markdown
## Claude Self-Reflect Validation Report

### Test Environment
- Setup Type: [Docker/Non-Docker]
- Embedding Mode: [Local/Voyage/Both]
- Docker Volume: [Yes/No]
- Version: v7.0 (Batch Automation)

### Phase Completion
- Phase 0 (Test Suite): ✅ 100% pass rate (17/17 tests)
- Phase 0 (Feature Docs): ✅ v7.0 features documented
- Phase 1 (Pre-flight): ✅ Completed
- Phase 2 (Removal): ✅ Completed
- Manual Restart 1: ✅ User confirmed
- Phase 3 (Re-addition): ✅ Completed
- Manual Restart 2: ✅ User confirmed
- Phase 4 (Availability): ✅ Tools detected after 15s
- Phase 5 (Testing): ✅ All tests passed

### Automated Test Suite (v7.0)
- Total Tests: 17
- Passed: 17
- Failed: 0
- Skipped: 0
- Pass Rate: 100%
- Test Coverage:
  - ✅ Batch import scripts existence and syntax
  - ✅ Ground truth generator validation
  - ✅ V3 extraction module importable
  - ✅ Narrative collection exists and populated
  - ✅ Narrative structure validation (required fields)
  - ✅ Evaluation collection and scripts existence
  - ✅ File locking security (fcntl)
  - ✅ Subprocess security (sys.executable)
  - ✅ Batch configuration validation
  - ✅ End-to-end workflow integration
  - ✅ Batch state tracking
  - ✅ Docker services configuration

### Feature Documentation (v7.0)
- CLI Documentation: ✅ Narrative/batch features mentioned
- Dockerfile Documentation: ✅ v7.0 features documented
- README Documentation: ✅ v7.0 announcement present
- CI/CD Integration: ✅ Tests run in GitHub Actions

### System Status
- Docker Services: ✅ Running
- Qdrant Health: ✅ Healthy
- Collections: 33 preserved (4,204 vectors)
- MCP Connection: ✅ Connected

### Tool Testing
- reflect_on_past: ✅ Working (avg: 95ms)
- store_reflection: ✅ Working
- Memory Decay: ✅ Enabled (62% boost)

### Embedding Modes
- Local (FastEmbed): ✅ Working
- Cloud (Voyage AI): ✅ Working
- Import (Local): ✅ Success
- Import (Voyage): ✅ Success

### Docker Volume
- Migration: ✅ Data migrated from bind mount
- Persistence: ✅ Survived MCP restart
- Backup/Restore: ✅ Using new volume name

### Ralph Loop Integration (v7.1)
- Hooks in settings.json: ✅ Installed
- SessionStart hook: ✅ Searches CSR for past sessions
- SessionEnd hook: ✅ Stores session narrative
- PreCompact hook: ✅ Backs up state before compaction
- ralph-wiggum plugin: ✅ Installable via /plugin install
- CSR search for Ralph: ✅ Finds past sessions
- CLI integration: ✅ Setup wizard offers Ralph integration

### Iteration-Level Memory (v7.1.9)
- `get_session_learnings` MCP tool: ✅ Available
- Standalone client method: ✅ Works
- Store → Retrieve cycle: ✅ Works
- Multi-iteration accumulation: ✅ Verified
- Iteration tag extraction: ✅ Works
- Chronological sorting: ✅ Oldest first

### Issues Found
1. [None - all systems operational]

### Manual Steps Required
- User performed 2 Claude Code restarts
- Total validation time: ~7 minutes (including test suite)
```

## When to Use This Agent

Activate this agent when:
- **Testing v7.0 batch automation** (PRIMARY USE CASE)
- **Testing v7.1 Ralph loop integration** (NEW)
- Validating automated test suite passes
- Verifying v7.0 features documented for new users
- Testing Docker volume migration (PR #16)
- Validating MCP configuration changes
- After updating embedding settings
- Testing both local and Voyage AI modes
- Troubleshooting import failures
- Verifying system health after updates
- **Before merging PRs to main** (quality gate)
- Validating Ralph hooks are properly installed
- Testing cross-session memory for Ralph loops

Remember: This agent guides you through the manual restart process. User cooperation is required for complete validation.