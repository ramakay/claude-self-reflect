# Release Notes - Claude Self Reflect v2.5.0

## 🚀 Critical Production Fixes

### Memory Management Overhaul
**Problem**: Streaming importer was getting OOM killed with 268MB+ conversation files  
**Solution**: 
- Increased container memory limit from 600MB to 1GB
- Implemented chunked JSONL processing (50 messages per chunk)
- Increased operational memory limit from 50MB to 100MB
- Added garbage collection between chunks

**Result**: Successfully handles files up to 268MB without OOM

### Collection Cache Optimization
**Problem**: Excessive Qdrant API calls (90+ per cycle) causing memory pressure  
**Solution**: 
- Implemented proper cache refresh that populates collections after clearing
- Added 60-second cache refresh interval
- Reduced API calls by 99% (from 90+ to 1 per cycle)

**Result**: Stable memory usage and reduced API overhead

### State Management Improvements
**Problem**: Risk of state file corruption during OOM events  
**Solution**: 
- Already had atomic state saves using `os.replace()`
- Verified temp file cleanup
- Ensured state persistence across restarts

**Result**: Zero data loss during container restarts

## 📊 Performance Metrics

| Metric | Before | After |
|--------|--------|-------|
| Memory Limit | 600MB | 1GB |
| Operational Memory | 50MB | 100MB |
| Large File Support | ❌ OOM at 24MB | ✅ Handles 268MB |
| API Calls/Cycle | 90+ | 1 |
| Container Stability | Restarts every 60s | Stable for hours |
| Memory Usage | OOM kills | ~262MB (25% of limit) |

## 🔧 Technical Changes

### docker-compose.yaml
```yaml
mem_limit: 1g  # Increased from 600m
memswap_limit: 1g
OPERATIONAL_MEMORY_MB: 100  # Increased from 50
```

### scripts/streaming-importer.py
- Added chunked JSONL processing with 50 messages per chunk
- Fixed collection cache to actually populate after clearing
- Added `gc.collect()` for memory management
- Chunk naming: `{conversation_id}_chunk_{n}` for large files

### .claude/agents/claude-self-reflect-test.md
- Added comprehensive cloud embedding test with backup/restore
- Includes Voyage AI dimension verification (1024 vs 384)
- Full mode switching test procedure

## ✅ Validation Results

- **Memory Stability**: ✅ Running stable at 262MB (25% of 1GB limit)
- **Large Files**: ✅ Successfully processed 268MB JSONL file
- **Collection Cache**: ✅ Only 1 API call per cycle
- **State Persistence**: ✅ Survives container restarts
- **Import Performance**: ✅ Processing 483+ collections
- **Cloud Embeddings**: 🔄 Test procedure added (requires VOYAGE_KEY)

## 🚨 Breaking Changes

None - All changes are backward compatible

## 📝 Migration Guide

For existing users:
1. Pull latest version: `git pull`
2. Restart containers: `docker compose --profile watch down && docker compose --profile watch up -d`
3. Large files (>100MB) will now be automatically chunked
4. No data migration required

## 🐛 Known Issues

- Large files temporarily moved during testing should be restored:
  ```bash
  mv ~/claude-large-files-backup/*.jsonl ~/.claude/projects/-Users-*/
  ```
- Cloud embedding test requires VOYAGE_KEY environment variable

## 🔮 Future Improvements

- Configurable chunk size (currently hardcoded at 50 messages)
- Streaming embeddings generation to further reduce memory
- Automatic large file detection and optimization
- Progress reporting for large file processing

## 📈 Upgrade Recommendation

**CRITICAL**: All users should upgrade to v2.5.0 immediately if experiencing:
- OOM kills with large conversation files
- Container restart loops
- High memory usage
- Slow import performance

## 🙏 Acknowledgments

This release addresses critical stability issues discovered during extensive stress testing with real-world conversation files up to 268MB in size.

---
**Version**: 2.5.0  
**Release Date**: August 5, 2025  
**Stability**: Production Ready  
**Minimum Requirements**: Docker with 1GB+ available memory