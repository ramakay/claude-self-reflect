# MCP Tool Evaluation System Proposal

## Problem Statement
Claude Self-Reflect has 15+ MCP tools but no systematic way to measure their effectiveness. According to Anthropic's best practices, "agents are only as effective as the tools we give them," yet we lack:
- Baseline performance metrics
- Tool selection accuracy measurements
- Token efficiency tracking
- Regression detection

## Proposed Solution: Three-Layer Evaluation Architecture

### Layer 1: Unit Tests (Tool-Level)
**Purpose**: Test individual tool functionality in isolation
```python
# Test each tool can be called successfully
async def test_reflect_on_past():
    result = await mcp_call("reflect_on_past", query="test")
    assert "search" in result
    assert len(result) < 25000  # Token limit
```

### Layer 2: Integration Tests (Workflow-Level)
**Purpose**: Test common multi-tool workflows
```python
# Test workflow: Search → Get Full → Store Reflection
async def test_research_workflow():
    search_result = await mcp_call("reflect_on_past", query="docker")
    conv_id = extract_conversation_id(search_result)
    full_conv = await mcp_call("get_full_conversation", id=conv_id)
    reflection = await mcp_call("store_reflection", content="Docker insight")
    assert all([search_result, full_conv, reflection])
```

### Layer 3: Agent Evaluation (End-to-End)
**Purpose**: Test how well Claude uses the tools to solve real tasks
```python
# Real task evaluation with Claude
async def test_agent_task():
    prompt = "Find all Docker debugging sessions from last week and summarize solutions"
    response = await claude_with_tools(prompt, tools=MCP_TOOLS)

    # Verify Claude:
    # 1. Used search_by_recency (not reflect_on_past)
    # 2. Found Docker-related content
    # 3. Provided actionable summary
```

## Implementation Strategy

### Phase 1: Quick Start (Week 1)
1. **Create test harness** that connects to running MCP server
2. **Define 20 golden queries** from actual user sessions
3. **Establish baseline metrics** (current performance)

### Phase 2: Automated Testing (Week 2)
1. **GitHub Actions integration** - run on every commit
2. **Performance regression detection** - alert if >20% slower
3. **Tool coverage reporting** - which tools are never used?

### Phase 3: Agent Optimization (Week 3)
1. **Give Claude the evaluation results**
2. **Let it optimize its own tool descriptions**
3. **A/B test improvements**

## Key Metrics to Track

| Metric | Measurement | Target |
|--------|------------|--------|
| Tool Selection Accuracy | % correct tool for task | >85% |
| Response Time P95 | 95th percentile latency | <500ms |
| Token Efficiency | Tokens per task | <1000 |
| Task Success Rate | % tasks completed correctly | >80% |
| Tool Utilization | % tools used in past week | >60% |

## Evaluation Task Examples

### Strong Tasks (Multi-Tool, Real-World)
```yaml
- prompt: "What breaking changes were made to the API last month and who should we notify?"
  expected_tools: ["search_by_recency", "search_by_file", "get_full_conversation"]
  verify: ["contains:API", "contains:breaking", "has_timeline"]

- prompt: "Debug why imports are showing 0% - check all related conversations"
  expected_tools: ["search_by_concept", "get_recent_work"]
  verify: ["contains:import", "contains:metadata"]
```

### Weak Tasks (Too Simple)
```yaml
# Avoid these:
- prompt: "Search for docker"  # Too vague
- prompt: "Store this: hello"  # Not realistic
```

## Testing Infrastructure

```python
# evaluation/runner.py
class MCPEvaluator:
    def __init__(self):
        self.mcp_client = MCPClient("http://localhost:3000")
        self.claude = Anthropic()

    async def run_task(self, task):
        # 1. Reset state
        # 2. Execute task with Claude
        # 3. Collect metrics
        # 4. Verify results

    async def analyze_results(self):
        # Generate report with:
        # - Success rates by category
        # - Common failure patterns
        # - Tool confusion matrix
        # - Performance bottlenecks
```

## Success Criteria
- [ ] 20+ evaluation tasks covering all tools
- [ ] Automated daily evaluation runs
- [ ] Performance dashboard
- [ ] Agent feedback incorporated
- [ ] 20% improvement in task success rate

## Risk Mitigation
- **Import complexity**: Use subprocess to call MCP server directly
- **Non-determinism**: Use temperature=0, run 3x and take majority
- **State pollution**: Reset Qdrant test collection between runs

## Alternative Approaches Considered
1. **Mock everything**: Too far from production reality
2. **Manual testing only**: Not scalable or reproducible
3. **Only unit tests**: Misses agent-level issues

## Next Steps
1. Get consensus on this approach
2. Create minimal viable evaluator (5 tasks)
3. Establish baseline metrics
4. Iterate based on results