# Memory & Context Management Integration Plan
**Version:** 6.0.0
**Target Release:** Q1 2025
**Status:** Planning → Implementation
**Branch:** `feat/memory-context-management-v2.0.1`

## Executive Summary

Integration of Claude 2.0.1's Memory Tool and Context Editing features with Claude Self-Reflect's existing AST-grep analysis and metadata enrichment systems. This creates a three-layer intelligent memory system with real-time statusline visualization.

---

## Architecture Overview

### Three-Layer Memory System

**Layer 1: Memory Tool (/memories/)**
- Persistent learned patterns from AST-grep analysis
- Cross-conversation distilled insights
- Project-specific knowledge bases
- Evolving architectural decisions

**Layer 2: Claude Self-Reflect (Qdrant)**
- Full conversation history (600+ conversations, 24 projects)
- AST-grep enriched metadata (100+ patterns)
- Temporal queries with 90-day memory decay
- Project-scoped semantic search

**Layer 3: Context Editing (Beta)**
- Automatic tool result clearing at 30k tokens
- Smart retention policies (keep last 5 tool uses)
- Cache-aware clearing (minimum 5k token savings)
- Exclude critical tools (reflect_on_past, search_memory)

---

## Phase 1: Context Management Integration (Week 1-2)

### 1.1 API Configuration

**Add Beta Support:**
```python
# mcp-server/src/context_manager.py
CONTEXT_MANAGEMENT_CONFIG = {
    "beta_header": "context-management-2025-06-27",
    "edits": [{
        "type": "clear_tool_uses_20250919",

        # Trigger at 30k tokens (well before 200k limit)
        "trigger": {
            "type": "input_tokens",
            "value": 30000
        },

        # Keep last 5 tool uses (most recent results)
        "keep": {
            "type": "tool_uses",
            "value": 5
        },

        # Clear at least 5k tokens to justify cache invalidation
        "clear_at_least": {
            "type": "input_tokens",
            "value": 5000
        },

        # NEVER clear these tools
        "exclude_tools": [
            "reflect_on_past",
            "csr_reflect_on_past",
            "search_memory",
            "store_reflection",
            "get_full_conversation",
            "store_to_memory"  # New Memory Tool
        ],

        # Keep tool inputs visible
        "clear_tool_inputs": False
    }]
}
```

### 1.2 Token Counting & Preview

**Preview Before Clearing:**
```python
def preview_context_clearing(messages, tools):
    """Preview clearing impact before applying"""
    response = client.beta.messages.count_tokens(
        model="claude-sonnet-4-5",
        messages=messages,
        tools=tools,
        betas=["context-management-2025-06-27"],
        context_management=CONTEXT_MANAGEMENT_CONFIG
    )

    original = response.context_management["original_input_tokens"]
    after = response.input_tokens
    savings = original - after

    return {
        "will_clear": savings > 0,
        "original_tokens": original,
        "after_tokens": after,
        "savings": savings,
        "percentage": (savings / original * 100) if original > 0 else 0,
        "worth_cache_break": savings >= 5000
    }
```

### 1.3 Response Tracking

**Track Applied Edits:**
```python
def track_context_management(response):
    """Track clearing from API response"""
    cm = response.get("context_management")
    if not cm or not cm.get("applied_edits"):
        return None

    stats = {
        "cleared_tools": 0,
        "cleared_tokens": 0,
        "timestamp": datetime.now().isoformat()
    }

    for edit in cm["applied_edits"]:
        if edit["type"] == "clear_tool_uses_20250919":
            stats["cleared_tools"] = edit["cleared_tool_uses"]
            stats["cleared_tokens"] = edit["cleared_input_tokens"]

    # Update unified state
    update_session_stats(stats)

    return stats
```

### 1.4 Statusline Integration

