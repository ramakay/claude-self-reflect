# Evaluation System: Current State & Roadmap

**Last Updated**: 2025-10-30
**Status**: Foundation Built, Not Integrated
**Priority**: Session-Start Integration (Priority 1)

---

## Executive Summary

### What It Is
The evaluation system for Claude Self-Reflect provides automated quality assurance by testing how well Claude uses MCP tools to solve real-world tasks. It measures search quality, tool selection accuracy, performance, and token efficiency.

### Current State
**Foundation Exists, Not Production-Ready**
- ✅ Ground truth generator implemented (Batch API)
- ✅ 48 evaluations stored in Qdrant (`ground_truth_evals` collection)
- ✅ Two evaluation scripts available
- ✅ Test suite for narrative generation
- ⚠️ **No automated session-start workflow**
- ⚠️ **No CI/CD integration**
- ⚠️ **No three-tier grading system**

### Why It Matters
- **Proactive Detection**: Catch regressions before users report them
- **Confidence Building**: Know when system is healthy vs. degraded
- **Quality Assurance**: Ensure 9.3x search improvement remains stable
- **Developer Experience**: Fast feedback loop during development

### Goal
**Priority 1**: Session-start integration providing sub-30s health checks with clear pass/fail feedback.

---

## What Exists Today

### 1. Ground Truth Generator
**File**: `/Users/ramakrishnanannaswamy/projects/claude-self-reflect/docs/design/batch_ground_truth_generator.py`

**Purpose**: Generate high-quality evaluation labels using Anthropic Batch API

**Capabilities**:
- Fetches narratives from Qdrant (`v3_all_projects` collection)
- Creates batch evaluation requests using GRADER_PROMPT.md
- Submits to Batch API (50% cost savings: $0.015/eval vs $0.30 streaming)
- Uses Claude Haiku 4.5 for fast processing (minutes vs 24 hours)
- Stores results in `ground_truth_evals` Qdrant collection

**Status**: ✅ Fully implemented, tested successfully

**Example Workflow**:
```bash
# Step 1: Fetch 50 narratives and create batch
python batch_ground_truth_generator.py

# Step 2: Wait ~10 minutes for Haiku processing

# Step 3: Retrieve and store results
python batch_ground_truth_generator.py retrieve
```

**Output**: 48 evaluations currently stored in Qdrant with structured grades:
```python
{
    "conversation_id": "abc123",
    "evaluation": "<xml>...</xml>",
    "scores": {
        "functional_correctness": 0.85,
        "design_quality": 0.78,
        "completeness": 0.90,
        "overall_grade": 0.84
    },
    "model": "claude-haiku-4.5",
    "timestamp": "2025-10-30T..."
}
```

---

### 2. Evaluation Scripts

#### A. run_evaluation.py (MCP Tool Evaluator)
**File**: `/Users/ramakrishnanannaswamy/projects/claude-self-reflect/scripts/evaluation/run_evaluation.py`

**Purpose**: Test Claude's use of MCP tools through actual API calls

**Architecture**:
```python
class MCPEvaluator:
    def run_single_task(task: EvaluationTask) -> EvaluationResult
    def verify_response(task, response, tools_called) -> bool
    def analyze_results() -> Dict[str, Any]
```

**Test Categories**:
- Semantic search accuracy
- Temporal search (time-constrained queries)
- File-based search
- Concept search
- Tool selection validation
- Response verification

**Status**: ⚠️ Prototype stage, requires `evaluation_tasks.json` (not created yet)

**Missing Dependencies**:
- Golden query corpus (evaluation_tasks.json)
- MCP tool definitions (currently mocked)
- Integration with real MCP server

---

#### B. simple_evaluation.py (Direct Tool Testing)
**File**: `/Users/ramakrishnanannaswamy/projects/claude-self-reflect/scripts/evaluation/simple_evaluation.py`

**Purpose**: Direct testing of MCP tools without Claude intermediary

**Tests Implemented**:
1. **Search Accuracy**: Do searches find relevant conversations?
2. **Performance**: Are searches under 500ms target?
3. **Tool Differentiation**: Do different tools behave differently?
4. **Token Efficiency**: Does brief mode reduce tokens by >50%?

