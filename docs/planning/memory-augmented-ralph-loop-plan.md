# Memory-Augmented Ralph Loop: Comprehensive Design Plan

**Version:** 2.0
**Status:** DRAFT - Refined with Compaction Insights
**Author:** Claude (via Ralph Loop iteration)
**Date:** 2026-01-04

---

## Executive Summary

This document proposes integrating Claude Self-Reflect's conversation memory system with the Ralph Wiggum iterative development loop. After extensive research, adversarial review, and analysis of the **compaction problem**, we recommend a **hybrid state preservation approach** that combines file-based intra-session state with CSR-based cross-session learning.

### Key Insights

1. **Why Ralph Works**: Models aren't trained to work for long periods - they naturally want to stop. Ralph forces continuation by refusing to accept the stop signal.

2. **The Compaction Problem**: When context is summarized mid-session, critical state is lost. This causes Claude to repeat failed approaches. This is the REAL problem to solve.

3. **Hybrid State Preservation**: Use `.ralph_state.md` for fast intra-session state (zero latency, always in context) + CSR hooks for cross-session learning (semantic search, tagged storage).

4. **CSR Integration Points**:
   - **PreCompact Hook**: Backup state to CSR before compaction
   - **SessionStart**: Search CSR for relevant past sessions
   - **SessionEnd**: Store narrative in CSR for future learning
   - **Stuck Detection**: Search CSR for similar past problems

### Key Findings

1. **Ralph Wiggum** is a bash-based iterative loop that repeatedly feeds the same prompt to Claude until task completion
2. **Claude Self-Reflect** provides semantic search across past conversations via 15+ MCP tools
3. **Compaction is lossy** - critical state disappears when context is summarized
4. **Files survive compaction** - state written to `.ralph_state.md` persists
5. **CSR provides cross-session learning** - past solutions are searchable

---

## 1. Problem Statement

### Current Ralph Wiggum Limitations

Ralph Wiggum's power comes from its simplicity: a bash while loop that keeps feeding Claude the same prompt until completion. However, this "amnesia" between sessions means:

- **No learning from past sessions**: Each new Ralph run starts from scratch
- **Repeated failures**: Same mistakes get made across sessions
- **No pattern recognition**: No awareness of successful strategies from similar problems
- **Wasted iterations**: Time spent rediscovering solutions that were found before

### Current Claude Self-Reflect Capabilities

CSR provides rich conversation memory but is designed for:

- **Post-hoc search**: Finding past discussions on a topic
- **Reflection storage**: Saving insights for future reference
- **Narrative generation**: AI-powered summaries of past work

### The Gap

There's no bridge between Ralph's iterative execution and CSR's memory system. Developers using Ralph can't benefit from past session learnings.

---

## 2. The Compaction Problem (Critical Insight)

### Why This Matters More Than Cross-Session Memory

The original design focused on cross-session learning (remembering past Ralph runs). But the **bigger problem** is intra-session state loss due to compaction.

### How Compaction Breaks Ralph Loops

```
Iteration 1-20: Context fills up with work history
    ↓
Context Window Limit Reached
    ↓
Claude Code Compacts: "Previous work summarized as 'worked on auth bug'"
    ↓
Iteration 21: Claude doesn't remember:
  - Specific error messages from iteration 5
  - Why approach X was abandoned in iteration 12
  - The exact file path that was problematic
    ↓
Claude REPEATS the same failed approach
```

### What Gets Lost in Compaction

| State Type | Example | Impact When Lost |
|------------|---------|------------------|
| Error details | "Redis timeout after 30s on pool.acquire()" | Retry with same broken config |
| Failed approaches | "Tried increasing buffer - caused OOM" | Waste iterations retrying |
| Decision rationale | "Chose JWT over sessions because of scaling" | Revisit already-decided questions |
| File state | "Modified auth.py:45, config.yaml:12" | Lose track of changes |
| Progress markers | "3/5 tests passing, 2 remaining failures" | Lose progress awareness |

### The Solution: Hybrid State Preservation

