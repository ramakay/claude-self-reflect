# Release Notes - Claude Self Reflect v2.5.0

## 🚀 Major Features

### Streaming Importer - Production-Ready Memory-Efficient Import
The new streaming importer replaces the legacy watcher with a memory-optimized solution that maintains under 50MB operational memory while processing conversations continuously.

**Key Improvements:**
- **99% reduction in API calls** through intelligent collection caching
- **JSONL support** for Claude's native conversation format
- **Memory efficiency** with operational usage under 50MB
- **Active session prioritization** for real-time conversation updates

## 🔧 Technical Improvements

### Memory Optimization
- Separated operational memory (50MB) from model memory (237MB FastEmbed)
- Container memory limit set to 600MB for stability
- Implemented collection caching to reduce Qdrant API pressure
- Fixed memory leaks in conversation processing

### Format Support
- Added native JSONL (JSON Lines) parsing for Claude conversations
- Fixed UUID generation for Qdrant point IDs using uuid5
- Improved conversation text extraction from assistant messages

### Performance
- Reduced Qdrant API calls from 90+ to 1 per import cycle
- Implemented 60-second collection cache refresh
- Optimized file processing with batch operations
- Active session detection for prioritized imports

## 🐛 Bug Fixes

1. **Fixed OOM kills** - Container was being killed due to excessive API calls
2. **Fixed point ID format** - Qdrant requires valid UUIDs, not arbitrary strings
3. **Fixed JSONL parsing** - Now correctly handles Claude's conversation format
4. **Fixed collection checks** - Eliminated redundant API calls through caching

## 📦 Container Changes

### Docker Compose Updates
```yaml
streaming-importer:
  mem_limit: 600m          # Increased from 400m
  environment:
    - WATCH_INTERVAL=60    # Production setting
    - MAX_MEMORY_MB=350    # Total memory budget
    - OPERATIONAL_MEMORY_MB=50  # Operations budget
```

### Removed Services
- `watcher` service deprecated in favor of `streaming-importer`

## 🔄 Migration Guide

### For Existing Users

1. **Stop old watcher**:
   ```bash
   docker compose --profile watch-old down
   ```

2. **Start new streaming importer**:
   ```bash
   docker compose --profile watch up -d
   ```

3. **Verify operation**:
   ```bash
   docker logs claude-reflection-streaming --tail 20
   ```

### Configuration Changes
- Default embedding model: FastEmbed (local, 384 dimensions)
- Set `PREFER_LOCAL_EMBEDDINGS=false` for Voyage AI
- Memory limit increased to 600MB (was 400MB)

## 📊 Performance Metrics

| Metric | v2.4.x | v2.5.0 | Improvement |
|--------|--------|--------|-------------|
| API calls/cycle | 90+ | 1 | 99% reduction |
| Operational memory | 100-200MB | <50MB | 75% reduction |
| Container restarts | Frequent | None | 100% stability |
| JSONL support | No | Yes | New feature |

## 🧪 Testing

Comprehensive stress testing completed:
- 25 test conversations (small/medium/large)
- 5-second import intervals (12x normal speed)
- 2+ hours continuous operation
- Memory monitoring with CSV logging
- MCP search validation confirmed

## 📝 Documentation

- Added streaming importer validation report
- Updated memory requirements documentation
- Created test conversation generator
- Added memory monitoring scripts

## ⚠️ Breaking Changes

None - this release is fully backward compatible.

## 🔜 Future Improvements

- Voyage AI embedding validation (pending)
- Further memory optimizations
- Parallel processing for large conversation batches
- Web UI for monitoring import status

## 🙏 Acknowledgments

Thanks to all users who reported memory issues and helped test the streaming importer.

## 📥 Installation

```bash
# Update to v2.5.0
git pull
docker compose build streaming-importer
docker compose --profile watch up -d
```

## 🐞 Known Issues

- Test files in `~/.claude/projects/` may not be visible to Docker without proper volume mapping
- Voyage AI mode not yet validated in v2.5.0 stress tests

## 📚 References

- [Streaming Importer Validation Report](./docs/testing/v2.5.0-streaming-importer-validation.md)
- [Memory Investigation Findings](./docs/troubleshooting/memory-investigation-findings.md)
- [Docker Architecture](./docs/architecture/docker-subprocess-memory-fix.md)