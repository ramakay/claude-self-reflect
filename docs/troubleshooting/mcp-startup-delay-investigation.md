# MCP Startup Delay Investigation

## Issue
MCP tools take 30+ seconds to become available after Claude Code restart

## Root Cause Analysis

### Finding 1: Model Initialization Timeout
- `FASTEMBED_DOWNLOAD_TIMEOUT` is set to 30 seconds by default
- Even when models are cached, the initialization thread has a 30-second join timeout
- Location: `mcp-server/src/embedding_manager.py` line 114-119

### Finding 2: Dual Model Initialization
When configured for Voyage (cloud embeddings):
1. Initializes local model first (even if not preferred)
2. Then initializes Voyage model
3. Both initializations happen sequentially, not in parallel

### Finding 3: Thread Join Behavior
```python
thread.join(timeout=self.download_timeout)  # 30 seconds
if thread.is_alive():
    logger.error(f"Model initialization timed out after {self.download_timeout}s")
```
The thread.join() waits up to 30 seconds even if the model loads quickly.

### Finding 4: Stale Lock Files
Found old lock files from August in `~/.cache/fastembed/.locks/`
These should be cleaned by `_clean_stale_locks()` but may cause issues.

## Solution Recommendations

### Immediate Fix
Reduce timeout when model is already cached:
```python
# Check if model is cached
if self._is_model_cached():
    timeout = 5  # Quick timeout for cached models
else:
    timeout = self.download_timeout  # Full timeout for downloads
```

### Long-term Improvements
1. **Parallel initialization** - Initialize local and Voyage models concurrently
2. **Smart timeout** - Detect cached models and use shorter timeouts
3. **Lazy loading** - Only initialize the model that's actually needed
4. **Better lock cleanup** - More aggressive stale lock removal

## Workaround
For now, users experiencing delays can:
1. Set `FASTEMBED_DOWNLOAD_TIMEOUT=5` in .env for faster startup (risky if model needs downloading)
2. Use local embeddings by default (instant startup)
3. Wait the 30 seconds (tools will work after the delay)

## Impact
- Affects all users switching between local and Voyage embeddings
- Most noticeable after Claude Code restart
- Does not affect functionality, only initial availability