**Enhanced csr-status Script:**
```python
# scripts/csr-status enhancements
class ContextStatsTracker:
    def __init__(self):
        self.session = {
            "total_cleared_tokens": 0,
            "total_cleared_tools": 0,
            "clearing_events": [],
            "cache_breaks": 0,
            "current_tokens": 0
        }

    def format_statusline(self):
        """Format for statusline display"""

        # No clearing yet
        if self.session["total_cleared_tokens"] == 0:
            if self.session["current_tokens"] > 25000:
                return f"Context: {format_tokens(self.session['current_tokens'])} (clearing soon)"
            return ""

        # Recent clearing
        if self.session["clearing_events"]:
            last = self.session["clearing_events"][-1]
            original = last["original_tokens"]
            after = last["after_tokens"]
            cleared = last["cleared_tokens"]
            tools = last["cleared_tools"]

            return f"Context: {format_tokens(original)}→{format_tokens(after)} (-{format_tokens(cleared)}↓ -{tools}🔧)"

        # Session total
        total = self.session["total_cleared_tokens"]
        tools = self.session["total_cleared_tools"]
        return f"Session: {format_tokens(total)} saved • {tools} tools cleared"

def format_tokens(tokens):
    """Format token count for display"""
    if tokens >= 1000:
        return f"{tokens//1000}k"
    return str(tokens)
```

**Statusline Display Examples:**
```
# Normal operation
CSR: ✓ Connected • 28/29 indexed • Context: 25k tokens

# Approaching threshold
CSR: ✓ Connected • 28/29 indexed • Context: 28k tokens (clearing soon)

# Just cleared
CSR: ✓ Connected • 28/29 indexed • Context: 70k→25k (-45k↓ -8🔧)

# Session summary
CSR: ✓ Connected • Session: 180k saved • 23 tools cleared • 4 cache refreshes
```

**Checklist:**
- [ ] Create `mcp-server/src/context_manager.py`
- [ ] Implement beta header support
- [ ] Add token counting preview endpoint
- [ ] Create ContextStatsTracker class
- [ ] Update `scripts/csr-status` with context formatting
- [ ] Add to unified state management
- [ ] Test with long conversations (50k+ tokens)

---

## Phase 2: Memory Tool Integration (Week 3-4)

### 2.1 Memory Tool Handler

**Create Memory Tool Module:**
```python
# mcp-server/src/memory_tools.py
from pathlib import Path
import json
from typing import Optional, List, Dict, Any
from fastmcp import Context

class MemoryToolHandler:
    """Handle Memory Tool operations with security."""

    def __init__(self, base_path: str = "~/.claude/memories"):
        self.base_path = Path(base_path).expanduser()
        self.base_path.mkdir(parents=True, exist_ok=True)

    def _validate_path(self, path: str) -> Path:
        """Prevent path traversal attacks."""
        full_path = (self.base_path / path).resolve()
        if not str(full_path).startswith(str(self.base_path)):
            raise ValueError("Path traversal detected")
        return full_path

    async def view(self, path: str) -> str:
        """View memory file contents."""
        file_path = self._validate_path(path)
        if not file_path.exists():
            return f"Memory file not found: {path}"
        return file_path.read_text()

    async def create(self, path: str, content: str) -> str:
        """Create new memory file."""
        file_path = self._validate_path(path)
        if file_path.exists():
            return f"Memory file already exists: {path}"

        file_path.parent.mkdir(parents=True, exist_ok=True)
        file_path.write_text(content)
        return f"Created memory: {path}"

    async def str_replace(self, path: str, old: str, new: str) -> str:
        """Replace text in memory file."""
        file_path = self._validate_path(path)
        content = file_path.read_text()

        if old not in content:
            return f"Text not found in {path}"

        new_content = content.replace(old, new, 1)
        file_path.write_text(new_content)
        return f"Updated memory: {path}"

    async def delete(self, path: str) -> str:
        """Delete memory file."""
        file_path = self._validate_path(path)
        file_path.unlink()
        return f"Deleted memory: {path}"
```

### 2.2 MCP Tool Definitions