**Status**: ✅ Functional, can run today if Qdrant is available

**Example Output**:
```
📝 Test 1: Search Accuracy
  Query: 'docker container errors'
  Success: ✅
  Time: 245ms

⚡ Test 2: Search Performance
  Query: 'testing'
  Time: 234ms (target: <500ms)
  Status: ✅

📊 EVALUATION SUMMARY
Total Tests: 12
Passed: 11
Success Rate: 92%
Average Response Time: 289ms
```

---

### 3. Grader Prompt Template
**File**: `/Users/ramakrishnanannaswamy/projects/claude-self-reflect/docs/design/GRADER_PROMPT.md`

**Purpose**: Standardized prompt for evaluating code generation sessions

**Evaluation Dimensions**:
- Functional Correctness (0.0-1.0): Does it work?
- Design Quality (0.0-1.0): Best practices, maintainability
- Completeness (0.0-1.0): All requirements met?

**Scoring Guidelines**:
- 0.9-1.0: Excellent (production-ready)
- 0.7-0.8: Good (minor issues)
- 0.5-0.6: Acceptable (needs refinement)
- 0.3-0.4: Needs work (significant gaps)
- 0.0-0.2: Inadequate (major issues)

**Output Format**:
```json
{
  "functional_correctness": 0.85,
  "design_quality": 0.78,
  "completeness": 0.90,
  "overall_grade": 0.84,
  "reasoning": "Solution addresses requirements...",
  "strengths": ["Clean code", "Good tests"],
  "weaknesses": ["Missing edge cases"],
  "confidence": 0.85
}
```

---

### 4. Test Suite
**File**: `/Users/ramakrishnanannaswamy/projects/claude-self-reflect/tests/batch_automation/test_narrative_generation.py`

**Tests**:
- Batch API integration
- Narrative generation quality
- Qdrant collection existence (`ground_truth_evals`)
- Data schema validation

**Status**: ✅ Passing in CI/CD

---

## What's NOT Complete

### 1. No Automated Session-Start Workflow
**Problem**: Evaluation runs are manual, requiring developer intervention

**Missing Components**:
- Session-start evaluation script
- Quick health check mode (<30s execution)
- Startup banner with visual feedback
- Configuration via environment variables

**Impact**: Regressions discovered reactively, not proactively

---

### 2. Missing Three-Tier Grading System
**Planned Architecture**:

#### Tier 1: Deterministic Checks (Fast)
- Build success/failure detection
- Test pass/fail counts
- Security issue scanning
- Code quality metrics
- **Confidence**: 0.95 (very reliable)

#### Tier 2: Model-Based Grading (Current Batch API)
- Claude evaluation of quality
- Semantic correctness assessment
- Design pattern analysis
- **Confidence**: 0.70 (good, but subjective)

#### Tier 3: Human Validation (Future)
- Developer feedback loop
- Manual quality review
- Ground truth labeling
- **Confidence**: 1.0 (definitive)

**Current State**: Only Tier 2 implemented (Batch API grading)

---

### 3. No Pattern Learning Implementation
**Concept**: Learn which code patterns correlate with success/failure

**Missing Features**:
- AST pattern canonicalization
- Session labeling (good/bad/neutral)
- Pattern frequency tracking
- MCP tool for pattern analysis

**Planned Design**:
```python
class PatternLearner:
    def canonicalize_pattern(ast_pattern):
        # useState(loading) -> useState($VAR)

    def label_session(conversation):
        # Use conversation signals: "works", "failed", etc.

    def update_pattern_stats(patterns, label):
        # Track: pattern -> {good: 5, bad: 2, neutral: 3}
```

**Status**: Design complete, implementation pending (Priority 2)

---

### 4. No CI/CD Integration
**Missing**:
- GitHub Actions workflow
- Automated evaluation on PR
- Performance regression detection
- Docker Compose test environment

**Desired Flow**:
```yaml
# .github/workflows/eval.yml
on: [pull_request]
jobs:
  evaluate:
    - Run quick evaluation (5 tests)
    - Post results as PR comment
    - Block merge if critical failures
```

---

