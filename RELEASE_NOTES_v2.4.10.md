# Release Notes - v2.4.10

## 🚀 Memory Optimization & Reliability Release

This release addresses critical memory issues discovered in v2.4.9, making Claude Self-Reflect more reliable and accessible for users with standard Docker memory allocations.

## 🎯 Key Improvements

### Memory Efficiency
- **60% reduction in memory usage** during import operations
- Import watcher now works reliably with 2GB Docker memory (after initial setup)
- No more OOM (Out of Memory) kills during normal operation

### Reliability Enhancements
- **Incremental state saving**: Progress saved after each file, not just at end
- **Graceful recovery**: If interrupted, import resumes from last successful file
- **Garbage collection**: Automatic memory cleanup after each file processing

## 📋 What Changed

### For New Users
- First-time import may still benefit from temporary 4GB Docker memory allocation
- Clear guidance added to README about memory requirements
- Setup wizard now provides memory configuration recommendations

### For Existing Users
- After updating, your import watcher will use significantly less memory
- No configuration changes required - improvements are automatic
- Subsequent imports remain fast (<60 seconds) with minimal memory usage

## 📊 Performance Metrics

| Metric | Before (v2.4.9) | After (v2.4.10) |
|--------|-----------------|------------------|
| Batch Size | 100 messages | 10 messages |
| Memory Peak | 2GB+ (OOM) | 1.5GB max |
| State Saves | Once at end | After each file |
| Recovery | Lost progress | Resumes from last file |

## 📚 New Documentation

### Memory Footprint Guide
Comprehensive documentation at `docs/memory-footprint.md` covering:
- First-time vs incremental import patterns
- Memory usage analysis
- Troubleshooting OOM issues
- Performance optimization tips

### Enhanced Troubleshooting
New section in `docs/troubleshooting.md` specifically for memory issues:
- OOM kill solutions
- Container restart fixes
- Memory configuration guidance

## 🔧 Technical Details

### Code Changes
- `scripts/import-conversations-unified.py`: Added garbage collection, reduced batch size
- `docker-compose.yaml`: Memory limits remain at 2GB (sufficient after optimizations)
- State persistence now happens incrementally for reliability

### Breaking Changes
None - this is a backward-compatible optimization release.

## 💡 Recommendations

### For Best Experience
1. **New installations**: Consider temporarily setting Docker to 4GB for first import
2. **Existing installations**: No action needed - improvements are automatic
3. **Low memory systems**: Now viable with 2GB Docker allocation

### Monitoring
Check import progress and memory usage:
```bash
# View import logs
docker logs -f claude-reflection-watcher

# Monitor memory usage
docker stats claude-reflection-watcher
```

## 🙏 Acknowledgments

Thanks to our users who reported memory issues and helped test the fixes. Your feedback drives these improvements!

## 📦 Upgrade Instructions

```bash
# Update the npm package
npm update -g claude-self-reflect

# Restart services to apply changes
docker compose down
docker compose --profile watch up -d
```

---

**Full changelog**: [v2.4.9...v2.4.10](https://github.com/ramakay/claude-self-reflect/compare/v2.4.9...v2.4.10)