**Add Memory Tools to MCP Server:**
```python
# In mcp-server/src/server.py
memory_handler = MemoryToolHandler()

@mcp.tool()
async def view_memory(ctx: Context, path: str = Field(description="Path to memory file")) -> str:
    """View contents of a memory file."""
    return await memory_handler.view(path)

@mcp.tool()
async def store_to_memory(
    ctx: Context,
    path: str = Field(description="Path to memory file"),
    content: str = Field(description="Content to store"),
    category: str = Field(default="general", description="Category: patterns/insights/quality/projects")
) -> str:
    """Store insight to Memory Tool and optionally to Qdrant reflection."""

    # Store to Memory Tool
    memory_result = await memory_handler.create(path, content)

    # Also store to Qdrant for searchability
    reflection_result = await store_reflection(
        ctx,
        content,
        tags=[category, f"memory:{path}"]
    )

    return f"{memory_result}\n{reflection_result}"

@mcp.tool()
async def search_memory(
    ctx: Context,
    query: str = Field(description="Search query"),
    category: Optional[str] = Field(default=None, description="Filter by category")
) -> str:
    """Search memory files."""
    results = []
    search_path = memory_handler.base_path

    if category:
        search_path = search_path / category

    for file_path in search_path.rglob("*.md"):
        content = file_path.read_text()
        if query.lower() in content.lower():
            results.append({
                "path": str(file_path.relative_to(memory_handler.base_path)),
                "preview": content[:200]
            })

    if not results:
        return "No memory files found matching query"

    return format_memory_results(results)
```

### 2.3 AST-Grep → Memory Pipeline

**Auto-Store High-Quality Patterns:**
```python
# In scripts/metadata_extractor.py
async def store_quality_patterns(metadata: Dict[str, Any], project: str):
    """Store high-quality AST patterns to Memory Tool."""

    avg_score = metadata.get("avg_quality_score", 0)

    # Only store A+ quality patterns (90+)
    if avg_score < 90:
        return

    # Extract pattern summary
    patterns = metadata.get("pattern_analysis", {})
    ast_elements = metadata.get("ast_elements", [])

    pattern_summary = f"""# Excellent Code Patterns - {project}
**Quality Score:** {avg_score}/100
**Date:** {datetime.now().strftime('%Y-%m-%d')}

## AST Patterns Detected
{json.dumps(patterns, indent=2)}

## Key Elements
{', '.join(ast_elements[:20])}

## Context
This pattern was extracted from a conversation with high code quality.
Consider reusing these patterns in similar contexts.
"""

    # Store to Memory Tool
    memory_path = f"patterns/{project}/excellent-{datetime.now().strftime('%Y%m%d')}.md"
    await memory_handler.create(memory_path, pattern_summary)

    # Update metadata with memory reference
    metadata["memory_references"] = metadata.get("memory_references", [])
    metadata["memory_references"].append(memory_path)
```

### 2.4 Directory Structure

**Memory Organization:**
```
~/.claude/memories/
├── patterns/
│   ├── claude-self-reflect/
│   │   ├── excellent-20250101.md
│   │   ├── async-patterns.md
│   │   └── error-handling.md
│   ├── other-project/
│   │   └── patterns.md
│   └── shared/
│       └── common-patterns.md
├── insights/
│   ├── performance-optimizations.md
│   ├── security-best-practices.md
│   └── architectural-decisions.md
├── quality/
│   ├── ast-patterns-learned.md
│   └── code-smells-resolved.md
└── projects/
    ├── claude-self-reflect/
    │   ├── architecture.md
    │   └── quality-trends.md
    └── README.md
```

**Checklist:**
- [ ] Create `mcp-server/src/memory_tools.py`
- [ ] Implement MemoryToolHandler with path validation
- [ ] Add MCP tools: view_memory, store_to_memory, search_memory
- [ ] Integrate with metadata_extractor.py
- [ ] Create auto-store for quality patterns (90+ score)
- [ ] Update metadata schema with memory_references
- [ ] Create initial directory structure
- [ ] Add security tests (path traversal prevention)

---

## Phase 3: Hybrid Search & Enhanced Metadata (Week 5-6)

### 3.1 Hybrid Search Implementation

**Memory + Vector Search:**
```python
@mcp.tool()
async def enhanced_reflect_on_past(
    ctx: Context,
    query: str = Field(description="Search query"),
    limit: int = Field(default=5, description="Max results"),
    include_memory: bool = Field(default=True, description="Include Memory Tool results")
) -> str:
    """Enhanced search: Memory Tool + Vector Search."""

    results = []

    # 1. Search Memory Tool first (O(1) file search)
    if include_memory:
        memory_results = await search_memory(ctx, query)
        if memory_results:
            results.append({
                "source": "memory",
                "rank": 1.0,  # Highest priority
                "content": memory_results
            })

    # 2. Vector search for conversation history
    vector_results = await csr_reflect_on_past(ctx, query, limit=limit)
    results.extend([{
        "source": "vector",
        "rank": 0.8,
        "content": r
    } for r in vector_results])

    # 3. Merge and rank by source + relevance
    ranked = sorted(results, key=lambda x: x["rank"], reverse=True)

    # 4. Suggest storing if high-value finding
    if should_persist(ranked):
        ctx.info("💡 This finding seems valuable. Consider: store_to_memory()")

    return format_hybrid_results(ranked)
```

