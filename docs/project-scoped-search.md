# Project-Scoped Search Guide

## Overview

Starting with v2.4.3, Claude Self-Reflect implements project-scoped search by default. This means conversations are automatically filtered to your current working directory's project, providing more focused and relevant results.

**Breaking Change**: This is a significant behavioral change from previous versions where all projects were searched by default.

## How Project Detection Works

The system automatically detects your current project by analyzing your working directory path:

```python
# Example path analysis
/Users/you/projects/ShopifyMCPMockShop/src/components
                    └─ Detected project: ShopifyMCPMockShop

/home/developer/work/projects/api-gateway/tests
                              └─ Detected project: api-gateway
```

### Detection Logic

1. The system looks for a `projects` directory in your path
2. The folder immediately after `projects` is considered your project name
3. If no `projects` folder exists, it checks for `.claude` directories
4. Falls back to searching all projects if detection fails

## Search Modes

### 1. Current Project Search (Default)

When you ask about past conversations without specifying scope:

```javascript
// API usage
reflect_on_past({
  query: "database optimization",
  limit: 5
})
// Searches only current project
```

**Natural Language Examples:**
- "What did we discuss about authentication?"
- "Find our debugging session from last week"
- "Show me the caching implementation"

### 2. Cross-Project Search

To search across all your projects:

```javascript
// API usage
reflect_on_past({
  query: "websocket implementations",
  project: "all",
  limit: 10
})
```

**Natural Language Examples:**
- "Search all projects for rate limiting"
- "Find websocket implementations across projects"
- "Look everywhere for OAuth examples"

### 3. Specific Project Search

To search a particular project by name:

```javascript
// API usage
reflect_on_past({
  query: "Docker configuration",
  project: "claude-self-reflect",
  limit: 5
})
```

**Natural Language Examples:**
- "Find Docker setup in claude-self-reflect project"
- "Search MyApp project for user authentication"
- "Look in the API project for error handling"

## Migration Guide for Pre-v2.4.3 Users

### What's Changed

| Aspect | Before v2.4.3 | After v2.4.3 |
|--------|---------------|--------------|
| Default Scope | All projects | Current project only |
| Result Count | Often overwhelming | Focused and relevant |
| Performance | Slower (searching all) | Faster (single collection) |
| Privacy | All projects mixed | Projects isolated by default |

### Adapting Your Workflow

1. **Getting Old Behavior Back**
   Simply add "all projects" to your queries:
   - Old: "Find authentication patterns"
   - New: "Find authentication patterns across all projects"

2. **Leveraging Project Isolation**
   - Work conversations stay separate from personal projects
   - Client projects remain isolated from each other
   - Faster searches with more relevant results

3. **Common Adjustments**
   - If you frequently need cross-project search, mention it explicitly
   - Use specific project names when you know where to look
   - Let the default behavior help you focus on current work

## Performance Implications

### Search Performance

| Search Type | Typical Latency | Collections Queried |
|-------------|----------------|-------------------|
| Current Project | ~50-100ms | 1 |
| Specific Project | ~50-100ms | 1 |
| All Projects | ~150-250ms | 24+ (depends on projects) |

### Memory Usage

- Single project searches use less memory
- Cross-project searches may load multiple embedding indices
- Qdrant handles this efficiently with lazy loading

## Advanced Usage

### Project Name Edge Cases

The system handles various project structures:

```bash
# Standard structure
/Users/you/projects/my-app → Project: "my-app"

# Nested projects
/Users/you/projects/client/web-app → Project: "client"

# Non-standard paths
/Users/you/code/my-project → Falls back to all projects
```

### Programmatic Project Detection

To check which project will be searched:

```python
# In your Python environment
import os
from pathlib import Path

cwd = os.getcwd()
path_parts = Path(cwd).parts

if 'projects' in path_parts:
    idx = path_parts.index('projects')
    if idx + 1 < len(path_parts):
        project = path_parts[idx + 1]
        print(f"Current project: {project}")
```

### Collection Naming Convention

Projects are stored in Qdrant with hashed names:

```
conv_<md5_hash>_local    # Local embeddings (FastEmbed)
conv_<md5_hash>_voyage   # Cloud embeddings (Voyage AI)
```

