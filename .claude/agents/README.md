# Claude Self Reflect - Specialized Agents

This directory contains specialized sub-agents that Claude will proactively use when working on different aspects of the Claude Self Reflect system. Each agent has focused expertise and will automatically activate when their domain is encountered.

## Available Agents

### 🔧 Core System Agents

1. **[qdrant-specialist](./qdrant-specialist.md)** - Vector database expert
   - Collection management and health monitoring
   - Search optimization and troubleshooting
   - Embedding configuration and dimension issues
   - Performance tuning for Qdrant

2. **[import-debugger](./import-debugger.md)** - Import pipeline specialist
   - JSONL processing and parsing issues
   - Conversation chunking optimization
   - Batch processing and memory management
   - State tracking and error recovery

3. **[docker-orchestrator](./docker-orchestrator.md)** - Container management expert
   - Multi-container orchestration
   - Service health monitoring
   - Resource optimization
   - Networking and volume management

4. **[mcp-integration](./mcp-integration.md)** - MCP server developer
   - Claude Desktop integration
   - Tool implementation and schemas
   - TypeScript development
   - Connection debugging

5. **[search-optimizer](./search-optimizer.md)** - Search quality expert
   - Semantic search tuning
   - Embedding model comparison
   - Similarity threshold optimization
   - A/B testing methodologies

6. **[reflection-specialist](./reflection-specialist.md)** - Conversation memory expert
   - Searching past conversations with MCP tools
   - Storing insights and reflections
   - Maintaining knowledge continuity
   - Cross-project conversation search

### 🌟 Open Source Development Agents

7. **[open-source-maintainer](./open-source-maintainer.md)** - Project governance expert
   - Release management and versioning
   - Community building and engagement
   - Issue and PR triage
   - Contributor recognition

8. **[documentation-writer](./documentation-writer.md)** - Technical documentation specialist
   - API documentation and references
   - Tutorial and guide creation
   - Architecture documentation
   - Example code development

9. **[performance-tuner](./performance-tuner.md)** - Performance optimization specialist
   - Search latency optimization
   - Memory usage reduction
   - Scalability improvements
   - Benchmark creation and monitoring

### 🧪 Testing and Validation Agents

10. **[reflect-tester](./reflect-tester.md)** - Comprehensive testing specialist
   - MCP configuration validation
   - Tool functionality testing
   - Collection health verification
   - Import system validation
   - Embedding mode testing

## How Agents Work

### Automatic Activation

Claude automatically engages the appropriate agent based on context. For example:

- Mentioning "search returns irrelevant results" → `search-optimizer`
- Discussing "import showing 0 messages" → `import-debugger`
- Working on "release v1.2.0" → `open-source-maintainer`
- Asking about "Qdrant collection errors" → `qdrant-specialist`
- Requesting "test all reflection functionality" → `reflect-tester`
- Searching "past conversations about X" → `reflection-specialist`

### Agent Capabilities

Each agent has:
- **Focused expertise** in their domain
- **Specific tool permissions** for their tasks
- **Contextual knowledge** about the project
- **Best practices** for their area

### Working with Multiple Agents

Agents can collaborate on complex issues:

```
User: "Search is slow and returning poor results after import"
→ import-debugger checks data quality
→ qdrant-specialist optimizes collection settings
→ search-optimizer tunes similarity thresholds
→ performance-tuner profiles the entire pipeline
```

## Creating New Agents

To add a new specialized agent:

1. Create a new `.md` file in this directory
2. Use the following template:

```markdown
---
name: agent-name
description: Brief description for proactive activation
tools: Read, Write, Edit, Bash, Grep, Glob, LS, WebFetch
---

You are a [role] for the Claude Self Reflect project. Your expertise covers [domains].

## Project Context
[Specific project knowledge relevant to this agent]

## Key Responsibilities
[Numbered list of main tasks]

## Essential Commands/Patterns
[Code blocks with common operations]

## Best Practices
[Domain-specific guidelines]
```

3. Update this README with the new agent
4. Test the agent activation with relevant prompts

## Agent Development Guidelines

- **Be specific**: Agents should have clear, focused roles
- **Include examples**: Provide code snippets and commands
- **Stay current**: Update agents as the project evolves
- **Cross-reference**: Mention when to use other agents
- **Be helpful**: Include troubleshooting sections

## Maintenance

Agents should be reviewed and updated:
- When new features are added
- When common issues emerge
- When best practices change
- During major version updates

Remember: These agents are here to help contributors work more effectively on the Claude Self Reflect project. They embody the project's expertise and best practices.