### 3.2 Enhanced Metadata Schema

**Extended Metadata Fields:**
```python
metadata = {
    # Existing fields
    "files_analyzed": [],
    "files_edited": [],
    "tools_used": [],
    "concepts": [],
    "ast_elements": [],
    "pattern_analysis": {},
    "avg_quality_score": 0.0,

    # NEW: Memory Tool integration
    "memory_references": [],  # Paths to /memories/ files
    "learned_patterns": [],    # Patterns worth persisting
    "quality_evolution": {     # Track improvements
        "previous_score": 0.0,
        "current_score": 0.0,
        "trend": "improving|stable|declining"
    },

    # NEW: Pattern frequency tracking
    "pattern_frequency": {
        "async_await": 15,
        "error_boundaries": 8,
        "type_guards": 12
    },

    # NEW: Context importance (for Context Editing)
    "context_importance": 0.0,  # 0-1 score, high = preserve longer

    # NEW: Token usage
    "original_tokens": 0,
    "tokens_after_clearing": 0,
    "tokens_saved": 0
}
```

**Checklist:**
- [ ] Implement enhanced_reflect_on_past with hybrid search
- [ ] Update metadata schema with new fields
- [ ] Create pattern_frequency tracking
- [ ] Implement quality_evolution monitoring
- [ ] Add context_importance scoring
- [ ] Update import pipeline with new metadata
- [ ] Test hybrid search accuracy

---

## Phase 4: Statusline Visualization (Week 7)

### 4.1 Comprehensive Status Display

**Enhanced Statusline Generation:**
```python
# scripts/csr-status - Complete statusline logic
def generate_statusline():
    """Generate comprehensive statusline with all features."""

    components = []

    # 1. Connection status
    components.append("CSR: ✓ Connected" if mcp_connected else "CSR: ✗ Disconnected")

    # 2. Indexing progress
    indexed, total = get_collection_stats()
    components.append(f"{indexed}/{total} indexed")

    # 3. Context management (if active)
    context_info = get_context_stats()
    if context_info:
        components.append(context_info)

    # 4. Memory Tool stats (if used)
    memory_info = get_memory_stats()
    if memory_info:
        components.append(memory_info)

    # 5. Quality score (if available)
    quality = get_session_quality()
    if quality:
        components.append(quality)

    return " • ".join(components)
```

### 4.2 Unified State Schema

**Updated unified-state.json:**
```json
{
    "version": "6.0.0",
    "last_updated": "2025-01-01T00:00:00Z",

    "context_management": {
        "total_cleared_tokens": 0,
        "total_cleared_tools": 0,
        "current_tokens": 0,
        "cache_breaks": 0,
        "clearing_events": []
    },

    "memory_tool": {
        "patterns_stored": 12,
        "insights_stored": 5,
        "quality_patterns": 8,
        "last_pattern_stored": "2025-01-01T11:30:00Z"
    },

    "session_quality": {
        "avg_score": 92.5,
        "grade": "A+",
        "pattern_count": 156
    }
}
```

**Checklist:**
- [ ] Enhance generate_statusline() with all components
- [ ] Implement get_context_stats()
- [ ] Implement get_memory_stats()
- [ ] Implement get_session_quality()
- [ ] Update unified-state.json schema
- [ ] Test all statusline display modes

---

## Phase 4.5: Code Review & Quality Gates (Week 7.5)

### 4.5.1 Automated Code Review Process

