# Streaming Importer Architecture

## Overview

The streaming importer is a high-performance, memory-efficient solution for importing Claude conversations into Qdrant vector database. It achieves stable memory usage of ~260MB while processing hundreds of files, compared to the previous implementation that suffered from memory leaks reaching 3.8GB+.

## Key Features

- **Stable Memory**: Maintains ~260MB memory usage regardless of file count
- **True Async Architecture**: Built from ground up with async/await patterns
- **Thread Pool Isolation**: FastEmbed runs in isolated thread pool executor
- **100% Import Coverage**: Successfully processes all conversation files
- **Dual Embedding Support**: Works with both local (FastEmbed) and cloud (Voyage AI) embeddings
- **State Management**: Maintains compatibility with both legacy and new state formats

## Architecture

### Core Components

```
StreamingImporter
├── EmbeddingProvider (Interface)
│   ├── FastEmbedProvider (Thread Pool Executor)
│   └── VoyageEmbedProvider (API Based)
├── QdrantService (Async Client)
│   ├── Collection Management
│   └── Batch Storage
└── State Manager
    ├── Legacy Format Support
    └── Stream Position Tracking
```

### Memory Management Strategy

The solution to the memory leak was inspired by the Qdrant MCP server implementation:

1. **Thread Pool Executor**: FastEmbed operations run in `asyncio.run_in_executor()`
2. **Memory Isolation**: Native ONNX Runtime memory is isolated in threads
3. **Periodic Cleanup**: `gc.collect()` and `malloc_trim()` after each file
4. **Batch Processing**: Embeddings processed in configurable chunks

### Key Innovation: Thread Pool Pattern

```python
async def embed_documents(self, documents: List[str]) -> List[List[float]]:
    loop = asyncio.get_event_loop()
    embeddings = await loop.run_in_executor(
        None, lambda: list(self.embedding_model.passage_embed(documents))
    )
    return [embedding.tolist() for embedding in embeddings]
```

This pattern prevents ONNX Runtime's memory pooling from accumulating in the main process.

## Configuration

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `QDRANT_URL` | http://localhost:6333 | Qdrant server URL |
| `VOYAGE_API_KEY` | (empty) | Optional Voyage AI API key |
| `PREFER_LOCAL_EMBEDDINGS` | true | Use local FastEmbed by default |
| `EMBEDDING_MODEL` | sentence-transformers/all-MiniLM-L6-v2 | FastEmbed model |
| `OPERATIONAL_MEMORY_MB` | 1500 | Memory threshold for processing |
| `MALLOC_ARENA_MAX` | 2 | Limit glibc memory arenas |
| `CHUNK_SIZE` | 5 | Messages per embedding chunk |
| `THREAD_POOL_WORKERS` | 2 | Thread pool size |

### Docker Deployment

```yaml
async-importer:
  build:
    context: .
    dockerfile: Dockerfile.async-importer
  container_name: claude-reflection-async
  profiles: ["async"]
  environment:
    - QDRANT_URL=http://qdrant:6333
    - OPERATIONAL_MEMORY_MB=1500
    - MALLOC_ARENA_MAX=2
  mem_limit: 4g
  restart: unless-stopped
```

## State File Format

The importer maintains dual format compatibility for smooth transitions:

```json
{
  "imported_files": {
    "/path/to/file.jsonl": {
      "last_modified": 1234567890.0,
      "last_imported": 1234567891.0,
      "chunks": 42
    }
  },
  "stream_position": {
    "imported_files": ["/path/to/file.jsonl"],
    "file_metadata": {
      "/path/to/file.jsonl": {
        "position": 42,
        "last_modified": 1234567890.0
      }
    }
  }
}
```

## Performance Metrics

### Memory Usage
- **Baseline**: 120MB (FastEmbed model loaded)
- **Operational**: 260MB (during active processing)
- **Peak**: <300MB (with large batches)
- **Stability**: No growth over time

### Processing Speed
- **Rate**: ~570 chunks/minute
- **Files**: 697 files in ~45 minutes
- **Chunks**: 25,000+ chunks processed
- **Reliability**: Zero container restarts

## File Processing Flow

1. **Discovery**: Scan project directories for JSONL files
2. **Prioritization**: HOT (recent) → WARM (pending) → COLD (old)
3. **Chunking**: Split conversations into embedding-sized chunks
4. **Embedding**: Process through thread pool executor
5. **Storage**: Batch upload to Qdrant
6. **State Update**: Persist progress for resumability

## Monitoring

### Health Checks
```bash
# Check memory usage
docker stats claude-reflection-async

# View logs
docker logs claude-reflection-async --tail 50

# Check processing status
docker logs claude-reflection-async 2>&1 | grep "Overall:"
```

### Key Metrics to Monitor
- Memory usage (should stay <300MB)
- Files processed count
- Chunks per minute rate
- Error messages in logs

## Troubleshooting

### Common Issues

1. **High Memory Usage**
   - Check `OPERATIONAL_MEMORY_MB` setting
   - Verify `MALLOC_ARENA_MAX=2` is set
   - Reduce `CHUNK_SIZE` if needed

2. **Slow Processing**
   - Increase `THREAD_POOL_WORKERS` (carefully)
   - Check network latency to Qdrant
   - Verify CPU isn't throttled

3. **State File Issues**
   - Importer handles both legacy and new formats
   - Automatic migration on first run
   - Manual reset: delete `/config/imported-files.json`

## Migration from Old Importer

1. **Stop old importer**: `docker stop claude-reflection-streaming`
2. **Start new importer**: `docker compose --profile async up -d`
3. **State preserved**: Automatic format migration
4. **No data loss**: All imported data retained

## Technical Deep Dive

### Why Thread Pool Executor Works

The memory leak in the original implementation was caused by ONNX Runtime's memory pooling strategy. When FastEmbed processes embeddings, ONNX Runtime allocates memory pools that aren't released back to the OS, even after `del` and `gc.collect()`.

By running FastEmbed operations in a thread pool executor:
1. Memory allocations happen in worker threads
2. Thread-local memory pools are isolated
3. Python's GIL ensures clean handoff of results
4. Main process memory remains stable

### Async Architecture Benefits

- **Non-blocking I/O**: File reading and Qdrant operations don't block
- **Concurrent Processing**: Multiple operations can overlap
- **Resource Efficiency**: Single process handles all operations
- **Clean Shutdown**: Graceful handling of interrupts

## Future Improvements

- [ ] Dynamic batch sizing based on available memory
- [ ] Parallel file processing (with memory safeguards)
- [ ] Incremental embedding updates for modified files
- [ ] Metrics endpoint for Prometheus monitoring
- [ ] Automatic memory limit detection

## References

- [Qdrant MCP Server](https://github.com/qdrant/mcp-server-qdrant) - Inspiration for thread pool pattern
- [FastEmbed Documentation](https://qdrant.github.io/fastembed/) - Local embedding library
- [ONNX Runtime Memory Management](https://onnxruntime.ai/docs/performance/tune-performance.html) - Understanding the memory issue