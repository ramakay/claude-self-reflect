# API Reference

This document describes the MCP (Model Context Protocol) tools available in Claude Self-Reflect.

## Overview

Claude Self-Reflect provides two main MCP tools that enable Claude to search past conversations and store insights for future reference. These tools are automatically available in Claude Code after installation.

## Available Tools

### `reflect_on_past`

Search for relevant past conversations using semantic search with optional time-based memory decay.

#### Parameters

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `query` | string | required | The search query to find semantically similar conversations |
| `limit` | integer | 5 | Maximum number of results to return |
| `min_score` | float | 0.7 | Minimum similarity score (0-1) |
| `project` | string/null | null | Search specific project only. If not provided, searches current project. Use 'all' to search across all projects |
| `brief` | boolean | false | Brief mode: returns minimal information for faster response |
| `include_raw` | boolean | false | Include raw Qdrant payload data for debugging |
| `response_format` | string | 'xml' | Response format: 'xml' or 'markdown' |
| `use_decay` | integer/string | -1 | Apply time-based decay: 1=enable, 0=disable, -1=use environment default |

#### Usage Examples

```javascript
// Basic search
mcp__claude-self-reflect__reflect_on_past({
  query: "debugging React hooks"
})

// Search with more results
mcp__claude-self-reflect__reflect_on_past({
  query: "database optimization",
  limit: 10
})

// Search with higher similarity threshold
mcp__claude-self-reflect__reflect_on_past({
  query: "authentication implementation",
  min_score: 0.8
})

// Force enable memory decay
mcp__claude-self-reflect__reflect_on_past({
  query: "recent bugs",
  use_decay: 1
})

// Search across all projects
mcp__claude-self-reflect__reflect_on_past({
  query: "Docker configurations",
  project: "all"
})

// Brief mode for faster response
mcp__claude-self-reflect__reflect_on_past({
  query: "authentication setup",
  brief: true
})

// Disable memory decay for historical search
mcp__claude-self-reflect__reflect_on_past({
  query: "project architecture decisions",
  use_decay: 0
})
```

#### Return Value

Returns a JSON string containing an array of search results. Each result includes:

```json
{
  "id": "unique_point_id",
  "score": 0.85,
  "timestamp": "2024-01-15T10:30:00Z",
  "role": "assistant",
  "excerpt": "The conversation content...",
  "project_name": "my-project",
  "conversation_id": "abc123",
  "collection_name": "conv_12345678_local"  // or _voyage for cloud mode
}
```

### `store_reflection`

Store an important insight or reflection for future reference.

**Note**: This tool is currently in development. It acknowledges storage but doesn't persist data yet.

#### Parameters

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `content` | string | required | The insight or reflection to store |
| `tags` | array[string] | [] | Tags to categorize this reflection |

#### Usage Examples

```javascript
// Store a simple reflection
mcp__claude-self-reflect__store_reflection({
  content: "Using React.memo significantly improved render performance in the dashboard"
})

// Store with tags
mcp__claude-self-reflect__store_reflection({
  content: "JWT tokens with 15-minute expiry provide good security/UX balance",
  tags: ["authentication", "security", "jwt"]
})

// Store debugging solution
mcp__claude-self-reflect__store_reflection({
  content: "Fixed memory leak by clearing event listeners in useEffect cleanup",
  tags: ["bug-fix", "react", "memory-leak"]
})
```

#### Return Value

Returns a confirmation message:
```
"Reflection stored successfully with tags: authentication, security, jwt"
```

### `search_by_file` (New in v2.5.6)

Find conversations that analyzed, modified, or referenced specific files. This tool is particularly useful for finding conversations where git operations were performed on specific files.

#### Parameters

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `file_path` | string | required | The file path to search for in conversations |
| `limit` | integer | 10 | Maximum number of results to return |
| `project` | string/null | null | Search specific project only. Use 'all' to search across all projects |

#### Usage Examples

```javascript
// Find conversations about a specific file
mcp__claude-self-reflect__search_by_file({
  file_path: "src/components/Header.tsx"
})

// Search for discussions about a config file
mcp__claude-self-reflect__search_by_file({
  file_path: "package.json",
  limit: 5
})

// Find git operations on a specific file across all projects
mcp__claude-self-reflect__search_by_file({
  file_path: "README.md",
  project: "all"
})
```

#### Return Value

Returns XML with structured results showing conversations that mentioned the file:

```xml
<search_by_file>
  <query>src/components/Header.tsx</query>
  <normalized_path>src/components/header.tsx</normalized_path>
  <count>3</count>
  <results>
    <result>
      <timestamp>2024-01-15T14:30:00Z</timestamp>
      <project>my-app</project>
      <action>Modified Header component styling</action>
      <tools_used>Edit, Read</tools_used>
      <preview>Updated the Header component to fix responsive...</preview>
    </result>
  </results>
</search_by_file>
```

### `search_by_concept` (New in v2.5.6)

Search conversations by conceptual themes, topics, or technologies. Uses semantic analysis of tool outputs and conversation content.

#### Parameters

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `concept` | string | required | The concept to search for (e.g., 'security', 'docker', 'testing') |
| `include_files` | boolean | true | Include file information in results |
| `limit` | integer | 10 | Maximum number of results to return |
| `project` | string/null | null | Search specific project only. Use 'all' to search across all projects |

#### Usage Examples