**Layer 1: .ralph_state.md (Intra-Session, Fast)**
- Updated by Claude every iteration
- Always in context (zero latency)
- Survives compaction (it's a file)
- Contains current working state

**Layer 2: CSR Hooks (Cross-Session, Smart)**
- `store_reflection()` at key moments (compaction, stuck, session-end)
- `reflect_on_past()` at session start and when stuck
- Semantic search for similar past problems
- Tagged for retrieval (`ralph_session`, `iteration_N`)

```
┌─────────────────────────────────────────────────────────────────┐
│                    LAYER 1: Fast Intra-Session                  │
│                                                                 │
│  .ralph_state.md (Updated Every Iteration)                      │
│  ──────────────────────────────────────────                     │
│  ## Current State (Iteration 15)                                │
│  Task: Fix authentication timeout                               │
│  Approach: JWT with Redis session store                         │
│  Failed: [session cookies, OAuth implicit flow]                 │
│  Blocking: Redis pool exhaustion                                │
│  Files: [auth.py:45, redis_config.yaml:12]                      │
│  Next: Increase pool size, add connection timeout               │
│                                                                 │
│  ✓ Zero latency (file read)                                     │
│  ✓ Always in context                                            │
│  ✓ Survives compaction                                          │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                    LAYER 2: Smart Cross-Session                 │
│                                                                 │
│  CSR store_reflection() + reflect_on_past()                     │
│  ──────────────────────────────────────────                     │
│                                                                 │
│  WHEN TO USE CSR:                                               │
│  ┌─────────────────┬───────────────────────────────────────┐    │
│  │ Trigger         │ Action                                │    │
│  ├─────────────────┼───────────────────────────────────────┤    │
│  │ Session Start   │ reflect_on_past("similar tasks")      │    │
│  │ PreCompact      │ store_reflection(state, ["ralph"])    │    │
│  │ Stuck (5+ iter) │ reflect_on_past("blocking error")     │    │
│  │ Session End     │ store_reflection(narrative, outcome)  │    │
│  └─────────────────┴───────────────────────────────────────┘    │
│                                                                 │
│  ✓ Semantic search across all past sessions                     │
│  ✓ Tagged for precise retrieval                                 │
│  ✓ Backup if .md file lost                                      │
│  ✓ Cross-session learning                                       │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

### Why Both Layers Are Needed

| Scenario | .ralph_state.md | CSR Hooks |
|----------|-----------------|-----------|
| Every iteration state | ✅ Fast, no latency | ❌ Too slow |
| Cross-session learning | ❌ File is session-scoped | ✅ Persistent in Qdrant |
| Backup/recovery | ❌ Single point of failure | ✅ Redundant storage |
| Semantic search | ❌ Exact match only | ✅ Vector similarity |
| Debugging | ✅ Human-readable file | ❌ Requires tool call |

---

## 3. Proposed Solution: Tiered Memory Integration

Based on adversarial review findings, we propose a **three-tier architecture** that progressively adds complexity only where value is proven.

### Tier 1: Pre-Session Memory Lookup (MVP)

**Complexity:** Low
**Value:** High
**Risk:** Low

```
┌─────────────────────────────────────────────────────────────┐
│                     BEFORE RALPH STARTS                     │
│                                                             │
│  1. User provides task description                          │
│  2. CSR search: "Find similar past sessions"               │
│  3. Generate .ralph_context.md with relevant memories       │
│  4. Ralph loop starts with static context                   │
│                                                             │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                    STANDARD RALPH LOOP                      │
│                                                             │
│  while true; do                                             │
│      claude --prompt "$PROMPT" --context .ralph_context.md  │
│  done                                                       │
│                                                             │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                   AFTER RALPH COMPLETES                     │
│                                                             │
│  1. Generate session narrative via Anthropic Batch API      │
│  2. Extract metadata: tools used, errors, solutions         │
│  3. Store in Qdrant with outcome tracking                   │
│  4. Available for future pre-session lookups                │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

**Implementation:**

```bash
# New command: /ralph-loop-with-memory
/ralph-loop-with-memory "Build a REST API for todos" \
    --completion-promise "COMPLETE" \
    --max-iterations 50
```

Under the hood:
1. Before loop starts, invoke `csr_reflect_on_past("Build REST API todos")`
2. Format top 3 results into `.ralph_context.md`
3. Inject context file into Claude's initial prompt
4. After completion, trigger narrative generation

### Tier 2: Lazy Memory Injection (When Stuck)

**Complexity:** Medium
**Value:** Medium
**Risk:** Medium

Only search memory when iterations stall:

```python
class LazyMemoryBridge:
    def __init__(self):
        self.stuck_threshold = 5  # consecutive failures
        self.consecutive_failures = 0
        self.last_git_sha = None

    def check_progress(self):
        """Called after each iteration via git hook"""
        current_sha = self.get_git_head()

        if current_sha == self.last_git_sha:
            self.consecutive_failures += 1
        else:
            self.consecutive_failures = 0
            self.last_git_sha = current_sha

        if self.consecutive_failures >= self.stuck_threshold:
            self.inject_memory_context()
            self.consecutive_failures = 0

    def inject_memory_context(self):
        """One-time memory search when stuck"""
        # Read recent iteration context from git log
        context = subprocess.check_output(
            ["git", "log", "-3", "--oneline"]
        ).decode()

        # Search CSR (via standalone client, not MCP)
        results = self.csr_client.search(
            query=context,
            limit=3,
            min_score=0.5
        )

        # Inject into .ralph_memories.md
        with open('.ralph_memories.md', 'w') as f:
            f.write("# Memory Injection (Detected Stuck Pattern)\n\n")
            for r in results:
                f.write(f"## Past Session: {r.timestamp}\n")
                f.write(f"**Score:** {r.score:.2f}\n")
                f.write(f"{r.content}\n\n")
```

**Trigger Mechanism:**
- Git post-commit hook checks for progress
- If 5 consecutive commits without meaningful changes, trigger memory search
- Higher latency acceptable since user is already blocked

### Tier 3: Real-Time Memory Stream (Future)

**Complexity:** High
**Value:** Uncertain
**Risk:** High

Full per-iteration memory injection. Only pursue after Tier 1 & 2 prove value.

**Requirements:**
- Shared configuration module between CSR MCP and Memory Bridge
- Aggressive caching (hot/warm/cold tiers)
- Local-only embeddings to minimize latency
- Outcome tracking with git SHA validation

**Not Recommended** for initial implementation.

---

## 3. Critical Design Decisions

### 3.1 MCP Unavailability Workaround

**Problem:** MCP tools are not available in subprocess context (Ralph bash loop).

**Solution:** Create a standalone Python client that mirrors CSR's search logic:

```python
# mcp-server/src/standalone_client.py

from qdrant_client import QdrantClient
from fastembed import TextEmbedding
import json
import os

class CSRStandaloneClient:
    """Standalone client for CSR when MCP is unavailable"""

    def __init__(self):
        self.config = self._load_config()
        self.qdrant = QdrantClient(
            url=self.config.get("qdrant_url", "http://localhost:6333"),
            api_key=os.environ.get("QDRANT_API_KEY")
        )
        self.embedder = TextEmbedding("sentence-transformers/all-MiniLM-L6-v2")

    def _load_config(self):
        config_path = os.path.expanduser(
            "~/.claude-self-reflect/config/unified-state.json"
        )
        if os.path.exists(config_path):
            with open(config_path) as f:
                return json.load(f)
        return {}

    def search(self, query: str, limit: int = 5, min_score: float = 0.3):
        """Search CSR memories using same logic as MCP server"""
        # Generate embedding
        embedding = list(self.embedder.embed([query]))[0]

        # Search Qdrant
        results = self.qdrant.search(
            collection_name=self._get_collection_name(),
            query_vector=embedding,
            limit=limit,
            score_threshold=min_score
        )

        return self._format_results(results)
```

### 3.2 Outcome Tracking

**Problem:** Memories without outcome context are actively harmful (failed solutions look like successful ones).

**Solution:** Extended metadata schema for Ralph sessions:

```json
{
    "session_id": "ralph_2026_01_04_abc123",
    "task_description": "Build REST API for todos",
    "iterations_total": 15,
    "iterations_successful": 12,
    "iterations_failed": 3,
    "outcome": "COMPLETED",
    "outcome_confidence": 0.95,
    "tools_used": ["file_edit", "bash", "test_runner"],
    "errors_encountered": ["syntax_error", "import_error"],
    "solutions_applied": ["refactor_module", "add_dependency"],
    "final_git_sha": "abc123def456",
    "completion_promise_met": true,
    "session_duration_minutes": 45,
    "memory_injections_used": 2,
    "memory_injection_helpful": true
}
```

### 3.3 Memory Token Limits

**Problem:** Unbounded memory injection overflows context window.

**Solution:** Hard limits and aggressive summarization:

```python
MAX_MEMORY_TOKENS = 2000
MAX_MEMORIES = 3
RELEVANCE_THRESHOLD = 0.6

def format_memories(sessions: List[SearchResult]) -> str:
    """Format memories with hard token limit"""
    output = ["# Relevant Past Sessions\n"]
    tokens_used = 0

    for session in sorted(sessions, key=lambda x: -x.score):
        if session.score < RELEVANCE_THRESHOLD:
            continue
        if len(output) > MAX_MEMORIES:
            break

        summary = truncate_to_tokens(session.content, 500)
        tokens_used += count_tokens(summary)

        if tokens_used > MAX_MEMORY_TOKENS:
            break

        output.append(f"\n## {session.timestamp} (Score: {session.score:.2f})\n")
        output.append(f"**Outcome:** {session.metadata.get('outcome', 'Unknown')}\n")
        output.append(f"{summary}\n")

    return "".join(output)
```

### 3.4 CSR Hook Implementations

**The four CSR integration points with detailed implementation:**

#### 3.4.1 SessionStart Hook

```python
# hooks/ralph_session_start.py
"""
Called when /ralph-loop-with-memory starts.
Searches CSR for relevant past sessions to inject as context.
"""

async def on_session_start(task_description: str, project: str = None):
    """Search CSR for relevant past Ralph sessions"""

    # Search for similar past tasks
    results = await reflect_on_past(
        query=f"ralph loop: {task_description}",
        limit=3,
        min_score=0.5,
        project=project
    )

    if not results:
        return None

    # Format as injectable context
    context = ["# Relevant Past Sessions (from CSR)\n"]
    for r in results:
        if r.metadata.get("outcome") == "COMPLETED":
            context.append(f"\n## ✅ Successful: {r.preview[:100]}...")
            context.append(f"**Approach that worked:** {r.metadata.get('approach', 'Unknown')}\n")
        else:
            context.append(f"\n## ⚠️ Failed: {r.preview[:100]}...")
            context.append(f"**Why it failed:** {r.metadata.get('failure_reason', 'Unknown')}\n")

    # Write to context file
    Path('.ralph_past_sessions.md').write_text('\n'.join(context))

    return results
```

#### 3.4.2 PreCompact Hook

```python
# hooks/ralph_pre_compact.py
"""
Called before context compaction.
Backs up critical state to CSR as insurance.
"""

async def on_pre_compact(session_id: str, iteration: int, state: dict):
    """Store state in CSR before compaction occurs"""

    state_content = f"""
Ralph Session State Backup (Pre-Compaction)
Session: {session_id}
Iteration: {iteration}
Timestamp: {datetime.now().isoformat()}

## Task
{state.get('task', 'Unknown')}

## Current Approach
{state.get('current_approach', 'Unknown')}

## Failed Approaches (DO NOT RETRY)
{json.dumps(state.get('failed_approaches', []), indent=2)}

## Blocking Errors
{json.dumps(state.get('blocking_errors', []), indent=2)}

## Key Decisions
{json.dumps(state.get('decisions', []), indent=2)}

## Files Modified
{json.dumps(state.get('files_modified', []), indent=2)}

## Next Planned Action
{state.get('next_action', 'Unknown')}
"""

    # Store in CSR with tags for retrieval
    await store_reflection(
        content=state_content,
        tags=[
            "ralph_state",
            f"session_{session_id}",
            f"iteration_{iteration}",
            "pre_compact_backup"
        ]
    )

    return True
```

#### 3.4.3 StuckDetection Hook

```python
# hooks/ralph_stuck_detection.py
"""
Called when 5+ iterations without progress detected.
Searches CSR for solutions to similar blocking errors.
"""

async def on_stuck_detected(blocking_error: str, context: str, session_id: str):
    """Search CSR for solutions to similar past problems"""

    # Search for similar errors
    error_results = await reflect_on_past(
        query=f"error solution: {blocking_error}",
        limit=5,
        min_score=0.4
    )

    # Search for similar context
    context_results = await reflect_on_past(
        query=context,
        limit=3,
        min_score=0.5
    )

    # Combine and deduplicate
    all_results = deduplicate(error_results + context_results)

    if not all_results:
        # No past solutions found - store this as a novel problem
        await store_reflection(
            content=f"Novel blocking problem: {blocking_error}\nContext: {context}",
            tags=["ralph_stuck", "novel_problem", f"session_{session_id}"]
        )
        return None

    # Format suggestions
    suggestions = ["# CSR Found Relevant Past Solutions\n"]
    for r in all_results:
        if r.metadata.get("outcome") == "COMPLETED":
            suggestions.append(f"\n## Past Solution (Score: {r.score:.2f})")
            suggestions.append(f"{r.content}\n")

    # Inject into context
    with open('.ralph_memories.md', 'a') as f:
        f.write('\n'.join(suggestions))

    return all_results
```

#### 3.4.4 SessionEnd Hook

```python
# hooks/ralph_session_end.py
"""
Called when Ralph session completes (success or max iterations).
Stores complete session narrative in CSR for future learning.
"""

async def on_session_end(
    session_id: str,
    outcome: str,  # "COMPLETED", "FAILED", "MAX_ITERATIONS"
    iterations: int,
    task: str,
    final_state: dict
):
    """Store complete session narrative in CSR"""

    narrative = f"""
# Ralph Session Complete

## Metadata
- Session ID: {session_id}
- Outcome: {outcome}
- Total Iterations: {iterations}
- Duration: {final_state.get('duration_minutes', 'Unknown')} minutes

## Task
{task}

## Final Approach
{final_state.get('final_approach', 'Unknown')}

## What Worked
{json.dumps(final_state.get('successful_strategies', []), indent=2)}

## What Failed (Don't Retry These)
{json.dumps(final_state.get('failed_approaches', []), indent=2)}

## Key Learnings
{json.dumps(final_state.get('learnings', []), indent=2)}

## Files Created/Modified
{json.dumps(final_state.get('files', []), indent=2)}
"""

    # Determine tags based on outcome
    tags = [
        "ralph_session",
        f"session_{session_id}",
        f"outcome_{outcome.lower()}",
        *final_state.get('concepts', [])
    ]

    # Store with outcome-aware tags
    await store_reflection(content=narrative, tags=tags)

    # If successful, also store the winning strategy separately
    if outcome == "COMPLETED":
        await store_reflection(
            content=f"Successful approach for '{task}': {final_state.get('final_approach')}",
            tags=["ralph_success", "winning_strategy", *final_state.get('concepts', [])]
        )

    return True
```

### 3.5 Security Considerations

**Problem:** Ralph sessions may contain sensitive data (API keys, internal hostnames).

**Solution:** Sanitization pipeline before embedding:

```python
import re

SENSITIVE_PATTERNS = [
    r'(?i)(api[_-]?key|secret|password|token)\s*[=:]\s*["\']?[\w-]+',
    r'\b(?:\d{1,3}\.){3}\d{1,3}\b',  # IP addresses
    r'(?i)sk-[a-zA-Z0-9]{20,}',  # API keys
    r'(?i)ghp_[a-zA-Z0-9]{36}',  # GitHub tokens
]

def sanitize_for_embedding(content: str) -> str:
    """Remove sensitive data before creating embeddings"""
    sanitized = content
    for pattern in SENSITIVE_PATTERNS:
        sanitized = re.sub(pattern, '[REDACTED]', sanitized)
    return sanitized
```

---

## 4. Implementation Plan

### Phase 1: Foundation (Week 1-2)

**Goal:** Create standalone CSR client and basic pre-session lookup.

**Tasks:**
1. [ ] Create `mcp-server/src/standalone_client.py`
2. [ ] Add Ralph session metadata schema
3. [ ] Create `/ralph-loop-with-memory` skill (wrapper around Ralph loop)
4. [ ] Implement `.ralph_context.md` generation
5. [ ] Add sanitization pipeline
6. [ ] Write integration tests

**Deliverables:**
- Working pre-session memory lookup
- Basic CLI command
- Documentation

### Phase 2: Post-Session Learning (Week 3-4)

**Goal:** Capture Ralph session outcomes for future reference.

**Tasks:**
1. [ ] Create `ralph_session_exporter.py`
2. [ ] Integrate with CSR narrative generation
3. [ ] Add outcome tracking fields
4. [ ] Create git hooks for session completion
5. [ ] Implement session quality scoring
6. [ ] Build session analytics dashboard

**Deliverables:**
- Automatic session capture
- Outcome-aware memories
- Quality metrics

### Phase 3: Lazy Memory Injection (Week 5-6)

**Goal:** Inject memories when iterations stall.

**Tasks:**
1. [ ] Implement `LazyMemoryBridge` class
2. [ ] Create git hook for progress detection
3. [ ] Add stuck pattern recognition
4. [ ] Build memory injection trigger
5. [ ] Add memory helpfulness tracking
6. [ ] Performance optimization

**Deliverables:**
- Automatic stuck detection
- Targeted memory injection
- Performance metrics

### Phase 4: Validation & Iteration (Week 7-8)

**Goal:** Validate value and iterate based on real usage.

**Tasks:**
1. [ ] A/B test memory vs. no-memory Ralph sessions
2. [ ] Collect user feedback
3. [ ] Measure iteration reduction metrics
4. [ ] Identify false positive patterns
5. [ ] Tune relevance thresholds
6. [ ] Document best practices

**Deliverables:**
- Validation report
- Tuned parameters
- User documentation

---

## 5. Success Metrics

### Primary Metrics

| Metric | Target | Measurement Method |
|--------|--------|-------------------|
| Iteration reduction | 20% fewer iterations per session | Compare with/without memory |
| Time to completion | 15% faster task completion | Session duration tracking |
| Repeated failure prevention | 50% reduction in known failure patterns | Pattern matching across sessions |
| User satisfaction | 4/5 rating | Post-session survey |

### Secondary Metrics

| Metric | Target | Measurement Method |
|--------|--------|-------------------|
| Memory relevance | >70% of injected memories rated helpful | User feedback |
| Latency overhead | <500ms for pre-session lookup | Performance monitoring |
| False positive rate | <10% of pattern matches are false | Manual review |
| Memory corpus growth | Linear with useful sessions | Storage metrics |

---

## 6. Risk Mitigation

### Technical Risks

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| MCP unavailability breaks design | Certain | High | Standalone client implementation |
| Latency kills iteration speed | High | High | Pre-session only (Tier 1), lazy injection (Tier 2) |
| Stale memories cause harm | Medium | High | Git SHA validation, outcome tracking |
| Memory pollution from failures | Medium | Medium | Outcome-aware filtering |
| Context overflow | Medium | Medium | Hard token limits |

### Organizational Risks

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Complexity creep | High | High | Phased approach, value validation gates |
| Maintenance burden | Medium | Medium | Shared configuration module |
| User confusion | Medium | Low | Clear documentation, opt-in features |

---

## 7. Architecture Diagram

```
┌─────────────────────────────────────────────────────────────────────────────┐
│              Memory-Augmented Ralph Loop (Hybrid Architecture)               │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  ╔═══════════════════════════════════════════════════════════════════════╗  │
│  ║  SESSION START                                                         ║  │
│  ╠═══════════════════════════════════════════════════════════════════════╣  │
│  ║                                                                        ║  │
│  ║  ┌──────────────────┐    CSR Hook    ┌────────────────────────────┐   ║  │
│  ║  │ /ralph-loop      │───────────────▶│ on_session_start()         │   ║  │
│  ║  │ -with-memory     │                │ reflect_on_past(task)      │   ║  │
│  ║  └──────────────────┘                └────────────────────────────┘   ║  │
│  ║           │                                     │                      ║  │
│  ║           │                                     ▼                      ║  │
│  ║           │                          ┌────────────────────────────┐   ║  │
│  ║           │                          │ .ralph_past_sessions.md    │   ║  │
│  ║           │                          │ (Past successes/failures)  │   ║  │
│  ║           │                          └────────────────────────────┘   ║  │
│  ║           │                                     │                      ║  │
│  ╚═══════════╪═════════════════════════════════════╪══════════════════════╝  │
│              │                                     │                         │
│              ▼                                     ▼                         │
│  ╔═══════════════════════════════════════════════════════════════════════╗  │
│  ║  ITERATION LOOP                                                        ║  │
│  ╠═══════════════════════════════════════════════════════════════════════╣  │
│  ║                                                                        ║  │
│  ║  ┌─────────────────────────────────────────────────────────────────┐  ║  │
│  ║  │                                                                  │  ║  │
│  ║  │  while true; do                                                  │  ║  │
│  ║  │      # Claude reads state files (LAYER 1 - Fast)                │  ║  │
│  ║  │      # - .ralph_state.md (current state)                        │  ║  │
│  ║  │      # - .ralph_past_sessions.md (CSR context)                  │  ║  │
│  ║  │                                                                  │  ║  │
│  ║  │      claude --prompt "$PROMPT"                                   │  ║  │
│  ║  │                                                                  │  ║  │
│  ║  │      # Claude updates .ralph_state.md each iteration            │  ║  │
│  ║  │      # Stop hook checks completion promise                       │  ║  │
│  ║  │  done                                                            │  ║  │
│  ║  │                                                                  │  ║  │
│  ║  └─────────────────────────────────────────────────────────────────┘  ║  │
│  ║           │                                                            ║  │
│  ║           │                                                            ║  │
│  ║  ┌────────┴────────────────────────────────────────────────────────┐  ║  │
│  ║  │                                                                  │  ║  │
│  ║  │  LAYER 1: .ralph_state.md (Updated Every Iteration)             │  ║  │
│  ║  │  ─────────────────────────────────────────────────              │  ║  │
│  ║  │  ## Current State (Iteration N)                                 │  ║  │
│  ║  │  Task: Fix authentication timeout                               │  ║  │
│  ║  │  Approach: JWT with Redis session store                         │  ║  │
│  ║  │  Failed: [session cookies, OAuth implicit]                      │  ║  │
│  ║  │  Blocking: Redis pool exhaustion                                │  ║  │
│  ║  │  Next: Increase pool size                                       │  ║  │
│  ║  │                                                                  │  ║  │
│  ║  └──────────────────────────────────────────────────────────────────┘  ║  │
│  ║                                                                        ║  │
│  ╚═══════════════════════════════════════════════════════════════════════╝  │
│              │                                                               │
│              │                                                               │
│  ╔═══════════╪═══════════════════════════════════════════════════════════╗  │
│  ║  CSR HOOKS (LAYER 2 - Smart, At Key Moments)                          ║  │
│  ╠═══════════╪═══════════════════════════════════════════════════════════╣  │
│  ║           │                                                            ║  │
│  ║           ├──────────────────────────────────────────────────────────┐║  │
│  ║           │                                                          │║  │
│  ║           ▼                                                          │║  │
│  ║  ┌─────────────────┐     ┌─────────────────┐     ┌────────────────┐ │║  │
│  ║  │ Context Near    │────▶│ on_pre_compact()│────▶│ Qdrant         │ │║  │
│  ║  │ Limit?          │     │ store_reflection│     │ (Backup)       │ │║  │
│  ║  └─────────────────┘     └─────────────────┘     └────────────────┘ │║  │
│  ║                                                          │          │║  │
│  ║  ┌─────────────────┐     ┌─────────────────┐             │          │║  │
│  ║  │ Stuck? (5+      │────▶│ on_stuck()      │─────────────┘          │║  │
│  ║  │ iterations)     │     │ reflect_on_past │                        │║  │
│  ║  └─────────────────┘     │ + inject results│                        │║  │
│  ║                          └─────────────────┘                        │║  │
│  ║                                                                      │║  │
│  ╚══════════════════════════════════════════════════════════════════════╝│  │
│                                                                          │   │
│              │                                                           │   │
│              ▼                                                           │   │
│  ╔═══════════════════════════════════════════════════════════════════════╗  │
│  ║  SESSION END                                                          ║  │
│  ╠═══════════════════════════════════════════════════════════════════════╣  │
│  ║                                                                        ║  │
│  ║  ┌─────────────────┐    CSR Hook    ┌─────────────────────────────┐   ║  │
│  ║  │ Completion      │───────────────▶│ on_session_end()            │   ║  │
│  ║  │ Detected        │                │ store_reflection(narrative) │   ║  │
│  ║  └─────────────────┘                │ + outcome + learnings       │   ║  │
│  ║                                     └─────────────────────────────┘   ║  │
│  ║                                                  │                     ║  │
│  ║                                                  ▼                     ║  │
│  ║                                     ┌─────────────────────────────┐   ║  │
│  ║                                     │ Qdrant Vector DB            │   ║  │
│  ║                                     │ - Session narrative         │   ║  │
│  ║                                     │ - Outcome (success/fail)    │   ║  │
│  ║                                     │ - Tagged for future search  │   ║  │
│  ║                                     └─────────────────────────────┘   ║  │
│  ║                                                                        ║  │
│  ╚════════════════════════════════════════════════════════════════════════╝  │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘

LEGEND:
────────────────────────────────────────────────────────────────────────────────
LAYER 1 (.ralph_state.md):  Fast, every iteration, zero latency, survives compaction
LAYER 2 (CSR Hooks):        Smart, key moments only, semantic search, cross-session
────────────────────────────────────────────────────────────────────────────────
```

---

## 8. File Structure

```
claude-self-reflect/
├── mcp-server/
│   └── src/
│       ├── standalone_client.py      # NEW: CSR client for Ralph
│       ├── ralph_memory_bridge.py    # NEW: Memory bridge logic
│       └── server.py                 # Existing MCP server
│
├── scripts/
│   ├── ralph/                        # NEW: Ralph integration scripts
│   │   ├── pre_session_lookup.py     # Pre-session memory search
│   │   ├── post_session_export.py    # Session capture & export
│   │   ├── stuck_detection.py        # Lazy injection trigger
│   │   └── memory_formatter.py       # Memory file generation
│   │
│   └── hooks/
│       └── ralph_post_commit.sh      # NEW: Git hook for stuck detection
│
├── hooks/                            # NEW: CSR Integration Hooks
│   ├── __init__.py
│   ├── ralph_session_start.py        # SessionStart: reflect_on_past()
│   ├── ralph_pre_compact.py          # PreCompact: store_reflection()
│   ├── ralph_stuck_detection.py      # Stuck: reflect_on_past() + inject
│   └── ralph_session_end.py          # SessionEnd: store_reflection(narrative)
│
├── skills/                           # NEW: Claude Code skills
│   └── ralph-loop-with-memory.md     # Skill definition
│
├── docs/
│   └── planning/
│       └── memory-augmented-ralph-loop-plan.md  # This document
│
└── tests/
    └── ralph/                        # NEW: Integration tests
        ├── test_standalone_client.py
        ├── test_memory_bridge.py
        ├── test_csr_hooks.py         # NEW: CSR hook tests
        └── test_stuck_detection.py
```

---

## 9. Testing Strategy

### 9.1 Test Categories

#### Unit Tests

```python
# tests/ralph/test_state_file.py

class TestRalphStateFile:
    """Test .ralph_state.md file operations"""

    def test_state_file_creation_on_first_session(self, tmp_path):
        """State file created when none exists"""
        state = RalphState.create(task="Build API", session_id="abc123")
        path = tmp_path / ".ralph_state.md"
        state.save(path)
        assert path.exists()
        assert "Build API" in path.read_text()

    def test_state_file_parsing_with_valid_content(self, sample_state_file):
        """Valid state file parsed correctly"""
        state = RalphState.load(sample_state_file)
        assert state.task == "Build API"
        assert state.iteration == 5
        assert "cookie auth" in state.failed_approaches

    def test_state_file_parsing_with_corrupted_content(self, tmp_path):
        """Corrupted file returns empty state with warning"""
        path = tmp_path / ".ralph_state.md"
        path.write_text("{{{{not valid markdown}}}}")
        state = RalphState.load(path)
        assert state.is_empty()
        assert state.load_warning == "Corrupted state file, starting fresh"

    def test_state_file_atomic_write(self, tmp_path, monkeypatch):
        """Write uses atomic rename to prevent corruption"""
        path = tmp_path / ".ralph_state.md"
        state = RalphState.create(task="Test task")

        # Simulate crash during write
        original_rename = Path.rename
        def crash_on_rename(self, target):
            raise OSError("Simulated disk failure")
        monkeypatch.setattr(Path, 'rename', crash_on_rename)

        with pytest.raises(StateWriteError):
            state.save(path)

        # Backup should still exist
        assert (path.with_suffix('.bak')).exists() or not path.exists()

    def test_state_file_max_size_enforcement(self, tmp_path):
        """State file truncated if exceeds MAX_STATE_SIZE_KB"""
        path = tmp_path / ".ralph_state.md"
        state = RalphState.create(task="Test")

        # Add huge content
        state.failed_approaches = ["x" * 100000]  # 100KB
        state.save(path)

        assert path.stat().st_size < MAX_STATE_SIZE_KB * 1024
        assert "TRUNCATED" in path.read_text()


class TestCSRIntegration:
    """Test CSR hook integrations"""

    @pytest.mark.asyncio
    async def test_session_start_with_csr_available(self, mock_csr_client):
        """Session start searches CSR and injects context"""
        mock_csr_client.reflect_on_past.return_value = [
            SearchResult(content="Past solution: use Redis", score=0.8)
        ]

        context = await on_session_start("Build auth system")

        assert mock_csr_client.reflect_on_past.called
        assert Path('.ralph_past_sessions.md').exists()
        assert "Redis" in Path('.ralph_past_sessions.md').read_text()

    @pytest.mark.asyncio
    async def test_session_start_with_csr_unavailable(self, mock_csr_client):
        """Session start continues gracefully when CSR down"""
        mock_csr_client.reflect_on_past.side_effect = ConnectionError("Qdrant down")

        context = await on_session_start("Build auth system")

        assert context is None
        # Session should still proceed without memory

    @pytest.mark.asyncio
    async def test_session_start_with_timeout(self, mock_csr_client):
        """Session start times out gracefully"""
        async def slow_query(*args, **kwargs):
            await asyncio.sleep(10)  # Longer than timeout
            return []
        mock_csr_client.reflect_on_past = slow_query

        # Should complete within timeout, not hang
        with pytest.raises(asyncio.TimeoutError):
            await asyncio.wait_for(
                on_session_start("Build auth"),
                timeout=CSR_QUERY_TIMEOUT_SECONDS
            )


class TestMemoryInjection:
    """Test memory formatting and injection"""

    def test_token_budget_enforcement(self):
        """Memory injection respects MAX_MEMORY_TOKENS"""
        memories = [
            SearchResult(content="x" * 5000, score=0.9),  # 5000 chars
            SearchResult(content="y" * 5000, score=0.8),
            SearchResult(content="z" * 5000, score=0.7),
        ]

        formatted = format_memories(memories)

        assert count_tokens(formatted) <= MAX_MEMORY_TOKENS
        # Highest scored memories should be prioritized
        assert "x" in formatted

    def test_relevance_threshold_filtering(self):
        """Low-relevance memories filtered out"""
        memories = [
            SearchResult(content="relevant", score=0.8),
            SearchResult(content="irrelevant", score=0.3),
        ]

        formatted = format_memories(memories)

        assert "relevant" in formatted
        assert "irrelevant" not in formatted
```

#### Integration Tests

```python
# tests/ralph/test_integration.py

class TestFullSessionLifecycle:
    """End-to-end session tests"""

    @pytest.mark.integration
    @pytest.mark.asyncio
    async def test_complete_session_with_memory(
        self,
        ralph_runner,
        csr_client,
        clean_state
    ):
        """Full session from start to end with memory features"""

        # Start session
        session = await ralph_runner.start(
            task="Create hello world script",
            max_iterations=5
        )

        # Verify session start hook fired
        assert Path('.ralph_past_sessions.md').exists()

        # Run a few iterations
        for i in range(3):
            await session.iterate()
            state = RalphState.load(Path('.ralph_state.md'))
            assert state.iteration == i + 1

        # End session
        await session.complete(outcome="COMPLETED")

        # Verify session end hook stored to CSR
        results = await csr_client.reflect_on_past(
            query=f"session_{session.session_id}"
        )
        assert len(results) > 0
        assert "COMPLETED" in results[0].content

    @pytest.mark.integration
    @pytest.mark.asyncio
    async def test_session_recovery_after_crash(
        self,
        ralph_runner,
        clean_state
    ):
        """Session recovers gracefully after abnormal termination"""

        # Start session
        session = await ralph_runner.start(task="Build API")
        await session.iterate()
        await session.iterate()

        # Simulate crash - don't call session.complete()
        session_id = session.session_id
        del session

        # Start new session - should detect incomplete previous
        new_session = await ralph_runner.start(task="Build API")

        assert new_session.recovered_from == session_id
        assert new_session.iteration == 3  # Continues from where left off


class TestConcurrency:
    """Test concurrent access scenarios"""

    @pytest.mark.integration
    @pytest.mark.asyncio
    async def test_concurrent_sessions_isolated(self):
        """Multiple Ralph sessions don't interfere"""

        async def run_session(project_dir: Path, task: str):
            async with RalphSession(project_dir, task) as session:
                for _ in range(3):
                    await session.iterate()
                return session.state

        # Run 3 sessions concurrently in different directories
        tasks = [
            run_session(Path("/tmp/proj_a"), "Task A"),
            run_session(Path("/tmp/proj_b"), "Task B"),
            run_session(Path("/tmp/proj_c"), "Task C"),
        ]

        results = await asyncio.gather(*tasks)

        # Each should have its own state
        assert results[0].task == "Task A"
        assert results[1].task == "Task B"
        assert results[2].task == "Task C"
```

#### Chaos Engineering Tests

```python
# tests/ralph/test_chaos.py

class TestChaosScenarios:
    """Chaos engineering tests for resilience"""

    @pytest.mark.chaos
    @pytest.mark.asyncio
    async def test_qdrant_killed_mid_session(
        self,
        docker_compose,
        ralph_runner
    ):
        """Session continues when Qdrant dies mid-session"""

        session = await ralph_runner.start(task="Build API")
        await session.iterate()

        # Kill Qdrant
        docker_compose.kill_service("qdrant")

        # Session should continue with degraded memory
        await session.iterate()  # Should not crash

        state = RalphState.load(Path('.ralph_state.md'))
        assert state.iteration == 2
        assert state.warnings == ["CSR unavailable, continuing without memory"]

        # Restart Qdrant
        docker_compose.start_service("qdrant")

        # Memory should resume
        await session.iterate()
        assert "CSR reconnected" in state.info

    @pytest.mark.chaos
    @pytest.mark.asyncio
    async def test_disk_full_during_state_write(
        self,
        ralph_runner,
        filled_disk
    ):
        """Handles disk full gracefully"""

        session = await ralph_runner.start(task="Build API")

        # Fill disk
        filled_disk.fill()

        # Should fail gracefully
        with pytest.warns(DiskSpaceWarning):
            await session.iterate()

        # Session should continue with in-memory state
        assert session.state.storage_mode == "memory_only"

    @pytest.mark.chaos
    def test_state_file_corrupted_mid_session(
        self,
        ralph_runner
    ):
        """Recovers from mid-session state file corruption"""

        session = ralph_runner.start_sync(task="Build API")
        session.iterate_sync()

        # Corrupt the state file
        Path('.ralph_state.md').write_text("{{{{garbage}}}}")

        # Next iteration should recover
        session.iterate_sync()

        state = RalphState.load(Path('.ralph_state.md'))
        assert state.recovered_from_corruption
        assert state.iteration >= 1
```

### 9.2 Test Infrastructure Requirements

```yaml
# tests/conftest.py fixtures

fixtures_required:
  - mock_csr_client: Mocked CSR for unit tests
  - csr_client: Real CSR for integration tests
  - ralph_runner: Session lifecycle manager
  - clean_state: Cleanup .ralph_state.md between tests
  - docker_compose: Control Qdrant container
  - filled_disk: Simulate disk full condition
  - sample_state_file: Pre-populated state file

test_categories:
  unit:
    markers: ["unit"]
    requirements: ["pytest", "pytest-asyncio", "pytest-mock"]
    run_time: "<30s"

  integration:
    markers: ["integration"]
    requirements: ["docker", "qdrant", "csr"]
    run_time: "<5m"

  chaos:
    markers: ["chaos"]
    requirements: ["docker", "chaos-toolkit"]
    run_time: "<10m"

  load:
    markers: ["load"]
    requirements: ["locust", "grafana"]
    run_time: "<30m"
```

### 9.3 CI/CD Integration

```yaml
# .github/workflows/ralph-memory-tests.yml

name: Ralph Memory Tests

on:
  push:
    paths:
      - 'hooks/**'
      - 'scripts/ralph/**'
      - 'mcp-server/src/standalone_client.py'

jobs:
  unit-tests:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Run unit tests
        run: pytest tests/ralph -m unit --cov=hooks --cov-report=xml
      - name: Upload coverage
        uses: codecov/codecov-action@v3

  integration-tests:
    runs-on: ubuntu-latest
    services:
      qdrant:
        image: qdrant/qdrant:latest
        ports:
          - 6333:6333
    steps:
      - uses: actions/checkout@v4
      - name: Run integration tests
        run: pytest tests/ralph -m integration

  chaos-tests:
    runs-on: ubuntu-latest
    if: github.ref == 'refs/heads/main'
    steps:
      - name: Run chaos tests
        run: pytest tests/ralph -m chaos
```

---

## 10. Error Handling Specification

### 10.1 Error Categories

```python
# hooks/errors.py

class RalphMemoryError(Exception):
    """Base class for Ralph memory errors"""
    pass

class CSRUnavailableError(RalphMemoryError):
    """CSR service is not reachable"""
    recovery_action = "Continue without memory, log warning"

class StateFileCorruptedError(RalphMemoryError):
    """State file is corrupted or unreadable"""
    recovery_action = "Create fresh state, backup corrupted file"

class StateFileLockError(RalphMemoryError):
    """Cannot acquire lock on state file"""
    recovery_action = "Wait with backoff, then force if stale"

class MemoryInjectionError(RalphMemoryError):
    """Failed to inject memories into context"""
    recovery_action = "Continue without injected memories"

class TokenBudgetExceededError(RalphMemoryError):
    """Not enough token budget for memory injection"""
    recovery_action = "Inject minimal/no memories"
```

### 10.2 Graceful Degradation Matrix

| Component | Failure Mode | Degraded Behavior | Recovery Action |
|-----------|--------------|-------------------|-----------------|
| Qdrant | Connection refused | Skip CSR queries | Retry on next iteration |
| Qdrant | Query timeout | Skip this query | Continue with cached |
| CSR MCP | Not responding | Use standalone client | Fall back to file-only |
| State file | Read error | Start fresh state | Backup corrupted file |
| State file | Write error | In-memory only | Retry with backoff |
| State file | Locked | Wait/skip update | Force after timeout |
| Memory search | No results | Continue without context | Log for debugging |
| Memory search | Low relevance | Filter below threshold | Don't inject irrelevant |

### 10.3 Error Handling Implementation

```python
# hooks/error_handling.py

import asyncio
from contextlib import asynccontextmanager
from typing import Optional, TypeVar

T = TypeVar('T')

async def safe_csr_query(
    query_fn: Callable[..., Awaitable[T]],
    *args,
    timeout: float = CSR_QUERY_TIMEOUT_SECONDS,
    fallback: Optional[T] = None,
    **kwargs
) -> Optional[T]:
    """Execute CSR query with timeout and error handling."""
    try:
        result = await asyncio.wait_for(
            query_fn(*args, **kwargs),
            timeout=timeout
        )
        return result
    except asyncio.TimeoutError:
        logger.warning(f"CSR query timed out after {timeout}s")
        metrics.increment("ralph.csr.timeout")
        return fallback
    except ConnectionError as e:
        logger.error(f"CSR connection failed: {e}")
        metrics.increment("ralph.csr.connection_error")
        return fallback
    except Exception as e:
        logger.exception(f"CSR query failed unexpectedly: {e}")
        metrics.increment("ralph.csr.unexpected_error")
        return fallback


@asynccontextmanager
async def state_file_lock(
    path: Path,
    timeout: float = STATE_LOCK_TIMEOUT_SECONDS
):
    """Context manager for exclusive state file access."""
    lock_path = path.with_suffix('.lock')
    lock_fd = None

    try:
        # Acquire lock with timeout
        start = time.time()
        while time.time() - start < timeout:
            try:
                lock_fd = os.open(str(lock_path), os.O_CREAT | os.O_RDWR)
                fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                await asyncio.sleep(0.1)
        else:
            # Check if lock is stale (older than 5 minutes)
            if lock_path.exists():
                age = time.time() - lock_path.stat().st_mtime
                if age > 300:  # 5 minutes
                    logger.warning("Breaking stale lock")
                    lock_path.unlink()
                    lock_fd = os.open(str(lock_path), os.O_CREAT | os.O_RDWR)
                    fcntl.flock(lock_fd, fcntl.LOCK_EX)
                else:
                    raise StateFileLockError(f"Lock held by another process")

        yield

    finally:
        if lock_fd is not None:
            fcntl.flock(lock_fd, fcntl.LOCK_UN)
            os.close(lock_fd)


def safe_state_write(state: RalphState, path: Path) -> bool:
    """Write state file atomically with backup."""
    temp_path = path.with_suffix('.tmp')
    backup_path = path.with_suffix('.bak')

    try:
        # Validate state before write
        if not state.is_valid():
            raise ValueError("Invalid state object")

        # Check size limit
        content = state.to_markdown()
        if len(content) > MAX_STATE_SIZE_KB * 1024:
            content = state.to_markdown_truncated(MAX_STATE_SIZE_KB * 1024)
            logger.warning("State truncated to fit size limit")

        # Write to temp file
        temp_path.write_text(content)

        # Backup existing file
        if path.exists():
            shutil.copy2(path, backup_path)

        # Atomic rename
        temp_path.rename(path)

        metrics.increment("ralph.state.write_success")
        return True

    except OSError as e:
        logger.error(f"State write failed: {e}")
        metrics.increment("ralph.state.write_error")

        # Cleanup temp file
        if temp_path.exists():
            temp_path.unlink()

        # Recovery: restore from backup
        if backup_path.exists() and not path.exists():
            shutil.copy2(backup_path, path)
            logger.info("Restored state from backup")

        return False
```

---

## 11. Hook Implementation (Official Claude Code Hooks)

### 11.1 Claude Code Hook Events (Verified from Official Docs)

Claude Code provides **10 official lifecycle hooks**:

| Hook Event | When It Fires | Available for Ralph? |
|-----------|---------------|----------------------|
| **SessionStart** | At session start or resume | ✅ Yes - Load past memories |
| **SessionEnd** | At session end | ✅ Yes - Store session narrative |
| **PreCompact** | Before context compaction | ✅ Yes - Backup state to CSR |
| **Stop** | When agent finishes responding | ✅ Yes - Ralph loop uses this! |
| **SubagentStop** | When subagent completes | ✅ Yes - For subagent tasks |
| **PreToolUse** | Before any tool call | ✅ Yes - Auto-approve safe ops |
| **PostToolUse** | After tool execution | ✅ Yes - Validate, format output |
| **UserPromptSubmit** | Before processing user input | ✅ Yes - Add context |
| **PermissionRequest** | When permission dialog shown | ✅ Yes - Auto-allow/deny |
| **Notification** | When Claude sends notifications | ⚪ Optional |

### 11.2 Hook Configuration Locations

Hooks are configured in JSON (priority low→high):
1. `~/.claude/settings.json` - User-level (all projects)
2. `.claude/settings.json` - Project-level (in git)
3. `.claude/settings.local.json` - Local project (not committed)

### 11.3 Hook Types

**1. Command Hooks (execute shell scripts)**
```json
{
  "hooks": {
    "PostToolUse": [{
      "matcher": "Write|Edit",
      "hooks": [{
        "type": "command",
        "command": "python /path/to/ralph_post_tool.py"
      }]
    }]
  }
}
```

**2. Prompt Hooks (LLM evaluates context)**
```json
{
  "hooks": {
    "Stop": [{
      "hooks": [{
        "type": "prompt",
        "prompt": "Check if Ralph loop should continue or if task is complete."
      }]
    }]
  }
}
```

### 11.4 Ralph Memory Hook Implementations

**SessionStart Hook (Official - Verified from Claude Code Docs)**

**Matchers available**: `startup`, `resume`, `clear`, `compact`

**Unique feature**: SessionStart is the ONLY hook with access to `$CLAUDE_ENV_FILE` for persisting environment variables.

```json
// .claude/settings.json
{
  "hooks": {
    "SessionStart": [{
      "matcher": "startup|resume",
      "hooks": [{
        "type": "command",
        "command": "${WORKSPACE}/scripts/ralph/session_start_hook.py"
      }]
    }]
  }
}
```

```python
#!/usr/bin/env python3
# scripts/ralph/session_start_hook.py
"""
Triggered at session start/resume. Searches CSR for relevant past sessions.

Input (via stdin): JSON with session_id, transcript_path, source
Output (via stdout): Context to inject into Claude's session
Exit code 0 = success, exit code 2 = blocking error
"""
import sys
import json
import subprocess
from pathlib import Path

def main():
    # Read hook input from stdin (official protocol)
    try:
        input_data = json.load(sys.stdin)
    except json.JSONDecodeError:
        input_data = {}

    session_id = input_data.get('session_id', '')
    source = input_data.get('source', 'startup')

    # Check if this is a Ralph session
    ralph_state = Path('.ralph_state.md')
    if not ralph_state.exists():
        sys.exit(0)  # Not a Ralph session, continue normally

    # Read task from state
    state_content = ralph_state.read_text()
    task = extract_task_from_state(state_content)

    if not task:
        sys.exit(0)

    # Search CSR for relevant memories
    result = subprocess.run(
        ['python', 'mcp-server/src/standalone_client.py', 'search', task],
        capture_output=True, text=True
    )

    if result.returncode == 0:
        try:
            memories = json.loads(result.stdout)
            if memories:
                # Write to context file
                context = format_memories(memories)
                Path('.ralph_past_sessions.md').write_text(context)

                # Output context to stdout (will be shown to Claude)
                print(f"# Loaded {len(memories)} relevant past sessions")
                print("# See .ralph_past_sessions.md for details")
        except json.JSONDecodeError:
            pass

    sys.exit(0)

def extract_task_from_state(content: str) -> str:
    """Extract task from ralph state markdown"""
    for line in content.split('\n'):
        if line.startswith('Task:'):
            return line.replace('Task:', '').strip()
    return ''

def format_memories(memories: list) -> str:
    """Format memories as markdown"""
    output = ["# Relevant Past Sessions (from CSR)\n"]
    for m in memories[:3]:  # Limit to top 3
        output.append(f"\n## Score: {m.get('score', 0):.2f}")
        output.append(f"{m.get('content', '')[:500]}...")
    return '\n'.join(output)

if __name__ == '__main__':
    main()
```

**PreCompact Hook (Official)**
```json
// .claude/settings.json
{
  "hooks": {
    "PreCompact": [{
      "matcher": "auto",
      "hooks": [{
        "type": "command",
        "command": "python scripts/ralph/pre_compact_hook.py"
      }]
    }]
  }
}
```

```python
# scripts/ralph/pre_compact_hook.py
"""
Triggered before context compaction. Backs up state to CSR.
NOTE: This hook is READ-ONLY - cannot block or modify compaction.
"""
import subprocess
from pathlib import Path
from datetime import datetime

def main():
    ralph_state = Path('.ralph_state.md')
    if not ralph_state.exists():
        return

    state_content = ralph_state.read_text()

    # Backup to CSR before compaction
    backup_content = f"""
# Pre-Compaction Backup
Timestamp: {datetime.now().isoformat()}

{state_content}
"""

    subprocess.run([
        'python', 'mcp-server/src/standalone_client.py',
        'store', backup_content,
        '--tags', 'ralph_state,pre_compact_backup'
    ])

    print("State backed up to CSR before compaction")

if __name__ == '__main__':
    main()
```

**SessionEnd Hook (Official - Verified from Claude Code Docs)**

**Reason values**: `clear`, `logout`, `prompt_input_exit`, `other`

**Note**: SessionEnd CANNOT block session termination.

```json
// .claude/settings.json
{
  "hooks": {
    "SessionEnd": [{
      "hooks": [{
        "type": "command",
        "command": "${WORKSPACE}/scripts/ralph/session_end_hook.py"
      }]
    }]
  }
}
```

```python
#!/usr/bin/env python3
# scripts/ralph/session_end_hook.py
"""
Triggered at session end. Stores complete session narrative to CSR.

Input (via stdin): JSON with session_id, transcript_path, cwd, reason
Cannot block session termination - used for cleanup/logging.
"""
import sys
import json
import subprocess
from pathlib import Path
from datetime import datetime

def main():
    # Read hook input from stdin (official protocol)
    try:
        input_data = json.load(sys.stdin)
    except json.JSONDecodeError:
        input_data = {}

    session_id = input_data.get('session_id', 'unknown')
    reason = input_data.get('reason', 'other')
    transcript_path = input_data.get('transcript_path', '')

    ralph_state = Path('.ralph_state.md')
    if not ralph_state.exists():
        sys.exit(0)  # Not a Ralph session

    state = parse_ralph_state(ralph_state.read_text())

    # Determine outcome based on state
    completion_promise_met = 'COMPLETE' in state.get('notes', '') or \
                            state.get('completion_promise_met', False)
    outcome = "COMPLETED" if completion_promise_met else "INCOMPLETE"

    # Generate narrative
    narrative = f"""
# Ralph Session Complete

## Metadata
- Session ID: {session_id}
- End Reason: {reason}
- Timestamp: {datetime.now().isoformat()}

## Task
{state.get('task', 'Unknown')}

## Outcome: {outcome}
## Iterations: {state.get('iteration', 0)}

## What Worked
{json.dumps(state.get('successful_approaches', []), indent=2)}

## What Failed (Don't Retry These)
{json.dumps(state.get('failed_approaches', []), indent=2)}

## Key Learnings
{json.dumps(state.get('learnings', []), indent=2)}
"""

    # Store to CSR
    try:
        subprocess.run([
            'python', 'mcp-server/src/standalone_client.py',
            'store', narrative,
            '--tags', f'ralph_session,outcome_{outcome.lower()},session_{session_id}'
        ], timeout=10)
    except subprocess.TimeoutExpired:
        # Don't block session end on CSR failure
        pass

    sys.exit(0)

def parse_ralph_state(content: str) -> dict:
    """Parse ralph state markdown into dict"""
    state = {}
    current_section = None

    for line in content.split('\n'):
        if line.startswith('Task:'):
            state['task'] = line.replace('Task:', '').strip()
        elif line.startswith('Iteration:'):
            try:
                state['iteration'] = int(line.replace('Iteration:', '').strip())
            except ValueError:
                state['iteration'] = 0
        elif line.startswith('## Failed'):
            current_section = 'failed_approaches'
            state[current_section] = []
        elif line.startswith('## ') and current_section:
            current_section = None
        elif current_section and line.strip().startswith('-'):
            state[current_section].append(line.strip()[1:].strip())

    return state

if __name__ == '__main__':
    main()
```

**Stop Hook for Stuck Detection (Official)**
```json
// .claude/settings.json
{
  "hooks": {
    "Stop": [{
      "hooks": [{
        "type": "prompt",
        "prompt": "Check .ralph_state.md. If the same error appears 3+ times in blocking_errors, search CSR for solutions using reflect_on_past() before continuing."
      }]
    }]
  }
}
```

### 11.5 PreCompact Limitation

**IMPORTANT**: The `PreCompact` hook is **read-only**:
- ✅ CAN: Log, backup state, notify
- ❌ CANNOT: Block compaction, modify what gets compacted

**Workaround for state preservation:**
1. Use `PreCompact` to backup state to CSR
2. Use `.ralph_state.md` file (survives compaction)
3. Claude reads state file at start of each response

### 11.6 Complete Hook Configuration for Ralph Memory

```json
// .claude/settings.json
{
  "hooks": {
    "SessionStart": [{
      "hooks": [{
        "type": "command",
        "command": "python scripts/ralph/session_start_hook.py"
      }]
    }],
    "PreCompact": [{
      "matcher": "auto",
      "hooks": [{
        "type": "command",
        "command": "python scripts/ralph/pre_compact_hook.py"
      }]
    }],
    "SessionEnd": [{
      "hooks": [{
        "type": "command",
        "command": "python scripts/ralph/session_end_hook.py"
      }]
    }],
    "Stop": [{
      "hooks": [{
        "type": "prompt",
        "prompt": "If in a Ralph loop, check .ralph_state.md for stuck patterns before deciding to stop."
      }]
    }]
  }
}
```

---

## 12. Open Questions

### Product Questions

1. **Should memory injection be opt-in or opt-out?**
   - Recommendation: Opt-in initially, opt-out after validation

2. **How should users control memory search scope?**
   - Options: Project-only, cross-project, time-based filtering

3. **What's the UX for reviewing injected memories?**
   - Options: Silent injection, confirmation prompt, preview mode

### Technical Questions

1. **How to handle embedding model version changes?**
   - May need re-embedding of historical sessions

2. **Should we support cloud embeddings for Ralph integration?**
   - Latency concerns vs. quality tradeoff

3. **How to handle very long Ralph sessions (100+ iterations)?**
   - May need session chunking for narrative generation

---

## 10. Appendices

### A. Research Sources

1. **Ralph Wiggum Plugin**: https://github.com/anthropics/claude-code/tree/main/plugins/ralph-wiggum
2. **Original Ralph Technique**: https://ghuntley.com/ralph/
3. **Claude Self-Reflect Architecture**: Internal documentation (CLAUDE.md)

### B. Adversarial Review Summary

Key findings from Codex evaluator adversarial review:

**Showstoppers Identified:**
1. MCP unavailability in subprocess context - addressed via standalone client
2. Per-iteration latency concerns - addressed via tiered approach
3. Complexity creep - addressed via phased implementation

**Risks Mitigated:**
- Stale memories: Git SHA validation
- Memory pollution: Outcome tracking
- Context overflow: Token limits
- Security: Sanitization pipeline

### C. Related Work

- **Ralph Orchestrator**: https://github.com/mikeyobrien/ralph-orchestrator
- **CSR v7.0 Narratives**: AI-powered conversation summarization
- **MCP Tools**: 15+ semantic search tools for conversation memory

---

## 11. Approval & Next Steps

### Approval Required From

- [ ] Project maintainer
- [ ] Security review (for sanitization pipeline)
- [ ] Performance review (for latency requirements)

### Immediate Next Steps

1. Validate value hypothesis with manual pre-session lookup experiment
2. Create standalone client prototype
3. Design skill command interface
4. Begin Phase 1 implementation

---

## 13. BLENDED ARCHITECTURE: Ralph + Batch Pipeline Integration

### Status: DESIGN COMPLETE - Ready for Implementation

This section shows how the **Ralph Wiggum memory features integrate with the existing batch pipeline**. The key insight is that we don't need separate infrastructure - we extend the existing PreCompact → batch-watcher → batch-monitor → Qdrant pipeline with Ralph-specific hooks.

### Existing Infrastructure (v7.0)

The following components are **already operational** and form the foundation:

| Component | File | Status | Function |
|-----------|------|--------|----------|
| PreCompact Hook | `src/runtime/precompact-hook.sh` | ✅ Running | Stages conversations before compaction |
| Batch Watcher | `src/runtime/batch_watcher.py` | ✅ Running | Queues files, triggers at 10+ files OR 30 min |
| Batch Monitor | `src/runtime/batch_monitor.py` | ✅ Running | Polls Batch API, retrieves narratives |
| Narrative Generator | `docs/design/batch_import_all_projects.py` | ✅ Running | AI narrative generation (9.3x quality boost) |
| Qdrant | Docker container | ✅ Running | Vector search across all conversations |

### Blended Architecture Diagram

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│           MEMORY-AUGMENTED RALPH LOOP + BATCH PIPELINE (Blended)                 │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                  │
│  ╔═══════════════════════════════════════════════════════════════════════════╗  │
│  ║  RALPH SESSION START (NEW - Uses Existing CSR)                            ║  │
│  ╠═══════════════════════════════════════════════════════════════════════════╣  │
│  ║                                                                            ║  │
│  ║  ┌───────────────────┐      ┌────────────────────────────────────────┐    ║  │
│  ║  │ /ralph-loop       │      │ scripts/ralph/session_start_hook.py    │    ║  │
│  ║  │ --with-memory     │─────▶│                                        │    ║  │
│  ║  │ "Build REST API"  │      │ 1. Detect .ralph_state.md exists       │    ║  │
│  ║  └───────────────────┘      │ 2. Search Qdrant via reflect_on_past() │    ║  │
│  ║                              │ 3. Write .ralph_past_sessions.md       │    ║  │
│  ║                              └────────────────────────────────────────┘    ║  │
│  ║                                                │                           ║  │
│  ║                                                ▼                           ║  │
│  ║                              ┌────────────────────────────────────────┐    ║  │
│  ║                              │ .ralph_past_sessions.md                │    ║  │
│  ║                              │ (Injected context from past sessions)  │    ║  │
│  ║                              │                                        │    ║  │
│  ║                              │ ## Past Session: 2026-01-03            │    ║  │
│  ║                              │ Task: Build REST API                   │    ║  │
│  ║                              │ Outcome: COMPLETED                     │    ║  │
│  ║                              │ Approach: FastAPI + SQLAlchemy         │    ║  │
│  ║                              └────────────────────────────────────────┘    ║  │
│  ╚═══════════════════════════════════════════════════════════════════════════╝  │
│                                         │                                        │
│                                         ▼                                        │
│  ╔═══════════════════════════════════════════════════════════════════════════╗  │
│  ║  RALPH ITERATION LOOP (Enhanced with State Preservation)                  ║  │
│  ╠═══════════════════════════════════════════════════════════════════════════╣  │
│  ║                                                                            ║  │
│  ║  ┌─────────────────────────────────────────────────────────────────────┐  ║  │
│  ║  │  while true; do                                                      │  ║  │
│  ║  │      # Claude reads state files (LAYER 1 - Fast, Zero Latency)      │  ║  │
│  ║  │      # - .ralph_state.md (current iteration state)                  │  ║  │
│  ║  │      # - .ralph_past_sessions.md (CSR context from SessionStart)    │  ║  │
│  ║  │                                                                      │  ║  │
│  ║  │      claude --prompt "$PROMPT"                                       │  ║  │
│  ║  │                                                                      │  ║  │
│  ║  │      # Claude updates .ralph_state.md EVERY iteration               │  ║  │
│  ║  │      # (Survives compaction - critical for state preservation!)     │  ║  │
│  ║  │  done                                                                │  ║  │
│  ║  └─────────────────────────────────────────────────────────────────────┘  ║  │
│  ║                                                                            ║  │
│  ║  ┌───────────────────────────────────────────────────────────────────┐    ║  │
│  ║  │ .ralph_state.md (Updated Every Iteration - Survives Compaction)   │    ║  │
│  ║  │ ──────────────────────────────────────────────────────────────    │    ║  │
│  ║  │ ## Current State (Iteration 15)                                   │    ║  │
│  ║  │ Task: Fix authentication timeout                                  │    ║  │
│  ║  │ Approach: JWT with Redis session store                            │    ║  │
│  ║  │ Failed: [session cookies, OAuth implicit flow]                    │    ║  │
│  ║  │ Blocking: Redis pool exhaustion                                   │    ║  │
│  ║  │ Files: [auth.py:45, redis_config.yaml:12]                         │    ║  │
│  ║  │ Next: Increase pool size, add connection timeout                  │    ║  │
│  ║  └───────────────────────────────────────────────────────────────────┘    ║  │
│  ║                                                                            ║  │
│  ╚═══════════════════════════════════════════════════════════════════════════╝  │
│                         │                              │                         │
│        ┌────────────────┤                              │                         │
│        ▼                │                              ▼                         │
│  ╔═══════════════════╗  │  ╔═════════════════════════════════════════════════╗  │
│  ║ PRECOMPACT HOOK   ║  │  ║  STUCK DETECTION (NEW - 5+ iterations)          ║  │
│  ║ (EXISTING v7.0)   ║  │  ╠═════════════════════════════════════════════════╣  │
│  ╠═══════════════════╣  │  ║                                                  ║  │
│  ║                   ║  │  ║  ┌──────────────────────────────────────────┐   ║  │
│  ║ precompact-hook.sh║  │  ║  │ Detect: Same error 5+ times in           │   ║  │
│  ║ + Ralph extension:║  │  ║  │         .ralph_state.md blocking_errors  │   ║  │
│  ║                   ║  │  ║  └──────────────────────────────────────────┘   ║  │
│  ║ 1. Stage convo    ║  │  ║                      │                          ║  │
│  ║ 2. Backup ralph   ║  │  ║                      ▼                          ║  │
│  ║    state to CSR   ║  │  ║  ┌──────────────────────────────────────────┐   ║  │
│  ║ 3. store_reflection║  │  ║  │ reflect_on_past("error: {blocking}")    │   ║  │
│  ║    with tags      ║  │  ║  │ → Search CSR for past solutions         │   ║  │
│  ║                   ║  │  ║  │ → Inject into .ralph_memories.md        │   ║  │
│  ╚═══════════════════╝  │  ║  └──────────────────────────────────────────┘   ║  │
│        │                │  ║                                                  ║  │
│        │                │  ╚══════════════════════════════════════════════════╝  │
│        │                │                              │                         │
│        ▼                │                              │                         │
│  ╔═══════════════════════════════════════════════════════════════════════════╗  │
│  ║  EXISTING BATCH PIPELINE (v7.0 - No Changes Needed)                       ║  │
│  ╠═══════════════════════════════════════════════════════════════════════════╣  │
│  ║                                                                            ║  │
│  ║  ┌─────────────────┐    ┌─────────────────┐    ┌────────────────────┐     ║  │
│  ║  │ batch_watcher.py│───▶│ batch_import_   │───▶│ batch_monitor.py   │     ║  │
│  ║  │                 │    │ all_projects.py │    │                    │     ║  │
│  ║  │ Queue files     │    │ Create batch job│    │ Poll & retrieve    │     ║  │
│  ║  │ Trigger at 10+  │    │ AI narratives   │    │ Import to Qdrant   │     ║  │
│  ║  │ OR 30 min       │    │ $0.012/convo    │    │                    │     ║  │
│  ║  └─────────────────┘    └─────────────────┘    └────────────────────┘     ║  │
│  ║                                                          │                 ║  │
│  ║                                                          ▼                 ║  │
│  ║                                                ┌────────────────────┐      ║  │
│  ║                                                │ Qdrant Vector DB   │      ║  │
│  ║                                                │ • AI narratives    │      ║  │
│  ║                                                │ • Ralph metadata   │      ║  │
│  ║                                                │ • Searchable via   │      ║  │
│  ║                                                │   MCP tools        │      ║  │
│  ║                                                └────────────────────┘      ║  │
│  ╚════════════════════════════════════════════════════════════════════════════╝  │
│                                         │                                        │
│                                         ▼                                        │
│  ╔═══════════════════════════════════════════════════════════════════════════╗  │
│  ║  RALPH SESSION END (NEW - Uses Batch Pipeline)                            ║  │
│  ╠═══════════════════════════════════════════════════════════════════════════╣  │
│  ║                                                                            ║  │
│  ║  ┌───────────────────────────────────────────────────────────────────┐    ║  │
│  ║  │ scripts/ralph/session_end_hook.py                                  │    ║  │
│  ║  │                                                                    │    ║  │
│  ║  │ 1. Parse .ralph_state.md for final state                          │    ║  │
│  ║  │ 2. Determine outcome (COMPLETED/FAILED/MAX_ITERATIONS)            │    ║  │
│  ║  │ 3. Generate session narrative with Ralph-specific metadata:       │    ║  │
│  ║  │    - session_id, iteration_count, outcome                         │    ║  │
│  ║  │    - successful_approaches, failed_approaches                     │    ║  │
│  ║  │    - completion_promise_met: true/false                           │    ║  │
│  ║  │ 4. Queue for batch processing (uses existing batch_watcher!)      │    ║  │
│  ║  │ 5. Store outcome-tagged reflection via store_reflection()         │    ║  │
│  ║  └───────────────────────────────────────────────────────────────────┘    ║  │
│  ║                                                                            ║  │
│  ╚════════════════════════════════════════════════════════════════════════════╝  │
│                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────┘

INTEGRATION POINTS:
─────────────────────────────────────────────────────────────────────────────────
1. SessionStart → Uses existing Qdrant + reflect_on_past() (no new infrastructure)
2. PreCompact → Extends existing precompact-hook.sh with Ralph state backup
3. Stuck Detection → Uses existing reflect_on_past() for solution search
4. SessionEnd → Queues to existing batch_watcher for narrative generation
5. All narratives → Processed by existing batch pipeline (9.3x quality boost)
─────────────────────────────────────────────────────────────────────────────────
```

### Integration Implementation Details

#### 1. SessionStart Hook Integration

**Extends:** Existing SessionStart hook in `.claude/settings.json`
**Uses:** Existing Qdrant + `reflect_on_past()` MCP tool

```python
# scripts/ralph/session_start_hook.py
"""
Triggered at session start. Uses EXISTING CSR infrastructure to search
for relevant past Ralph sessions.
"""

import subprocess
import json
from pathlib import Path

def main():
    ralph_state = Path('.ralph_state.md')
    if not ralph_state.exists():
        return  # Not a Ralph session

    task = extract_task_from_state(ralph_state.read_text())

    # Use EXISTING standalone_client (same as batch system uses)
    from mcp_server.src.standalone_client import CSRStandaloneClient
    client = CSRStandaloneClient()

    # Search for past Ralph sessions with same/similar task
    results = client.search(
        query=f"ralph session: {task}",
        limit=3,
        min_score=0.5
    )

    # Filter for completed sessions (avoid repeating failures)
    successful = [r for r in results if r.metadata.get('outcome') == 'COMPLETED']

    # Write context file for Claude to read
    if successful:
        context = format_past_sessions(successful)
        Path('.ralph_past_sessions.md').write_text(context)
```

#### 2. PreCompact Hook Integration

**Extends:** Existing `precompact-hook.sh`
**Adds:** Ralph state backup to CSR before compaction

```bash
# src/runtime/precompact-hook.sh (ENHANCED)
#!/bin/bash

# EXISTING: Stage conversation for batch processing
timeout $IMPORT_TIMEOUT bash -c "
    source '$VENV_PATH/bin/activate' 2>/dev/null
    python '$CLAUDE_REFLECT_DIR/src/runtime/import-latest.py'
"

# NEW: If Ralph session, also backup state to CSR
if [ -f ".ralph_state.md" ]; then
    python3 << 'PYTHON'
from pathlib import Path
from mcp_server.src.standalone_client import CSRStandaloneClient

state = Path('.ralph_state.md').read_text()
client = CSRStandaloneClient()
client.store_reflection(
    content=f"Pre-compaction Ralph state backup:\n{state}",
    tags=["ralph_state", "pre_compact_backup"]
)
PYTHON
fi

exit 0  # Never block compaction
```

#### 3. SessionEnd Hook Integration

**Extends:** Existing SessionEnd hook
**Uses:** Existing batch_watcher queue for narrative generation

```python
# scripts/ralph/session_end_hook.py
"""
Triggered at session end. Uses EXISTING batch pipeline for narrative
generation, but adds Ralph-specific metadata.
"""

from pathlib import Path
import json

def main():
    ralph_state = Path('.ralph_state.md')
    if not ralph_state.exists():
        return

    state = parse_ralph_state(ralph_state.read_text())

    # Determine outcome
    outcome = "COMPLETED" if state.get('completion_promise_met') else "INCOMPLETE"

    # Create narrative with Ralph metadata
    narrative = {
        "type": "ralph_session",
        "task": state.get('task'),
        "outcome": outcome,
        "iterations": state.get('iteration', 0),
        "successful_approaches": state.get('successful_approaches', []),
        "failed_approaches": state.get('failed_approaches', []),
        "learnings": state.get('learnings', [])
    }

    # Queue for EXISTING batch processing
    # The batch_watcher will pick this up and trigger narrative generation
    from src.runtime.batch_watcher import BatchQueue, BatchWatcherConfig
    queue = BatchQueue(BatchWatcherConfig())
    queue.add(
        file_path=str(Path.cwd() / '.ralph_session.jsonl'),
        project=Path.cwd().name,
        metadata=narrative
    )

    # Also store immediate reflection (for quick access)
    from mcp_server.src.standalone_client import CSRStandaloneClient
    client = CSRStandaloneClient()
    client.store_reflection(
        content=format_session_summary(narrative),
        tags=["ralph_session", f"outcome_{outcome.lower()}"]
    )
```

#### 4. Stuck Detection Integration

**Uses:** Existing `reflect_on_past()` for solution search
**Trigger:** Stop hook with prompt checking for stuck patterns

```json
// .claude/settings.json
{
  "hooks": {
    "Stop": [{
      "hooks": [{
        "type": "prompt",
        "prompt": "Check .ralph_state.md for stuck patterns. If the same error appears 3+ times in blocking_errors, use reflect_on_past() to search CSR for solutions before continuing."
      }]
    }]
  }
}
```

### What This Integration Provides

| Feature | How It Works | Infrastructure Used |
|---------|--------------|---------------------|
| Pre-session memory | SessionStart hook searches Qdrant | EXISTING: reflect_on_past() |
| State preservation | .ralph_state.md survives compaction | NEW: File + CSR backup |
| Stuck recovery | Stop hook triggers CSR search | EXISTING: reflect_on_past() |
| Session narrative | SessionEnd queues to batch | EXISTING: batch_watcher |
| AI-powered summary | Batch API generates narrative | EXISTING: batch pipeline |
| Cross-session search | MCP tools query Qdrant | EXISTING: All MCP tools |

### Implementation Checklist

**Phase 1: Hook Scripts** (NEW - Uses existing infrastructure)
- [ ] `scripts/ralph/session_start_hook.py` - Search past sessions
- [ ] `scripts/ralph/session_end_hook.py` - Queue session narrative
- [ ] Enhance `precompact-hook.sh` - Add Ralph state backup

**Phase 2: Configuration** (Modify existing)
- [ ] Update `.claude/settings.json` - Add Ralph-specific matchers
- [ ] Add Stop hook prompt - Stuck detection trigger

**Phase 3: State File Format** (NEW)
- [ ] Define `.ralph_state.md` schema
- [ ] Add Claude instructions for state updates
- [ ] Add state parsing utilities

**No Changes Needed:**
- ✅ batch_watcher.py - Already handles any queued files
- ✅ batch_monitor.py - Already polls and imports results
- ✅ batch_import_all_projects.py - Already generates narratives
- ✅ Qdrant - Already stores and indexes
- ✅ MCP tools - Already enable search

---

**Document Status:** DESIGN COMPLETE - Blended Architecture
**Version:** 4.0 (Blended with Existing Batch Pipeline)
**Ralph Loop Iterations:** 6

### Key Design Insight

The Memory-Augmented Ralph Loop **does not require new infrastructure**. Instead, it extends the existing v7.0 batch pipeline with lightweight hooks:

```
┌─────────────────────────────────────────────────────────────────────────┐
│                        WHAT'S NEW (Ralph-Specific)                       │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│  NEW Scripts (3 files):                                                  │
│  • scripts/ralph/session_start_hook.py   (~50 lines)                    │
│  • scripts/ralph/session_end_hook.py     (~80 lines)                    │
│  • Enhanced precompact-hook.sh           (+10 lines)                    │
│                                                                          │
│  NEW Configuration:                                                      │
│  • .claude/settings.json modifications   (hook matchers)                │
│  • Stop hook prompt for stuck detection                                  │
│                                                                          │
│  NEW State Files:                                                        │
│  • .ralph_state.md (created by Claude each iteration)                   │
│  • .ralph_past_sessions.md (created by SessionStart hook)               │
│                                                                          │
├─────────────────────────────────────────────────────────────────────────┤
│                       WHAT'S REUSED (v7.0 Pipeline)                      │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│  Existing Services (No Changes):                                         │
│  • batch_watcher.py      - Queues files, triggers batch                 │
│  • batch_monitor.py      - Polls API, imports to Qdrant                 │
│  • batch_import_all_projects.py - AI narrative generation               │
│  • Qdrant                - Vector search                                 │
│  • All MCP tools         - reflect_on_past(), store_reflection(), etc.  │
│  • precompact-hook.sh    - Extended, not replaced                       │
│                                                                          │
│  Total Lines Changed in Existing Code: ~10 (precompact extension)       │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

### Verification Status

| Component | Verified? | Source |
|-----------|-----------|--------|
| SessionStart hook | ✅ Yes | Official Claude Code docs via claude-code-guide |
| SessionEnd hook | ✅ Yes | Official Claude Code docs via claude-code-guide |
| PreCompact hook | ✅ Yes | Official Claude Code docs (read-only) |
| Stop hook | ✅ Yes | Official Claude Code docs |
| Hook stdin/stdout protocol | ✅ Yes | Official Claude Code docs |
| Testing strategy | ✅ Yes | Unit, integration, chaos tests defined |
| Error handling | ✅ Yes | Graceful degradation matrix defined |

### Iteration History

| Iteration | Key Changes |
|-----------|-------------|
| 1 | Initial research, tiered architecture, adversarial review |
| 2 | Added compaction problem analysis, hybrid state preservation (file + CSR), detailed CSR hook implementations |
| 3 | Added comprehensive testing strategy (unit, integration, chaos tests), error handling specification, graceful degradation matrix |
| 4 | **CORRECTED** hook documentation using official Claude Code docs - confirmed 10 lifecycle hooks exist (SessionStart, SessionEnd, PreCompact, Stop, etc.), added JSON configuration examples, Python hook scripts |
| 5 | **VERIFIED** hooks via claude-code-guide agent: SessionStart (matchers: startup/resume/clear/compact, has CLAUDE_ENV_FILE), SessionEnd (reason: clear/logout/prompt_input_exit/other, cannot block), updated Python scripts with stdin/stdout protocol |
| 6 | **BLENDED ARCHITECTURE** with existing v7.0 batch pipeline. Documented how Ralph hooks integrate with: PreCompact → batch_watcher → batch_monitor → Qdrant. Key insight: NO new infrastructure needed - just 3 new hook scripts (~140 lines total) that extend existing services. Added production pipeline flow diagram showing integration points. |

### Refinement Summary (Iteration 2)

**New Insights Incorporated:**
1. **Why Ralph Works**: Models aren't trained for long autonomous work - Ralph exploits this by refusing to accept stop signals
2. **The Compaction Problem**: Critical state is lost when context is summarized - this causes repeated failures
3. **Hybrid Solution**: Use files for fast intra-session state + CSR hooks for smart cross-session learning

**CSR Integration Points Added:**
- `on_session_start()`: Search past sessions before starting
- `on_pre_compact()`: Backup state to CSR before compaction
- `on_stuck_detected()`: Search CSR for solutions when blocked
- `on_session_end()`: Store narrative with outcome for future learning

**Confidence:** High - Addresses both intra-session compaction and cross-session learning
