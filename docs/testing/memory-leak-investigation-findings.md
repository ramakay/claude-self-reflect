# FastEmbed Memory Leak Investigation Findings

## Date: 2025-08-14
## Issue: Streaming importer memory grows to 3GB+ causing OOM

## Root Cause Analysis

### The Problem
FastEmbed (using ONNX Runtime) has a native memory leak that causes memory to grow unbounded during continuous processing. The memory grows from ~120MB to 3GB+ when processing hundreds of conversation files.

### Why It Happens
1. **ONNX Runtime Memory Pooling**: ONNX Runtime uses aggressive memory pooling and doesn't release memory back to the OS
2. **Native Memory**: The leak is in native (C++) memory, not Python objects, so `gc.collect()` doesn't help
3. **Continuous Processing**: Unlike request/response patterns, continuous streaming accumulates memory

## Attempted Solutions

### 1. Memory Management Functions ❌ Partial Success
- Added `gc.collect()` after each file
- Added `malloc_trim(0)` to force memory release
- Set `MALLOC_ARENA_MAX=2` to reduce fragmentation
- **Result**: Slowed growth but didn't prevent hitting 3GB limit

### 2. Batch Size Reduction ❌ No Effect  
- Reduced from 50 → 25 → 10 embeddings per batch
- **Result**: Memory still grew to 3GB, just took longer

### 3. Thread Pool Isolation (Qdrant MCP Pattern) ❌ Failed
- Implemented AsyncEmbedder with ThreadPoolExecutor
- Used `asyncio.run_in_executor()` pattern from Qdrant MCP
- Added thread pool recycling every N files
- **Result**: Memory leak persisted because:
  - Our importer is fundamentally synchronous with `asyncio.run()` calls
  - Thread pool doesn't isolate memory in Python
  - Continuous processing vs request/response pattern

### 4. Full Async Rewrite ❌ Failed
- Created ground-up async version modeled on Qdrant MCP
- Used proper async/await throughout
- **Result**: Still hit memory issues because the underlying FastEmbed library holds memory

## Why Qdrant MCP Works But Ours Doesn't

### Qdrant MCP Server
- Processes individual requests (small data)
- Short-lived operations
- Memory released between requests
- True async from ground up (FastMCP framework)
- No continuous processing

### Our Streaming Importer  
- Processes hundreds of files continuously
- Long-running process
- Memory accumulates over time
- Retrofit async on sync codebase
- Continuous streaming pattern

## Viable Solutions

### 1. Container Restart Strategy ✅ Currently Working
- Let container restart when hitting memory limit
- State persistence ensures no data loss
- Simple and reliable
- **Downside**: Brief interruption every ~50 files

### 2. Subprocess Isolation (Not Implemented)
- Run FastEmbed in separate subprocess
- Kill and restart subprocess periodically
- **Downside**: Complex IPC, performance overhead

### 3. Switch to Voyage AI ✅ Recommended
- No memory leak issues
- Cloud-based processing
- **Downside**: Requires API key, costs money

## Performance Impact

- Processing rate: ~10 conversations/minute with memory management
- Memory grows at ~50MB per file
- Container restarts every ~50 files
- Total backlog: 543 files would take ~1 hour with restarts

## Recommendations

### Short Term
1. Keep container restart strategy with current memory management
2. Set memory limits appropriately (4GB recommended)
3. Monitor and adjust OPERATIONAL_MEMORY_MB

### Long Term  
1. **Preferred**: Switch to Voyage AI for production use
2. **Alternative**: Implement subprocess isolation if staying with FastEmbed
3. **Consider**: Alternative embedding libraries without memory leaks

## Lessons Learned

1. Thread pool isolation doesn't solve native memory leaks in Python
2. Async patterns don't help with memory management in native libraries
3. Container orchestration (restart strategy) is a valid solution for memory leaks
4. Not all patterns (like Qdrant MCP) translate to different use cases
5. Sometimes the "ugly" solution (container restarts) is the pragmatic choice

## Configuration for Production

```yaml
# docker-compose.yaml
streaming:
  mem_limit: 4g
  restart: unless-stopped
  environment:
    - OPERATIONAL_MEMORY_MB=2000  # Restart before 4GB
    - MALLOC_ARENA_MAX=2          # Reduce fragmentation
    - CHUNK_SIZE=10               # Smaller batches
```

## Status
- 543 files pending (82.4% backlog)
- Current solution: Container restarts with memory management
- Recommendation: Switch to Voyage AI or accept restart strategy