### 5. No Golden Query Corpus
**Missing**: 20-task evaluation suite with:
- Real-world prompts
- Expected tool calls
- Verification criteria
- Performance targets

**Planned Structure**:
```json
{
  "version": "1.0",
  "quick_tests": [
    {
      "id": "search_docker_errors",
      "prompt": "Find Docker errors from last week",
      "expected_tools": ["search_by_recency"],
      "verify_response": {
        "contains": ["docker", "error"],
        "min_score": 0.6
      },
      "category": "temporal_search"
    }
  ],
  "full_tests": [...] // 20 total
}
```

**Status**: Specification exists (Priority 1 implementation doc), file not created

---

## Past Discussions & Key Decisions

### Evolution: Over-Engineered → Consensus Design

#### Initial Proposal (Rejected)
- Separate MCP tool for evaluation
- Microservices architecture with event buses
- Complex Bayesian scoring
- **Verdict**: 10-100x more complex than needed

#### Consensus Design (Approved)
**Reviewers**: GPT-5, Opus 4.1, Codex
**Document**: `/Users/ramakrishnanannaswamy/projects/claude-self-reflect/docs/proposals/final-evaluation-consensus.md`

**Key Agreements**:
1. ✅ Three-layer architecture (Unit → Integration → Agent)
2. ✅ Subprocess isolation for integration/agent tests
3. ✅ In-process unit tests with mocked Qdrant
4. ✅ Simple frequency counting over Bayesian statistics
5. ✅ Conversation signals for session labeling
6. ✅ Python module execution (`python -m`) not shell scripts

**Divergences Resolved**:
| Model | Focus | Key Insight |
|-------|-------|-------------|
| GPT-5 | Metrics, CI/CD | IR metrics (nDCG, Recall@10), Docker Compose |
| Opus 4.1 | Test Data Quality | Versioned test corpus, deterministic seeds |
| Codex | Implementation | Pipe deadlocks, zombie prevention, robust lifecycle |

**Final Technical Stack**:
- Python module execution (not `run-mcp.sh`)
- Robust process management with stdout/stderr drainage
- Ephemeral test collections per run
- IR metrics + failure categorization
- Cross-platform support (no shell dependencies)

---

### Design Decisions

#### Why Simple Frequency Counting?
**Decision**: Start with basic pattern counting, not Bayesian statistics

**Rationale**:
- Complexity: Bayesian adds unnecessary complexity initially
- Data volume: Need significant data before advanced stats help
- Interpretability: Simple ratios easier to understand/debug
- Iteration: Can upgrade later if needed

**Example**:
```python
# Simple approach (v1)
pattern_score = good_count / (good_count + bad_count)

# Advanced approach (future)
pattern_score = bayesian_posterior(good_count, bad_count, prior)
```

---

#### Why Conversation Signals for Labeling?
**Decision**: Use conversation text to label sessions as good/bad/neutral

**Signals Used**:
- **Good**: "thanks", "perfect", "works", "success"
- **Bad**: "error", "failed", "broken", "doesn't work"
- **Neutral**: Everything else

**Rationale**:
- Availability: Every conversation has these signals
- Immediacy: No external dependencies or delays
- Correlation: Strong correlation with actual session quality
- Privacy: No need for external metrics or telemetry

**Example**:
```python
def label_session(conversation):
    last_messages = conversation[-5:]

    if any("error" in msg or "failed" in msg for msg in last_messages):
        return "bad"
    elif any("thanks" in msg or "works" in msg for msg in last_messages):
        return "good"
    else:
        return "neutral"
```

---

#### Why Project-Specific Patterns First?
**Decision**: Learn patterns per-project, not globally

**Rationale**:
- Relevance: Patterns vary by project type (React vs Flask vs Docker)
- Privacy: No cross-project data sharing initially
- Simplicity: Easier to implement and validate
- Future: Can aggregate to global catalog later

---

## Architecture Overview

### Three-Layer Testing Hierarchy