**Step 1: Cyclomatic Complexity Check**
```bash
#!/bin/bash
echo "=== CYCLOMATIC COMPLEXITY VALIDATION ==="

# Install radon if not present
pip install radon 2>/dev/null

# Check all new modules
NEW_MODULES=(
    "mcp-server/src/context_manager.py"
    "mcp-server/src/memory_tools.py"
    "scripts/enhanced_metadata.py"
)

for module in "${NEW_MODULES[@]}"; do
    echo "Checking: $module"

    # Get complexity stats
    STATS=$(radon cc "$module" -s -a)
    AVG_COMPLEXITY=$(echo "$STATS" | grep "Average complexity:" | awk '{print $3}' | tr -d '()')

    # Parse average complexity
    if (( $(echo "$AVG_COMPLEXITY > 5.0" | bc -l) )); then
        echo "❌ FAIL: $module has avg complexity $AVG_COMPLEXITY (must be <5)"
        exit 1
    fi

    # Check individual functions
    HIGH_COMPLEXITY=$(radon cc "$module" -n C)  # C grade = complexity >10
    if [ -n "$HIGH_COMPLEXITY" ]; then
        echo "❌ FAIL: $module has functions with complexity >10"
        echo "$HIGH_COMPLEXITY"
        exit 1
    fi

    echo "✅ PASS: $module (avg: $AVG_COMPLEXITY)"
done

echo "✅ All modules pass complexity requirements"
```

**Step 2: Zen Tools Code Review**
```bash
# Use mcp__zen__codereview for comprehensive analysis
echo "=== ZEN TOOLS CODE REVIEW ==="

# Review context_manager.py
mcp__zen__codereview \
    --files "mcp-server/src/context_manager.py" \
    --model "anthropic/claude-opus-4" \
    --review-type "full" \
    --focus-on "complexity,security,api-integration" \
    --prompt "Review Context Management implementation focusing on:
    1. Cyclomatic complexity (must be <5 avg, <10 max)
    2. API integration with Claude 2.0.1 beta headers
    3. Security of stats tracking and file I/O
    4. Error handling completeness"

# Review memory_tools.py
mcp__zen__codereview \
    --files "mcp-server/src/memory_tools.py" \
    --model "anthropic/claude-opus-4" \
    --review-type "security" \
    --focus-on "path-traversal,injection,complexity" \
    --prompt "Security-focused review of Memory Tool:
    1. Path traversal prevention completeness
    2. Input sanitization for all operations
    3. Cyclomatic complexity verification
    4. Race condition potential in file operations"
```

**Step 3: AST-GREP Quality Validation**
```bash
# Validate code quality with existing AST-grep patterns
echo "=== AST-GREP QUALITY CHECK ==="

python scripts/ast_grep_final_analyzer.py \
    --files mcp-server/src/context_manager.py \
    --files mcp-server/src/memory_tools.py \
    --output quality-report.json

# Check quality scores
SCORE=$(jq '.overall_quality_score' quality-report.json)
if (( $(echo "$SCORE < 90" | bc -l) )); then
    echo "❌ FAIL: Quality score $SCORE is below 90 threshold"
    jq '.issues' quality-report.json
    exit 1
fi

echo "✅ Quality score: $SCORE/100"
```

### 4.5.2 Pre-commit Validation

**Use mcp__zen__precommit for Git Changes Review:**
```bash
# Comprehensive pre-commit validation
mcp__zen__precommit \
    --path /Users/ramakrishnanannaswamy/projects/claude-self-reflect \
    --model "anthropic/claude-opus-4" \
    --include-staged true \
    --include-unstaged true \
    --prompt "Validate Memory & Context Management integration:

    ORIGINAL REQUEST: Integrate Claude 2.0.1 Memory Tool and Context Editing

    REQUIREMENTS:
    1. Context Management with beta API support
    2. Memory Tool with path traversal protection
    3. Hybrid search (Memory + Vector)
    4. Statusline visualization
    5. Cyclomatic complexity <5 avg, <10 max
    6. AST-grep quality score >90

    CRITICAL CHECKS:
    - All functions complexity validated
    - Security: path traversal prevention
    - API: correct beta headers
    - Tests: comprehensive coverage
    - Documentation: complete and accurate"
```

### 4.5.3 CI/CD Integration