The hash is derived from the project path for consistent identification.

## Best Practices

### 1. Understanding How Claude Organizes Your Conversations

Claude automatically stores conversations based on your working directory when you start a conversation. The system detects your project from your current path:

```
# When you start Claude from these directories:
~/projects/my-app         → Conversations stored under "my-app"
~/projects/client/website → Conversations stored under "client"
~/work/api-gateway       → Falls back to searching all projects
```

To ensure proper project isolation:
- Always start Claude from your project's root directory
- Be consistent about where you launch Claude for each project
- Check your current directory with `pwd` before starting important conversations

### 2. Use Natural Language Effectively

The reflection-specialist agent understands context:

- **Implicit current project**: "What was that SQL optimization?"
- **Explicit cross-project**: "Have I solved this error in any project?"
- **Specific targeting**: "Check the blog project for markdown parsing"

### 3. Leverage Project Isolation

- Work conversations stay separate from personal projects automatically
- Client projects remain isolated from each other
- Sensitive discussions don't leak into unrelated searches

### 4. Performance Optimization

- Default to project search for speed
- Only use cross-project when necessary
- Consider time decay settings for large projects

## Troubleshooting

### Issue: "No results found" but you know the conversation exists

**Possible Causes:**
1. Conversation is in a different project
2. Project detection failed
3. Score threshold too high

**Solutions:**
```bash
# Try cross-project search
"Search all projects for [your query]"

# Check detected project
pwd  # Verify you're in the right directory

# Lower score threshold
"Find [query] with lower threshold"
```

### Issue: Getting results from wrong project

**Possible Cause:** Project detection might be confused by path structure

**Solution:** Use explicit project name:
```
"Search specifically in ProjectName for [query]"
```

### Issue: Slow search performance

**Possible Causes:**
1. Searching all projects unnecessarily
2. Large number of collections
3. Complex query with low score threshold

**Solutions:**
- Use project-scoped search when possible
- Increase min_score to reduce result set
- Be more specific in queries

## Security and Privacy Considerations

### Project Isolation Benefits

1. **Client Confidentiality**: Each client's conversations stay within their project
2. **Personal/Work Separation**: Personal projects don't mix with work
3. **Compliance**: Easier to manage data retention per project
4. **Access Control**: Future versions could implement per-project permissions

### Data Handling

- Each project's embeddings are stored separately
- No cross-contamination between project vector spaces
- Deletion of a project's collection removes all its data

## Future Enhancements

Based on user feedback, we're considering:

1. **Project Aliases**: Custom names for projects
2. **Import Configuration**: Exclude projects from indexing entirely
3. **Project Groups**: Search related projects together
4. **Search History**: Per-project search analytics
5. **Project Templates**: Predefined search patterns per project type

## API Reference

### reflect_on_past Parameters

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| query | string | required | Search query text |
| limit | int | 5 | Maximum results to return |
| min_score | float | 0.7 | Minimum similarity score |
| use_decay | int/string | -1 | Time decay: 1=on, 0=off, -1=default |
| project | string | None | Project scope (None=current, "all"=all, "name"=specific) |

### Examples

```python
# Current project search
{
  "query": "authentication flow",
  "limit": 5,
  "min_score": 0.05
}

# All projects with decay
{
  "query": "error handling patterns",
  "project": "all",
  "use_decay": 1,
  "limit": 10
}

# Specific project without decay
{
  "query": "database schema",
  "project": "ecommerce-platform",
  "use_decay": 0,
  "min_score": 0.0
}
```

## Conclusion

Project-scoped search represents a significant improvement in how Claude Self-Reflect handles conversation memory. By defaulting to project-specific searches, we provide:

- More relevant results
- Better performance
- Natural project isolation
- Flexibility when needed

The breaking change is intentional and designed to match how developers naturally think about their work - in project contexts. We believe this change, while requiring some adjustment, significantly improves the daily experience of using Claude Self-Reflect.

For questions or feedback:
- Join the discussion: [Project-Scoped Search Feedback](https://github.com/ramakay/claude-self-reflect/discussions/17)
- Report bugs: [GitHub Issues](https://github.com/ramakay/claude-self-reflect/issues)