```
┌─────────────────────────────────────────────┐
│  Agent Layer (End-to-End)                   │
│  - Full Claude + MCP interaction            │
│  - Real conversations, real search          │
│  - Measures: Task completion, tool choice   │
│  - Execution: Subprocess isolation          │
└─────────────────────────────────────────────┘
                    ▲
                    │
┌─────────────────────────────────────────────┐
│  Integration Layer (MCP Tools)              │
│  - Direct MCP tool testing                  │
│  - Mock Claude, real Qdrant                 │
│  - Measures: Search quality, performance    │
│  - Execution: Subprocess with test data     │
└─────────────────────────────────────────────┘
                    ▲
                    │
┌─────────────────────────────────────────────┐
│  Unit Layer (Functions)                     │
│  - Pure function testing                    │
│  - Mock everything (Qdrant, embeddings)     │
│  - Measures: Logic correctness              │
│  - Execution: In-process, fast             │
└─────────────────────────────────────────────┘
```

**Current Implementation**:
- Unit Layer: ⚠️ Partial (import issues)
- Integration Layer: ✅ `simple_evaluation.py`
- Agent Layer: ⚠️ `run_evaluation.py` (needs golden corpus)

---

### Pattern Learning Architecture (Planned)

```
┌──────────────────────┐
│ Conversation Import  │
│  - Extract AST       │
│  - Canonicalize      │
│  - Store in Qdrant   │
└──────────┬───────────┘
           │
           ▼
┌──────────────────────┐
│ Session Labeling     │
│  - Read last 5 msgs  │
│  - Apply heuristics  │
│  - Label: good/bad   │
└──────────┬───────────┘
           │
           ▼
┌──────────────────────┐
│ Pattern Scoring      │
│  - Count frequencies │
│  - Calculate ratios  │
│  - Apply smoothing   │
└──────────┬───────────┘
           │
           ▼
┌──────────────────────┐
│ MCP Tool Interface   │
│  analyze_patterns()  │
│  - Real-time lookup  │
│  - Format report     │
└──────────────────────┘
```

**Status**: Architecture approved, implementation not started

---

### Batch API Integration for Ground Truth

```
┌──────────────┐
│   Fetch      │  1. Get narratives from Qdrant
│  Narratives  │     (v3_all_projects collection)
└──────┬───────┘
       │
       ▼
┌──────────────┐
│   Create     │  2. Build batch requests with
│  Batch File  │     GRADER_PROMPT.md
└──────┬───────┘
       │
       ▼
┌──────────────┐
│   Submit     │  3. Send to Anthropic Batch API
│   to API     │     (Haiku 4.5, ~$0.001/eval)
└──────┬───────┘
       │
       ▼
┌──────────────┐
│    Wait      │  4. Processing time: 5-10 minutes
│  ~10 mins    │     (vs 24 hours for Opus)
└──────┬───────┘
       │
       ▼
┌──────────────┐
│  Retrieve    │  5. Download results (JSONL)
│   Results    │     Parse evaluations
└──────┬───────┘
       │
       ▼
┌──────────────┐
│   Store      │  6. Push to ground_truth_evals
│  in Qdrant   │     collection (48 evals stored)
└──────────────┘
```

**Status**: ✅ Fully operational, 48 evaluations generated

---

## Implementation Roadmap

### Priority 1: Session-Start Integration (1-2 days)
**Goal**: Fast (<30s) health checks on session start

**Tasks**:
1. Create `evaluation_tasks.json` with 20 golden queries
   - 5 marked as "quick" for session start
   - Real-world prompts with expected tools
   - Verification criteria

2. Build `session_start_eval.py`
   - `--quick` mode: 5 tests, <30s execution
   - `--full` mode: 20 tests, comprehensive
   - Parallel execution where possible
   - Timeout handling (10s per test)

3. Design startup banner
   ```
   ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   🧪 Claude Self-Reflect Health Check
   ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   ✅ Qdrant Connection      (12ms)
   ✅ Search Accuracy         (245ms, score: 0.68)
   ✅ Performance Target      (avg: 234ms)
   ✅ Token Efficiency        (52% reduction)
   ✅ Tool Availability       (15/15 tools)

   📊 Overall: HEALTHY (5/5 passed)
   ⏱️  Total time: 1.2s
   ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   ```

