# Components Deep Dive

Detailed exploration of each system component.

## 1. Qdrant Vector Database

### Overview
Qdrant is a vector similarity search engine designed for production use. We chose it because:
- Runs locally (no cloud dependency)
- Fast similarity search
- Built-in persistence
- Good Python/JavaScript clients

### Configuration
```yaml
# docker-compose.yaml
volumes:
  qdrant_data:

services:
  qdrant:
    image: qdrant/qdrant:latest
    ports:
      - "6333:6333"
    volumes:
      - qdrant_data:/qdrant/storage
```

### Data Model
Each conversation chunk is stored as:
```json
{
  "id": "uuid",
  "vector": [1024 dimensions],
  "payload": {
    "content": "conversation text",
    "project": "project-name",
    "timestamp": "2024-01-26T10:30:00Z",
    "file_path": "path/to/conversation.jsonl",
    "chunk_index": 0,
    "total_chunks": 5
  }
}
```

### Collections
- One collection per project:
  - Local embeddings: `conv_<md5_hash>_local`
  - Cloud embeddings: `conv_<md5_hash>_voyage`
- Isolation prevents cross-project information leakage
- MD5 hash ensures valid collection names

## 2. MCP Server for Conversation Memory

### Architecture
Built with FastMCP, a Python framework for Model Context Protocol servers.

### Tools Provided

#### `store_reflection`
Stores important insights for future reference.
```python
async def store_reflection(content: str, tags: List[str] = None) -> str:
    # Generate embedding
    # Store in Qdrant with current timestamp
    # Return confirmation
```

#### `reflect_on_past`
Searches conversation history semantically.
```python
async def reflect_on_past(
    query: str, 
    limit: int = 5,
    min_score: float = 0.7,
    use_decay: bool = None
) -> List[Dict]:
    # Convert query to embedding
    # Search across all collections
    # Apply memory decay if enabled
    # Return relevant chunks
```

### Configuration
Environment variables:
- `VOYAGE_KEY`: API key for cloud embeddings (optional)
- `QDRANT_URL`: Database URL (default: http://localhost:6333)
- `PREFER_LOCAL_EMBEDDINGS`: Use local embeddings (default: true)
- `ENABLE_MEMORY_DECAY`: Enable time-based decay (default: false)
- `DECAY_WEIGHT`: How much recency matters (default: 0.3)
- `DECAY_SCALE_DAYS`: Half-life for decay (default: 90)

## 3. Python Importer

### Import Pipeline
1. **Discovery**: Find all conversation JSONL files
2. **Parsing**: Extract messages from each file
3. **Chunking**: Split into 500-token chunks
4. **Embedding**: Generate vectors:
   - Local mode: FastEmbed (default)
   - Cloud mode: Voyage AI
5. **Storage**: Save to Qdrant with metadata

### Import Scripts

#### `import-conversations-voyage.py`
Standard import for all conversations.
```bash
python scripts/import-conversations-voyage.py
```

#### `import-conversations-voyage-streaming.py`
Memory-efficient streaming import for large datasets.
```bash
python scripts/import-conversations-voyage-streaming.py --limit 100
```

#### `import-single-project.py`
Import specific project only.
```bash
python scripts/import-single-project.py ~/.claude/projects/my-project
```

### Chunking Strategy
- **Size**: 500 tokens per chunk
- **Overlap**: 50 tokens between chunks
- **Context**: Preserves conversation flow
- **Metadata**: Each chunk knows its position

### Embedding Generation
```python
def generate_embedding(text: str) -> List[float]:
    response = voyage_client.embed(
        texts=[text],
        model="voyage-3-large",
        input_type="document"
    )
    return response.embeddings[0]
```

## 4. Claude Sub-Agents

Specialized agents in `.claude/agents/` for different tasks:

### Available Agents
1. **qdrant-specialist**: Database operations and troubleshooting
2. **import-debugger**: Import pipeline issues
3. **docker-orchestrator**: Container management
4. **mcp-integration**: MCP server development
5. **search-optimizer**: Search quality tuning
6. **reflection-specialist**: Conversation memory usage

### Agent Activation
Agents activate automatically based on context:
```
User: "The import is showing 0 messages"
Claude: [Activates import-debugger agent]
```

## 5. Configuration System

### File Structure
```
config/
├── imported-files.json    # Tracks imported conversations
└── .env                   # Environment variables
```

### Imported Files Tracking
```json
{
  "files": {
    "/path/to/conversation.jsonl": {
      "imported_at": "2024-01-26T10:30:00Z",
      "chunks": 15,
      "collection": "conv_abc123_voyage"
    }
  }
}
```

## Performance Characteristics

### Search Performance
- **Latency**: ~100ms for cross-collection search
- **Throughput**: 100+ searches/second
- **Accuracy**: 66.1% relevance (with decay: 85%+)

### Import Performance
- **Speed**: ~1000 conversations/minute
- **Memory**: Streaming mode uses <100MB RAM
- **CPU**: Single-threaded, CPU-bound on embedding

### Storage Requirements
- **Vectors**: ~4KB per conversation chunk
- **Metadata**: ~1KB per chunk
- **Total**: ~5GB per 1M chunks

## Troubleshooting Common Issues

### Import Failures
- Check VOYAGE_KEY is set
- Verify Qdrant is running
- Look for malformed JSONL files

### Search Not Finding Results
- Ensure conversations are imported
- Check minimum score threshold
- Try enabling memory decay

### Performance Issues
- Use streaming import for large datasets
- Index optimization in Qdrant
- Consider batch processing

## See Also
- [Architecture Overview](architecture-details.md)
- [Memory Decay Guide](memory-decay.md)
- [Troubleshooting Guide](troubleshooting.md)