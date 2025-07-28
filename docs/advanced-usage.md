# Advanced Usage Guide

Power user features and detailed examples.

## Using the Reflection Agent

### Automatic Activation
The reflection agent activates automatically when you ask about past conversations:

```
"What did we discuss about database design?"
"Find our previous debugging session"  
"Have we encountered this error before?"
```

### Explicit Activation
Force the reflection agent:
```
"Use the reflection agent to search for our API discussions"
```

### Direct Tool Usage
For advanced users, you can call tools directly:

```
User: Can you check our past conversations about authentication?
Claude: I'll search through our conversation history about authentication...
[Uses reflect_on_past tool]

User: Remember that we decided to use JWT tokens for the API
Claude: I'll store this decision for future reference...
[Uses store_reflection tool]
```

## Search Strategies

### Basic Search
Simple queries work best:
```
"database optimization"
"React hooks"  
"authentication bug"
```

### Time-Scoped Search
Reference time in your query:
```
"PostgreSQL issues from last week"
"Recent discussions about API design"
"Yesterday's debugging session"
```

### Project-Specific Search (v2.4.3+)
The system automatically scopes to your current project context:
```
# Default behavior - searches current project only
"What did we discuss about authentication?"

# Explicit project targeting
"Search the ecommerce-platform project for payment processing"
"Look in claude-self-reflect for import issues"
```

### Cross-Project Search Strategies

#### When to Use Cross-Project Search
1. **Learning from Past Solutions**: "How have I handled rate limiting across all projects?"
2. **Finding Patterns**: "Show me all error handling approaches I've used"
3. **Architecture Comparisons**: "Compare authentication methods across projects"
4. **Debugging Similar Issues**: "Have I seen this error in any project?"

#### How to Trigger Cross-Project Search
```
# Natural language triggers
"Search all projects for WebSocket implementations"
"Find OAuth patterns across all my work"
"Look everywhere for database migration strategies"

# API approach
reflect_on_past(query="websocket", project="all")
```

#### Performance Considerations
- Current project search: ~50-100ms
- All projects search: ~150-250ms (scales with project count)
- Use specific queries to reduce result processing time

#### Organizing Results from Multiple Projects
When searching across projects, the reflection agent will:
1. Group results by project
2. Show project names clearly
3. Prioritize based on relevance and recency
4. Suggest project-specific deep dives if patterns emerge

## Memory Decay Control

### Per-Search Control
```javascript
// Prioritize recent conversations
await mcp.reflect_on_past({
  query: "database optimization",
  useDecay: true
});

// Search all time equally
await mcp.reflect_on_past({
  query: "foundational architecture decisions",
  useDecay: false
});
```

### Global Configuration
In your `.env` file:
```env
# Enable decay by default
ENABLE_MEMORY_DECAY=true

# Adjust decay parameters
DECAY_WEIGHT=0.3        # 30% weight on recency
DECAY_SCALE_DAYS=90     # 90-day half-life
```

## Import Strategies

### Continuous Import
Set up a watcher for new conversations:
```bash
python scripts/import-watcher.py
```

### Selective Import
Import only specific projects:
```bash
python scripts/import-single-project.py ~/.claude/projects/my-project
```

### Batch Import with Limits
For large conversation sets:
```bash
python scripts/import-conversations-unified.py --limit 1000 --batch-size 50
```

### Switching Between Embedding Modes
```bash
# Use local embeddings (default)
python scripts/import-conversations-unified.py

# Use cloud embeddings (requires Voyage key)
python scripts/import-conversations-unified.py --cloud

# Check which mode is active
python scripts/check-collections.py
```

## Testing & Validation

### Dry-Run Mode
Test without making changes:
```bash
python scripts/import-conversations-unified.py --dry-run --preview
```

### Validate Setup
Check everything is configured:
```bash
python scripts/validate-setup.py
```

Output:
```
✅ API Key         [PASS] Voyage API key is valid
✅ Qdrant          [PASS] Connected to http://localhost:6333  
✅ Claude Logs     [PASS] 24 projects, 265 files, 125.3 MB
✅ Disk Space      [PASS] 45.2 GB free
```

## Performance Optimization

### Large Datasets
For 100k+ conversations:
1. Use streaming import
2. Enable batch processing
3. Consider memory decay
4. Optimize Qdrant indices

### Search Speed
To improve search performance:
```python
# Adjust search parameters
results = await reflect_on_past(
    query="optimization",
    limit=3,           # Fewer results = faster
    min_score=0.8      # Higher threshold = fewer results
)
```

## Troubleshooting

### Common Issues

#### Import Shows 0 Messages
```bash
# Check file format
head -n 5 ~/.claude/projects/*/conversations/*.jsonl

# Validate JSON structure
python scripts/validate-jsonl.py
```

#### Search Returns Nothing
1. Check if conversations are imported:
   ```bash
   python scripts/check-collections.py
   ```
2. Lower the similarity threshold
3. Try with memory decay disabled

#### Slow Performance
1. Use streaming import for large files
2. Reduce chunk overlap
3. Batch API calls

## Integration Examples

### With Claude Projects
Combine with CLAUDE.md for best results:
- CLAUDE.md: Project rules and context
- Self-Reflect: Conversation history

### In Development Workflow
```python
# Before starting work
"What did we decide about the API structure?"

# During debugging  
"Have we seen this error before?"

# After solving something
"Remember this solution: [details]"
```

## Advanced Configuration

### Understanding Embedding Models

Claude Self-Reflect supports two embedding modes, each with distinct characteristics:

#### Local Embeddings (FastEmbed)
- **Model**: sentence-transformers/all-MiniLM-L6-v2
- **Dimensions**: 384
- **Performance**: ~100ms for 1000 embeddings
- **Storage**: ~1.5KB per conversation chunk
- **Privacy**: Complete - no data leaves your machine
- **Score Range**: Typically 0.02-0.2 (lower than cloud)

#### Cloud Embeddings (Voyage AI)
- **Model**: voyage-3-large
- **Dimensions**: 1024
- **Performance**: ~500ms for 1000 embeddings (includes network)
- **Storage**: ~4KB per conversation chunk
- **Privacy**: Conversations sent to Voyage for processing
- **Score Range**: Typically 0.05-0.3 (higher accuracy)

### Custom Embedding Models
While the two primary modes are supported, advanced users can implement:
- OpenAI embeddings (requires code modification)
- Custom FastEmbed models (change EMBEDDING_MODEL in .env)
- Alternative cloud providers (requires new embedding client)

### Alternative Vector Databases
Qdrant can be replaced with:
- Pinecone (cloud-based)
- Weaviate (GraphQL interface)
- ChromaDB (simpler, embedded)

### Custom Chunking Strategies
Modify chunk size and overlap in import scripts:
```python
CHUNK_SIZE = 500        # tokens per chunk
CHUNK_OVERLAP = 50      # overlap between chunks
```

## Best Practices

### What to Remember
✅ Debugging solutions  
✅ Architecture decisions  
✅ API design choices  
✅ Performance optimizations

### What NOT to Remember
❌ Temporary discussions  
❌ Failed approaches (unless learning)  
❌ Sensitive information  
❌ Personal data

### Search Tips
1. Use specific terms
2. Reference time when relevant
3. Start broad, then narrow
4. Check multiple phrasings

## See Also
- [Architecture Details](architecture-details.md)
- [Components Guide](components.md)  
- [Memory Decay Guide](memory-decay.md)
- [Troubleshooting Guide](troubleshooting.md)