**Update CI Pipeline (.github/workflows/ci.yml):**
```yaml
quality-gates:
  runs-on: ubuntu-latest

  steps:
  - uses: actions/checkout@v5

  - name: Install quality tools
    run: |
      pip install radon pytest pytest-asyncio

  - name: Cyclomatic Complexity Check
    run: |
      radon cc mcp-server/src/context_manager.py -s -a
      radon cc mcp-server/src/memory_tools.py -s -a

      # Fail if avg complexity >5
      python -c "
      import sys
      import subprocess
      result = subprocess.run(['radon', 'cc', 'mcp-server/src/', '-a', '-j'],
                             capture_output=True, text=True)
      import json
      data = json.loads(result.stdout)
      for file, metrics in data.items():
          if metrics.get('average_complexity', 0) > 5.0:
              print(f'FAIL: {file} complexity too high')
              sys.exit(1)
      print('PASS: All modules within complexity limits')
      "

  - name: Security Scan
    run: |
      # Check for hardcoded secrets
      if grep -r "VOYAGE_KEY\|API_KEY.*=" mcp-server/src/ --include="*.py" | grep -v "os.environ"; then
        echo "FAIL: Hardcoded secrets found"
        exit 1
      fi

      # Check path traversal patterns
      if grep -r "os.path.join.*\.\." mcp-server/src/ --include="*.py"; then
        echo "WARNING: Potential path traversal pattern"
      fi

  - name: AST-GREP Quality Validation
    run: |
      python scripts/ast_grep_final_analyzer.py \
        --files mcp-server/src/context_manager.py \
        --files mcp-server/src/memory_tools.py \
        --min-score 90

memory-context-tests:
  needs: quality-gates
  runs-on: ubuntu-latest

  steps:
  - uses: actions/checkout@v5

  - name: Test Context Management
    run: |
      python -m pytest tests/test_context_manager.py -v

  - name: Test Memory Tools
    run: |
      python -m pytest tests/test_memory_tools.py -v

  - name: Test Hybrid Search
    run: |
      python -m pytest tests/test_hybrid_search.py -v
```

### 4.5.4 Manual Code Review Checklist

**Before PR Submission:**
- [ ] Run cyclomatic complexity check: All modules <5 avg
- [ ] Run Zen code review: No critical/high issues
- [ ] Run AST-GREP validation: Score >90
- [ ] Run precommit validation: All checks pass
- [ ] Test all 3 agents: csr-validator, claude-self-reflect-test, reflect-tester
- [ ] Update documentation: README, CHANGELOG, integration plan
- [ ] Create test coverage report: Show >80% coverage
- [ ] Run security scan: No secrets, no path traversal
- [ ] Verify statusline display: All components shown correctly
- [ ] Test both embedding modes: Local and Cloud compatible

**PR Description Template:**
```markdown
## Summary
[Brief description of changes]

## Code Quality Validation
- ✅ Cyclomatic complexity: Avg <5, Max <10
- ✅ Zen code review: Grade A (no critical issues)
- ✅ AST-GREP score: [score]/100
- ✅ Security scan: No issues
- ✅ Test coverage: [percentage]%

## Testing
- ✅ csr-validator: [status]
- ✅ claude-self-reflect-test: [status]
- ✅ reflect-tester: [status]
- ✅ Unit tests: [count] passing
- ✅ Integration tests: [count] passing

## Breaking Changes
[None/List any breaking changes]

## Documentation
- [ ] README updated
- [ ] CHANGELOG updated
- [ ] API docs updated
- [ ] Migration guide (if needed)
```

**Checklist:**
- [ ] Run automated complexity check
- [ ] Run Zen Tools code review
- [ ] Run AST-GREP quality validation
- [ ] Run precommit validation
- [ ] Run security scan
- [ ] Update CI/CD pipeline
- [ ] Create manual review checklist
- [ ] Test PR submission process

---

## Phase 5: Testing & Validation (Week 8)

### 5.1 Test with All 3 Specialized Agents

**csr-validator:** Fast MCP protocol validation
**claude-self-reflect-test:** Comprehensive E2E testing
**reflect-tester:** Installation & configuration testing

### 5.2 New Test Coverage