4. Add configuration
   ```bash
   # .env
   EVAL_ON_STARTUP=false  # Opt-in
   EVAL_TIMEOUT_SECONDS=30
   EVAL_PERFORMANCE_TARGET_MS=500
   ```

5. Update documentation
   - Add to CLAUDE.md action guide
   - Document in .env.example
   - Create usage examples

**Deliverables**:
- ✅ Golden query corpus (evaluation_tasks.json)
- ✅ Session-start script with quick/full modes
- ✅ Visual startup banner
- ✅ Environment variable configuration
- ✅ Integration documentation

**Success Criteria**:
- Sub-30s execution for quick mode
- Clear pass/fail per test
- Graceful degradation if Qdrant down
- Non-zero exit code for CI integration

**Planning Document**: `/Users/ramakrishnanannaswamy/projects/claude-self-reflect/docs/planning/eval-priority1-implementation.md`

---

### Priority 2: Pattern Learning MVP (1 week)
**Goal**: Discover and score code patterns from conversations

**Tasks**:
1. Pattern canonicalization (Day 1-2)
   - Extend `ast_extractor.py`
   - Replace variables: `useState(loading)` → `useState($VAR)`
   - Store in Qdrant metadata

2. Session labeling (Day 3-4)
   - Implement conversation signal detection
   - Label: good/bad/neutral
   - Backfill existing conversations

3. Pattern scoring (Day 5-6)
   - Count frequencies by label
   - Calculate quality scores
   - Apply Laplace smoothing

4. MCP tool integration (Day 7)
   - Add `analyze_code_patterns` tool
   - Format evaluation reports
   - Test with real code

**Deliverables**:
- Pattern canonicalization function
- Session labeling heuristics
- Pattern scoring system
- MCP tool for analysis

**Success Metrics**:
- 100+ unique patterns discovered
- 80%+ labeling accuracy
- <100ms pattern analysis
- Active use in 10+ projects

**Planning Document**: `/Users/ramakrishnanannaswamy/projects/claude-self-reflect/docs/planning/eval-system-progress.md`

---

### Priority 3: Robust Evaluation with CI/CD (2-3 weeks)
**Goal**: Production-grade evaluation in CI/CD pipeline

**Tasks**:
1. Fix import structure (Week 1)
   - Restructure `mcp-server/` package
   - Enable in-process unit tests
   - Add proper `__init__.py` exports

2. Process lifecycle manager (Week 1)
   - Robust MCP server startup
   - Stdout/stderr drainage
   - Zombie prevention
   - Health check loops

3. Test data infrastructure (Week 2)
   - Ephemeral Qdrant collections
   - Versioned test corpus
   - Deterministic seeds
   - Cleanup between runs

4. CI/CD integration (Week 2-3)
   - Docker Compose test environment
   - GitHub Actions workflow
   - PR comment with results
   - Performance regression alerts

5. IR metrics (Week 3)
   - nDCG@5 for ranking quality
   - Recall@10 for coverage
   - Failure categorization
   - Cost tracking

**Deliverables**:
- Restructured imports
- MCPLifecycle class
- Test data management
- CI/CD pipeline
- IR metrics dashboard

**Success Criteria**:
- In-process unit tests working
- Hermetic CI environment
- <500ms tool latency (excluding startup)
- 25+ evaluation tasks
- Cross-platform support

**Reference**: `/Users/ramakrishnanannaswamy/projects/claude-self-reflect/docs/proposals/final-evaluation-consensus.md`

---

### Priority 4: Complete Three-Tier Grading (1 month)
**Goal**: Multi-confidence evaluation system

**Tasks**:
1. Tier 1: Deterministic checks
   - Build success/failure detection
   - Test result parsing
   - Security scanning integration
   - Code quality metrics

2. Tier 2: Model grading (existing)
   - Continue Batch API usage
   - Expand grader prompt
   - Add confidence scoring

3. Tier 3: Human validation
   - Web interface for review
   - Feedback collection
   - Ground truth updates
   - Active learning loop

4. Confidence-weighted ensemble
   - Combine tier scores
   - Weighted by confidence
   - Uncertainty quantification

**Deliverables**:
- Tier 1 deterministic analyzer
- Tier 2 improvements
- Tier 3 validation UI
- Ensemble scoring system

