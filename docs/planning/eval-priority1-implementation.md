# Evaluation System Priority 1: Session-Start Integration

**Branch**: `feature/eval-system-session-start`
**Timeline**: 1-2 days
**Status**: Planning → Implementation
**Owner**: Development Team

---

## 🎯 Motivation

### Problem Statement
Claude Self-Reflect has comprehensive evaluation infrastructure (ground truth generator, 48 stored evals, evaluation scripts) but **none of it runs automatically**. Users have no visibility into system health, search quality degradation, or performance regressions until they encounter problems.

### Why This Matters
- **Early Detection**: Catch regressions before users report them
- **Confidence**: Know when the system is healthy vs. degraded
- **Debugging**: When issues arise, eval history shows what changed
- **Foundation**: Sets up infrastructure for Pattern Learning (Priority 2)

### Current Pain Points
1. No automated evaluation runs
2. Manual script execution required
3. No feedback on system health at session start
4. Regression detection is reactive, not proactive

---

## 📋 Solution Overview

### What We're Building
A lightweight, opt-in evaluation system that runs at session start, providing immediate feedback on MCP tool health, search quality, and performance.

### Key Components
1. **Golden Query Corpus** - 20 real-world evaluation tasks
2. **Session-Start Script** - Fast (<30s) health check runner
3. **Startup Banner** - Visual feedback on eval status
4. **Configuration** - Opt-in via environment variable

### Design Principles
- ⚡ **Fast**: <30 seconds execution time
- 🔇 **Non-blocking**: Warns but doesn't prevent session start
- 🎯 **Focused**: Tests critical paths only (full evals run separately)
- 📊 **Actionable**: Clear pass/fail with debugging hints

---

## 🏗️ Technical Architecture

### Component Breakdown

#### 1. Golden Query Corpus
**File**: `scripts/evaluation/evaluation_tasks.json`

```json
{
  "version": "1.0",
  "description": "Real-world evaluation tasks for session health checks",
  "quick_tests": [...],  // 5 fast tests for session start
  "full_tests": [...]    // 20 comprehensive tests
}
```

**Task Categories**:
- Semantic search (5 tasks) - Accuracy, relevance, ranking
- Temporal search (3 tasks) - Time-constrained queries
- File-based search (3 tasks) - Finding code modifications
- Concept search (3 tasks) - Theme-based discovery
- Tool selection (3 tasks) - Correct tool for task
- Token efficiency (3 tasks) - Brief vs full mode

#### 2. Session-Start Evaluator
**File**: `scripts/evaluation/session_start_eval.py`

```python
class SessionStartEvaluator:
    """Lightweight eval runner optimized for speed"""

    def run_quick_checks(self) -> EvalSummary:
        # 5 critical tests only:
        # 1. Qdrant connectivity
        # 2. Search accuracy (1 query)
        # 3. Performance (<500ms)
        # 4. Token efficiency
        # 5. Tool availability
```

**Performance Targets**:
- Total execution: <30 seconds
- Single test: <5 seconds
- Timeout per test: 10 seconds
- Parallel execution where possible

#### 3. Session Hook Integration
**Location**: `CLAUDE.md` - Session Start section

```markdown
## 🧪 Session Health Check (Automatic)

If enabled, Claude Code runs a quick evaluation on startup:

```bash
# Automatic (if EVAL_ON_STARTUP=true)
python scripts/evaluation/session_start_eval.py --quick

# Manual run anytime
python scripts/evaluation/session_start_eval.py
```

**Startup Banner Example**:
```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
🧪 Claude Self-Reflect Health Check
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
✅ Qdrant Connection      (12ms)
✅ Search Accuracy         (245ms, score: 0.68)
✅ Performance Target      (avg: 234ms, p95: 450ms)
✅ Token Efficiency        (52% reduction in brief mode)
✅ Tool Availability       (15/15 tools responding)

📊 Overall: HEALTHY (5/5 passed)
⏱️  Total time: 1.2s
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

#### 4. Configuration
**File**: `.env` (and `.env.example`)

```bash
# Evaluation Settings
EVAL_ON_STARTUP=false           # Enable session-start evals (opt-in)
EVAL_TIMEOUT_SECONDS=30         # Max time for eval run
EVAL_PERFORMANCE_TARGET_MS=500  # Target latency for searches
```

---

## ✅ Acceptance Criteria

### Functional Requirements

#### FR1: Golden Query Corpus
- [ ] 20 evaluation tasks defined with real-world prompts
- [ ] Each task has: prompt, expected_tools, verify_response, category
- [ ] 5 tasks marked as "quick" for session-start checks
- [ ] Tasks validated against actual system (all pass)
- [ ] JSON schema is valid and parseable

#### FR2: Session-Start Script
- [ ] Executes in <30 seconds for quick checks
- [ ] Returns clear pass/fail status per test
- [ ] Non-zero exit code if any test fails (for CI integration)
- [ ] Graceful degradation if Qdrant unavailable
- [ ] Outputs structured JSON for programmatic parsing
- [ ] Human-readable console output with colors
- [ ] `--quick` flag runs 5 tests only
- [ ] Without `--quick` flag runs all 20 tests

#### FR3: Startup Banner
- [ ] Displays eval results in clean, visual format
- [ ] Shows timing per test and total
- [ ] Uses emojis/colors for pass/fail (✅❌)
- [ ] Provides actionable error messages
- [ ] Can be suppressed with `--silent` flag

#### FR4: Configuration
- [ ] EVAL_ON_STARTUP environment variable works
- [ ] Defaults to disabled (opt-in)
- [ ] Can be enabled per-project via .env
- [ ] All eval settings documented in .env.example

### Non-Functional Requirements