**Test Script:**
```bash
#!/bin/bash
# Test AST-grep → Memory Tool pipeline
test_ast_memory_pipeline() {
    python scripts/import-conversations-unified.py --file test.jsonl
    [ -f ~/.claude/memories/patterns/test/excellent-*.md ] && echo "✅ Pattern stored"
}

# Test context clearing
test_context_clearing() {
    # Simulate 30k+ token conversation
    # Verify clearing triggers and saves ≥5k tokens
}

# Test hybrid search
test_hybrid_search() {
    # Store pattern to memory
    # Search should find both memory and vector results
}
```

**Validation Checklist:**
- [ ] All 15+ existing MCP tools pass
- [ ] 3 new Memory Tool tools pass
- [ ] Context clearing triggers correctly
- [ ] AST patterns auto-store at 90+ quality
- [ ] Hybrid search works
- [ ] Statusline shows all components
- [ ] All 3 test agents pass
- [ ] Code quality gates pass
- [ ] CI/CD pipeline passes

---

## Success Metrics

### Performance Targets
- Search latency: <5ms (with Memory Tool: <3ms)
- Context clearing: 50%+ token reduction
- Memory storage: <100ms per pattern

### Quality Targets
- 90%+ of A+ patterns auto-stored
- Pattern evolution tracked 100+ sessions
- Quality scores increase 20% over 3 months

### Feature Coverage
- 15+ MCP tools: 100% functional
- 3 new Memory Tool tools: 100% functional
- Context management: Active in all sessions

---

## Timeline

**Week 1-2:** Context Management (API, preview, tracking, statusline)
**Week 3-4:** Memory Tool (handler, MCP tools, AST integration)
**Week 5-6:** Hybrid Search (enhanced search, metadata, evolution)
**Week 7:** Statusline (display, unified state)
**Week 8:** Testing (all 3 agents, new coverage, release v6.0)

---

## Code Quality Standards

### Cyclomatic Complexity Guidelines

Following the successful refactoring in PR #69 that reduced import script complexity from 49 to <10:

**Targets:**
- **Average complexity:** <5 (achieved 3.36 in PR #69)
- **Maximum per function:** <10 (strict limit)
- **Reduction goal:** 77% from baseline

**Strategy Pattern Approach (from PR #69):**
```python
# ✅ GOOD: Strategy pattern reduces complexity
class MessageProcessorFactory:
    """Factory creates appropriate processor for message type."""
    def get_processor(self, msg_type):
        return {
            'text': TextProcessor(),
            'tool_use': ToolUseProcessor(),
            'thinking': ThinkingProcessor()
        }.get(msg_type, DefaultProcessor())

# ❌ BAD: Complex if-elif chains
def process_message(msg):
    if msg.type == 'text':
        # 10 lines of logic
    elif msg.type == 'tool_use':
        # 10 lines of logic
    elif msg.type == 'thinking':
        # 10 lines of logic
    # ... complexity grows to 49
```

**Refactoring Modules (PR #69 Pattern):**
1. `context_manager.py` → Use ContextStrategy pattern for different trigger types
2. `memory_tools.py` → Separate handlers for each operation (view, create, update, delete)
3. Enhanced search → Strategy for different result ranking algorithms

**Measurement:**
```bash
# Install radon for complexity measurement
pip install radon

# Check cyclomatic complexity
radon cc mcp-server/src/context_manager.py -s

# Target output:
# M 3.36 - Average complexity (good)
# A 3.36 - All functions <10 (excellent)
```

**Validation Checklist:**
- [ ] All new modules: Average complexity <5
- [ ] No function exceeds complexity 10
- [ ] Use Strategy pattern for conditional logic
- [ ] Extract helpers for repeated patterns
- [ ] Document complex algorithms separately

## Risk Mitigation

**Cache Invalidation Costs:** Use clear_at_least=5000
**Path Traversal:** Strict validation in MemoryToolHandler
**Breaking Changes:** Comprehensive testing with all 3 agents
**Performance:** Memory Tool O(1) lookups
**Code Complexity:** Follow PR #69 patterns, measure with radon

---

## Next Steps

1. ✅ Create feature branch
2. ✅ Create comprehensive plan
3. ✅ Initialize TodoWrite checklist
4. [ ] Commit initial documentation
5. [ ] Begin Phase 1 implementation

---

**Document Version:** 1.0
**Last Updated:** 2025-01-01
**Status:** Ready for Implementation
