# Claude Self-Reflect Agent Documentation

## Overview
This document provides comprehensive documentation for all agents in the Claude Self-Reflect system.

## Agent Registry

### 1. quality-fixer
**Location**: `.claude/agents/quality-fixer.md`
**Purpose**: Automated code quality fixer that safely applies AST-GREP fixes with regression testing
**Activation Keywords**:
- "quality issues detected"
- "/fix-quality" command invoked
**Tools Access**: Read, Edit, Bash, Grep, Glob, TodoWrite
**Key Capabilities**:
- Applies AST-GREP pattern fixes automatically
- Performs regression testing before/after fixes
- Creates detailed fix reports
- Integrates with quality gate system

### 2. reflection-specialist
**Location**: `.claude/agents/reflection-specialist.md`
**Purpose**: Conversation memory expert for searching past conversations and storing insights
**Activation Keywords**:
- "find conversations about X"
- "what did we discuss about Y"
- "search previous discussions"
- "storing important findings"
**Tools Access**: All mcp__claude-self-reflect tools
**Key Capabilities**:
- Semantic search across conversation history
- Time-based search with decay
- Insight storage and retrieval
- Cross-project search capabilities

### 3. import-debugger
**Location**: `.claude/agents/import-debugger.md`
**Purpose**: Import pipeline debugging specialist for JSONL processing and conversation chunking
**Activation Keywords**:
- "import failures occur"
- "processing shows 0 messages"
- "chunking issues arise"
- "import showing 0%"
**Tools Access**: Read, Edit, Bash, Grep, Glob, LS
**Key Capabilities**:
- JSONL validation and repair
- Python script troubleshooting
- Conversation chunking analysis
- Import tracking diagnostics

### 4. search-optimizer
**Location**: `.claude/agents/search-optimizer.md`
**Purpose**: Search quality optimization expert for improving semantic search accuracy
**Activation Keywords**:
- "search results are poor"
- "relevance is low"
- "search seems irrelevant"
- "embedding models need comparison"
**Tools Access**: Read, Edit, Bash, Grep, Glob, WebFetch
**Key Capabilities**:
- Similarity threshold tuning
- Embedding performance analysis
- Search query optimization
- Model comparison and selection

### 5. qdrant-specialist
**Location**: `.claude/agents/qdrant-specialist.md`
**Purpose**: Qdrant vector database expert for collection management and troubleshooting
**Activation Keywords**:
- "Qdrant operations"
- "collection issues"
- "vector search problems"
- "Qdrant collection issues"
**Tools Access**: Read, Bash, Grep, Glob, LS, WebFetch
**Key Capabilities**:
- Collection management
- Vector search optimization
- Embedding configuration
- Performance troubleshooting

### 6. mcp-integration
**Location**: `.claude/agents/mcp-integration.md`
**Purpose**: MCP server development expert for Claude Desktop integration and tool implementation
**Activation Keywords**:
- "developing MCP tools"
- "configuring Claude Desktop"
- "debugging MCP connections"
- "MCP server issues"
**Tools Access**: Read, Edit, Bash, Grep, Glob, WebFetch
**Key Capabilities**:
- TypeScript development for MCP
- Tool implementation
- Connection debugging
- Configuration management

### 7. docker-orchestrator
**Location**: `.claude/agents/docker-orchestrator.md`
**Purpose**: Docker Compose orchestration expert for container management and service health
**Activation Keywords**:
- "Docker services fail"
- "containers restart"
- "compose configurations need debugging"
- "docker issues"
**Tools Access**: Read, Edit, Bash, Grep, LS
**Key Capabilities**:
- Container health monitoring
- Service orchestration
- Deployment troubleshooting
- Compose configuration management

### 8. claude-self-reflect-test
**Location**: `.claude/agents/claude-self-reflect-test.md`
**Purpose**: Comprehensive end-to-end testing specialist for Claude Self-Reflect system
**Activation Keywords**:
- "test installations"
- "validate configurations"
- "system integrity checks"
- "release validation"
**Tools Access**: Read, Bash, Grep, Glob, LS, Write, Edit, TodoWrite, all mcp__claude-self-reflect tools
**Key Capabilities**:
- Full system validation
- Import pipeline testing
- MCP integration testing
- Search functionality verification
- Embedding mode testing (local/cloud)