**Success Criteria**:
- All three tiers operational
- Confidence calibration
- Human feedback integration
- Improved accuracy over single-tier

---

## References

### Core Files

**Ground Truth Generation**:
- `/Users/ramakrishnanannaswamy/projects/claude-self-reflect/docs/design/batch_ground_truth_generator.py` - Batch API integration
- `/Users/ramakrishnanannaswamy/projects/claude-self-reflect/docs/design/GRADER_PROMPT.md` - Evaluation template

**Evaluation Scripts**:
- `/Users/ramakrishnanannaswamy/projects/claude-self-reflect/scripts/evaluation/run_evaluation.py` - MCP tool evaluator
- `/Users/ramakrishnanannaswamy/projects/claude-self-reflect/scripts/evaluation/simple_evaluation.py` - Direct tool testing

**Planning Documents**:
- `/Users/ramakrishnanannaswamy/projects/claude-self-reflect/docs/planning/eval-priority1-implementation.md` - Session-start integration plan
- `/Users/ramakrishnanannaswamy/projects/claude-self-reflect/docs/planning/eval-system-progress.md` - Pattern learning design
- `/Users/ramakrishnanannaswamy/projects/claude-self-reflect/docs/proposals/final-evaluation-consensus.md` - GPT-5/Opus/Codex consensus

**Test Suite**:
- `/Users/ramakrishnanannaswamy/projects/claude-self-reflect/tests/batch_automation/test_narrative_generation.py` - Batch API tests

---

### Design Decisions

**Consensus Review Process**:
- GPT-5: Focus on metrics, CI/CD, hermeticity
- Opus 4.1: Emphasis on test data quality
- Codex: Implementation specifics (process management, cross-platform)

**Key Agreements**:
- Three-layer architecture is sound
- Subprocess for integration/agent, in-process for unit
- Simple frequency counting initially
- Conversation signals for labeling
- Python module execution (not shell scripts)

**Risk Mitigations**:
- Pipe deadlocks → Async I/O or threads
- Import failures → Package restructuring
- Test flakiness → Versioned deterministic data
- Platform lock-in → Python module execution
- Zombie processes → Explicit cleanup

---

## Quick Reference

### Running Evaluations Today

**Simple Direct Testing** (works now):
```bash
cd /Users/ramakrishnanannaswamy/projects/claude-self-reflect
source venv/bin/activate
python scripts/evaluation/simple_evaluation.py
```

**Ground Truth Generation**:
```bash
# Requires ANTHROPIC_API_KEY in .env
python docs/design/batch_ground_truth_generator.py
# Wait 10 minutes
python docs/design/batch_ground_truth_generator.py retrieve
```

**Check Existing Evaluations**:
```bash
# Query ground_truth_evals collection
curl -X POST http://localhost:6333/collections/ground_truth_evals/points/scroll \
  -H "Content-Type: application/json" \
  -d '{"limit": 10, "with_payload": true, "with_vector": false}'
```

---

### Next Steps (Immediate Actions)

**Week 1: Priority 1 Implementation**
1. Create `evaluation_tasks.json` with 20 golden queries
2. Build `session_start_eval.py` script
3. Design startup banner with rich visual feedback
4. Add EVAL_ON_STARTUP configuration
5. Update CLAUDE.md with evaluation guide

**Week 2-3: Pattern Learning**
1. Implement pattern canonicalization
2. Add session labeling logic
3. Build pattern scoring system
4. Create MCP tool interface

**Month 2: CI/CD Integration**
1. Fix import structure for unit tests
2. Build robust process lifecycle manager
3. Set up ephemeral test collections
4. Add GitHub Actions workflow

---

## Conclusion

The evaluation system has a **solid foundation** with ground truth generation, test scripts, and clear architecture. The critical gap is **automation** - converting manual scripts into a seamless session-start workflow that provides proactive quality assurance.

**Priority 1** (session-start integration) is the highest-impact, lowest-complexity next step. It will:
- Catch regressions early
- Build developer confidence
- Set up infrastructure for advanced features
- Require only 1-2 days of focused work

The path forward is clear, the design is validated by multiple AI systems, and the implementation is ready to begin.
