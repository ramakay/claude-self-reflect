# CPU Usage Optimization Analysis - Streaming Importer

## Critical Issue Identified
- **Problem**: Streaming importer running at 1437% CPU usage
- **Impact**: System overload, potential service degradation
- **Root Cause**: Continuous processing without throttling or delays

## Root Cause Analysis

### 1. Infinite Loop Without Delays
```python
# PROBLEM: Continuous processing
while True:
    new_files = await self.find_new_files()
    if new_files:
        # Process ALL files immediately
        for i in range(0, len(new_files), self.config.batch_size):
            await self.import_batch(batch)
```

### 2. Concurrent Embedding Generation
- FastEmbed running in ThreadPoolExecutor without limits
- Multiple embeddings processed simultaneously
- No semaphore or rate limiting

### 3. Rapid Qdrant Requests
- PUT requests every ~500ms
- No request throttling
- `wait=False` causing fire-and-forget pattern

### 4. Memory and GC Pressure
- Continuous object creation
- Garbage collection overhead
- No proper cleanup between batches

## Optimization Solution

### 1. CPU Throttling Implementation
```python
# Monitor and limit CPU usage
async def check_cpu_throttle(self):
    current_cpu = self.get_cpu_usage()
    if current_cpu > self.config.max_cpu_percent:
        delay = min(5.0, 0.5 * overage_ratio)
        await asyncio.sleep(delay)
```

### 2. Request Rate Limiting
```python
# Semaphore for concurrent operations
self.request_semaphore = asyncio.Semaphore(5)
self.embedding_semaphore = asyncio.Semaphore(2)
```

### 3. Batch Processing with Delays
```python
# Process in smaller batches
embedding_batch_size: int = 5  # Reduced from unlimited
file_processing_delay_s: float = 0.5  # Delay between files
embedding_delay_ms: int = 100  # Delay between embedding batches
```

### 4. Configuration Changes
| Parameter | Old Value | New Value | Impact |
|-----------|-----------|-----------|---------|
| import_frequency | 5s | 30s | 6x reduction in check frequency |
| batch_size | 10 | 5 | 50% reduction in concurrent files |
| memory_limit_mb | 500 | 400 | Tighter memory control |
| max_cpu_percent | N/A | 50% | CPU usage cap |

## Performance Metrics

### Before Optimization
- CPU Usage: 1437%
- Memory: 364MB
- Requests/sec: ~2
- Files/min: ~12

### Expected After Optimization
- CPU Usage: <50%
- Memory: <400MB
- Requests/sec: ~0.5
- Files/min: ~4-6

## Implementation Steps

1. **Stop current streaming importer**
   ```bash
   docker stop claude-reflection-streaming
   ```

2. **Deploy optimized version**
   ```bash
   cp scripts/streaming-importer-optimized.py scripts/streaming-importer.py
   docker-compose up -d streaming-importer
   ```

3. **Monitor performance**
   ```bash
   docker stats claude-reflection-streaming
   ```

## Key Improvements

1. **Adaptive Throttling**: Dynamically adjusts delays based on CPU usage
2. **Semaphore Controls**: Limits concurrent operations
3. **Batch Size Reduction**: Processes fewer files simultaneously
4. **Inter-operation Delays**: Adds breathing room between operations
5. **CPU Monitoring**: Tracks and responds to CPU usage patterns

## Risk Mitigation

1. **Slower Processing**: Files will be processed more slowly but sustainably
2. **Queue Buildup**: May accumulate backlog during high activity
3. **Memory Management**: More aggressive cleanup between batches

## Monitoring Commands

```bash
# Watch CPU usage
watch -n 1 'docker stats --no-stream claude-reflection-streaming'

# Check logs
docker logs -f claude-reflection-streaming

# Monitor Qdrant load
curl http://localhost:6333/metrics
```

## Rollback Plan

If issues occur:
```bash
# Restore original
cp scripts/streaming-importer-backup.py scripts/streaming-importer.py
docker-compose restart streaming-importer
```

## Conclusion

The optimization reduces CPU usage from 1437% to target 50% through:
- Rate limiting and throttling
- Batch size reduction
- Strategic delays
- CPU usage monitoring

This trades processing speed for system stability and sustainability.