### 9. documentation-writer
**Location**: `.claude/agents/documentation-writer.md`
**Purpose**: Technical documentation specialist for creating comprehensive docs
**Activation Keywords**:
- "writing documentation"
- "creating examples"
- "explaining complex concepts"
- "API documentation"
**Tools Access**: Read, Write, Edit, MultiEdit, Grep, Glob, LS
**Key Capabilities**:
- API reference creation
- Tutorial development
- Architecture guide writing
- Example code generation

### 10. open-source-maintainer
**Location**: `.claude/agents/open-source-maintainer.md`
**Purpose**: Open source project maintainer expert for managing contributions and releases
**Activation Keywords**:
- "release management"
- "contributor coordination"
- "community building"
- "managing PR reviews"
**Tools Access**: Read, Write, Edit, Bash, Grep, Glob, LS, WebFetch
**Key Capabilities**:
- Release management
- PR review coordination
- Community engagement
- Project governance

### 11. performance-tuner
**Location**: `.claude/agents/performance-tuner.md`
**Purpose**: Performance optimization specialist for improving system efficiency
**Activation Keywords**:
- "analyzing bottlenecks"
- "optimizing queries"
- "improving system efficiency"
- "performance issues"
**Tools Access**: Read, Write, Edit, Bash, Grep, Glob, LS, WebFetch
**Key Capabilities**:
- Bottleneck analysis
- Query optimization
- Memory usage reduction
- System scaling

### 12. reflect-tester
**Location**: `.claude/agents/reflect-tester.md`
**Purpose**: Comprehensive testing specialist for validating reflection system functionality
**Activation Keywords**:
- "testing installations"
- "validating configurations"
- "troubleshooting system issues"
**Tools Access**: Read, Bash, Grep, LS, WebFetch, ListMcpResourcesTool, mcp__claude-self-reflect tools
**Key Capabilities**:
- Installation validation
- Configuration testing
- System troubleshooting
- MCP integration verification

## Agent Invocation Patterns

### Proactive Activation
Agents marked with "Use PROACTIVELY" in their descriptions should be invoked automatically when their trigger conditions are met, without waiting for explicit user request.

### Manual Invocation
Use the Task tool with the appropriate subagent_type parameter:
```python
Task(
    description="Short task description",
    subagent_type="agent-name",
    prompt="Detailed task instructions"
)
```

### Parallel Execution
Multiple agents can be invoked concurrently:
```python
# Single message with multiple Task invocations
[
    Task(subagent_type="search-optimizer", ...),
    Task(subagent_type="qdrant-specialist", ...)
]
```

## Agent Communication Patterns

### 1. Context Preservation
- Agents run in isolated contexts
- Use continuation_id for multi-turn conversations
- Results are returned as focused summaries

### 2. Result Synthesis
- Main Claude instance synthesizes agent outputs
- Cross-agent validation possible through sequential calls
- Shared state through file system or database

### 3. Error Handling
- Agents return error states clearly
- Fallback strategies defined per agent
- Main instance handles recovery

**Example Error Recovery**:
```python
# Search optimization with fallback
try:
    result = Task(
        subagent_type="search-optimizer",
        prompt="Optimize search for: docker issues"
    )
except AgentExecutionError:
    # Fallback to basic search without optimization
    result = mcp__claude-self-reflect__reflect_on_past(
        "docker issues",
        min_score=0.2  # Lower threshold for wider results
    )
```

## Best Practices

### 1. Agent Selection
- Match agent to task complexity
- Use specialized agents for domain-specific tasks
- Consider parallel execution for independent tasks

### 2. Prompt Engineering
- Provide clear, detailed instructions
- Include relevant context and constraints
- Specify expected output format

### 3. Resource Management
- Monitor agent execution time
- Use appropriate timeout values
- Clean up temporary resources

## Integration with Quality Systems

### AST-GREP Integration
- quality-fixer agent uses AST-GREP patterns
- Patterns loaded from unified_registry.json
- Automatic fix application with validation

### Hook Integration
- Agents respect system hooks
- Pre-commit validation through quality gates
- Post-generation cleanup and formatting

## Monitoring and Debugging

### 1. Agent Logs
- Check agent-specific log files
- Monitor execution metrics
- Track success/failure rates

### 2. Debug Mode
- Enable verbose output for troubleshooting
- Capture intermediate states
- Validate agent assumptions

### 3. Performance Metrics
- Execution time per agent
- Resource consumption
- Success rate tracking

## Version History
- v1.0: Initial agent documentation
- v1.1: Added quality-fixer and AST-GREP integration
- v1.2: Enhanced with hook system documentation