#### NFR1: Performance
- [ ] Quick checks complete in <30 seconds (P95)
- [ ] Individual tests timeout at 10 seconds
- [ ] No memory leaks during repeated runs
- [ ] Parallel test execution where possible

#### NFR2: Reliability
- [ ] Graceful failure when services unavailable
- [ ] No crashes on malformed responses
- [ ] Deterministic results for same queries
- [ ] Test data seeding for consistent scoring

#### NFR3: Usability
- [ ] Clear error messages with remediation steps
- [ ] Progress indicators during long runs
- [ ] Summary report at end
- [ ] Exit codes match standard conventions (0=success, 1=failure)

#### NFR4: Maintainability
- [ ] Code follows project style guidelines
- [ ] Comprehensive docstrings
- [ ] Unit tests for core logic
- [ ] Integration tests for end-to-end flow

---

## 📝 Implementation Checklist

### Phase 0: Branch & Documentation ✅
- [ ] Create feature branch: `feature/eval-system-session-start`
- [ ] Write planning document (this file)
- [ ] Create acceptance criteria (above)
- [ ] Review and approve plan

### Phase 1: Golden Query Corpus (4 hours)
- [ ] Create `scripts/evaluation/evaluation_tasks.json`
- [ ] Define 5 semantic search tasks with ground truth
- [ ] Define 3 temporal search tasks (time constraints)
- [ ] Define 3 file-based search tasks
- [ ] Define 3 concept search tasks
- [ ] Define 3 tool selection tasks
- [ ] Define 3 token efficiency tasks
- [ ] Mark 5 tasks as "quick" for session start
- [ ] Validate all tasks run successfully
- [ ] Document task format and schema

### Phase 2: Session-Start Script (6 hours)
- [ ] Create `scripts/evaluation/session_start_eval.py`
- [ ] Implement `SessionStartEvaluator` class
- [ ] Add `run_quick_checks()` method (5 tests)
- [ ] Add `run_full_evaluation()` method (20 tests)
- [ ] Implement parallel test execution
- [ ] Add timeout handling per test
- [ ] Create structured output (JSON + console)
- [ ] Design startup banner formatting
- [ ] Add `--quick`, `--silent`, `--json` flags
- [ ] Handle Qdrant connectivity failures gracefully
- [ ] Add progress indicators
- [ ] Write unit tests for core logic

### Phase 3: Integration (2 hours)
- [ ] Update `CLAUDE.md` with session-start instructions
- [ ] Add eval system to "Action Guide" section
- [ ] Document EVAL_ON_STARTUP configuration
- [ ] Update `.env.example` with eval settings
- [ ] Add eval commands to quick reference
- [ ] Test integration end-to-end

### Phase 4: Testing & Validation (4 hours)
- [ ] Run quick checks successfully (<30s)
- [ ] Run full evaluation successfully
- [ ] Test with Qdrant down (graceful failure)
- [ ] Test with EVAL_ON_STARTUP=true/false
- [ ] Verify all 20 tasks pass
- [ ] Check performance meets targets
- [ ] Validate JSON output schema
- [ ] Test parallel execution works
- [ ] Verify no memory leaks
- [ ] Run through acceptance criteria

### Phase 5: Documentation & PR (2 hours)
- [ ] Write `docs/development/eval-system-state.md`
- [ ] Document motivation and architecture
- [ ] Add troubleshooting guide
- [ ] Create usage examples
- [ ] Update CHANGELOG.md
- [ ] Create PR with clear description
- [ ] Request review from maintainers

---

## 🎯 Success Metrics

### Immediate (Day 1)
- ✅ All 20 golden queries defined and validated
- ✅ Session-start script executes in <30s
- ✅ Startup banner displays correctly

### Short-term (Week 1)
- ✅ 10+ developers opt-in to EVAL_ON_STARTUP
- ✅ Zero false positive failures
- ✅ Catches first real regression

### Medium-term (Month 1)
- ✅ Becomes default for new installations
- ✅ Integrated into CI/CD pipeline
- ✅ Foundation for Priority 2 (Pattern Learning)

---

## 🚧 Risks & Mitigations

| Risk | Impact | Probability | Mitigation |
|------|--------|-------------|------------|
| Eval timeout exceeds 30s | High | Medium | Parallel execution, aggressive timeouts |
| False positive failures | High | Medium | Conservative thresholds, grace period |
| Qdrant unavailable | Medium | Low | Graceful degradation, offline mode |
| Golden queries outdated | Medium | High | Regular review and updates |
| Session start delay | Medium | Low | Make opt-in, skip if >30s |

---

## 🔄 Future Enhancements (Post-Priority 1)

### Priority 2: Pattern Learning (Next)
- AST pattern canonicalization
- Session labeling (good/bad/neutral)
- Pattern scoring and frequency tracking
- MCP tool for real-time pattern analysis

### Priority 3: Robust Evaluation
- CI/CD integration (GitHub Actions)
- Performance regression detection
- IR metrics (nDCG, Recall@10)
- Docker Compose test environment

### Priority 4: Three-Tier Grading
- Tier 1: Deterministic checks (build/test/security)
- Tier 2: Model-based grading (current batch API)
- Tier 3: Human validation interface

---

## 📚 References

- Ground Truth Generator: `docs/design/batch_ground_truth_generator.py`
- Existing Eval Scripts: `scripts/evaluation/run_evaluation.py`
- Consensus Design: `docs/proposals/final-evaluation-consensus.md`
- Pattern Learning Plan: `docs/planning/eval-system-progress.md`
- MCP Evaluation Proposal: `docs/proposals/mcp-evaluation-system.md`

---

**Last Updated**: 2025-10-30
**Status**: Ready for Implementation ✅