```javascript
// Find security-related conversations
mcp__claude-self-reflect__search_by_concept({
  concept: "security"
})

// Search for Docker discussions without file details
mcp__claude-self-reflect__search_by_concept({
  concept: "docker",
  include_files: false
})

// Find testing conversations across all projects
mcp__claude-self-reflect__search_by_concept({
  concept: "testing",
  project: "all",
  limit: 15
})
```

#### Return Value

Returns XML with concept-based search results:

```xml
<search_by_concept>
  <concept>security</concept>
  <count>5</count>
  <results>
    <result>
      <score>0.89</score>
      <timestamp>2024-01-15T10:00:00Z</timestamp>
      <project>auth-service</project>
      <concepts>security, authentication, jwt</concepts>
      <related_concepts>authorization, tokens, middleware</related_concepts>
      <files>src/auth/middleware.ts, config/security.json</files>
      <preview>Implemented JWT authentication middleware with...</preview>
    </result>
  </results>
</search_by_concept>
```

## Configuration

The behavior of these tools can be configured through environment variables:

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `VOYAGE_KEY` | optional | Your Voyage AI API key (only for cloud mode) |
| `PREFER_LOCAL_EMBEDDINGS` | true | Use local embeddings by default (no API needed) |
| `QDRANT_URL` | http://localhost:6333 | URL of your Qdrant instance |
| `ENABLE_MEMORY_DECAY` | false | Enable time-based memory decay globally |
| `DECAY_WEIGHT` | 0.3 | Weight of decay factor in scoring (0-1) |
| `DECAY_SCALE_DAYS` | 90 | Half-life for memory decay in days |
| `USE_NATIVE_DECAY` | false | Use Qdrant's native decay (experimental) |

### Setting Environment Variables

1. **In `.env` file** (recommended):
   ```bash
   VOYAGE_KEY=your-api-key-here
   QDRANT_URL=http://localhost:6333
   ENABLE_MEMORY_DECAY=true
   DECAY_SCALE_DAYS=60
   ```

2. **When adding MCP to Claude**:
   ```bash
   claude mcp add claude-self-reflect "/path/to/run-mcp.sh" \
     -e VOYAGE_KEY="your-key" \
     -e ENABLE_MEMORY_DECAY="true"
   ```

## Memory Decay

Memory decay helps prioritize recent conversations over older ones, mimicking human memory patterns.

### How It Works

When enabled, the search scoring combines:
- **Semantic similarity**: How well the content matches your query
- **Time decay**: How recent the conversation is

The formula: `adjusted_score = similarity_score + (decay_weight * e^(-age/scale))`

### Decay Examples

With default settings (90-day half-life, 0.3 weight):
- Today's conversation: +0.30 boost
- 1 week old: +0.28 boost
- 1 month old: +0.22 boost
- 3 months old: +0.15 boost
- 6 months old: +0.08 boost
- 1 year old: +0.03 boost

### When to Use Decay

**Enable decay for**:
- Finding recent bug fixes
- Current project context
- Latest implementation details
- Recent decisions

**Disable decay for**:
- Historical architecture decisions
- Long-term project knowledge
- Reference implementations
- Best practices documentation

## Error Handling

Common errors and their meanings:

### API Errors

| Error | Cause | Solution |
|-------|-------|----------|
| "Voyage AI API key not configured" | Missing API key (cloud mode) | Set VOYAGE_KEY environment variable or use local mode |
| "Collection not found" | No conversations imported | Run import script first |
| "Rate limit exceeded" | Too many API calls | Wait and retry with smaller batches |

### Connection Errors

| Error | Cause | Solution |
|-------|-------|----------|
| "Connection refused" | Qdrant not running | Start Qdrant with Docker |
| "Timeout" | Slow network/large query | Increase timeout or reduce query size |

## Best Practices

1. **Query Construction**
   - Be specific but not overly narrow
   - Include relevant technical terms
   - Use natural language

2. **Score Thresholds**
   - 0.7-0.8: Good for general searches
   - 0.8-0.9: For precise matches
   - <0.7: When exploring broadly

3. **Result Limits**
   - 5-10: For focused searches
   - 10-20: For exploration
   - >20: Rarely needed, may be slow

4. **Performance Tips**
   - Cache frequently used queries
   - Use appropriate score thresholds
   - Limit search scope when possible

## Integration Examples

### In Claude Code

When Claude receives a question about past work:

```
User: "What was that PostgreSQL optimization we did?"

Claude uses: reflect_on_past({
  query: "PostgreSQL optimization performance",
  limit: 5
})

Claude: "I found our conversation from December 15th where we discovered 
that adding a GIN index on the metadata JSONB column reduced query time 
from 2.3s to 45ms."
```

### Automated Workflows

Future possibilities (not yet implemented):
- Auto-store insights after problem solving
- Weekly summaries of key decisions
- Project knowledge extraction

## Limitations

1. **Storage**: `store_reflection` not yet persistent
2. **Performance**: Large conversation histories may be slow
3. **Context**: Limited to conversation text content and tool outputs
4. **File Search**: Only finds files mentioned in tool outputs (git operations, edits, reads)

## Future Enhancements

Planned improvements:
- ✅ ~~Project-specific search filtering~~ (Available since v2.4.3)
- ✅ ~~File-based search~~ (Available since v2.5.6)
- ✅ ~~Concept-based search~~ (Available since v2.5.6)
- Persistent reflection storage (in development)
- Code snippet extraction with syntax highlighting
- Conversation threading and follow-ups
- Multi-modal search (images, code)
- Export capabilities (CSV, JSON)

For the latest updates, see the [GitHub repository](https://github.com/ramakay/claude-self-reflect).