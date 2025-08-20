# Voyage AI Token Limit Fix

## Problem

The import script was failing with "The max allowed tokens per submitted batch is 120000" errors when importing large conversations to Voyage AI. This was causing data loss as some conversations couldn't be imported.

## Root Cause

The script was batching conversations based on message count (10 messages per chunk, 10 chunks per batch = 100 messages), without considering the actual token count. Large messages with code blocks or verbose content could easily exceed Voyage AI's 120,000 token limit per batch.

## Solution

Implemented token-aware batching that:
1. Estimates token count before sending to Voyage AI
2. Dynamically adjusts batch sizes to stay under limits
3. Splits oversized chunks when necessary
4. Provides fallback to original batching method

## Implementation Details

### Token Estimation
- Conservative estimate: 3 characters = 1 token
- Provides safety buffer for the 120k limit
- Configurable via `TOKEN_ESTIMATION_RATIO` environment variable

### Dynamic Batching
- Maximum tokens per batch: 100,000 (20k buffer from 120k limit)
- Accumulates chunks until approaching limit
- Starts new batch when limit would be exceeded
- Configurable via `MAX_TOKENS_PER_BATCH` environment variable

### Chunk Splitting
- Detects chunks that exceed token limit individually
- Splits by messages first (preserves context better)
- Recursively splits if needed
- Truncates single oversized messages as last resort

### Configuration

New environment variables:
- `MAX_TOKENS_PER_BATCH` (default: 100000) - Maximum tokens per Voyage AI batch
- `TOKEN_ESTIMATION_RATIO` (default: 3) - Characters per token estimate
- `USE_TOKEN_AWARE_BATCHING` (default: true) - Enable/disable token-aware batching

### Backwards Compatibility

The implementation includes a feature flag (`USE_TOKEN_AWARE_BATCHING`) that allows falling back to the original batching method if needed. This ensures we can quickly revert if issues arise.

## Testing

To test the fix:
1. Set `USE_TOKEN_AWARE_BATCHING=true` (default)
2. Import conversations with large messages
3. Monitor logs for batch statistics
4. Verify no token limit errors occur

### Debug Logging

Enable debug logging to see batch statistics:
```bash
export LOG_LEVEL=DEBUG
python scripts/import-conversations-unified.py
```

This will show:
- Number of batches created
- Chunks per batch
- Estimated tokens per batch

## Performance Impact

- Minimal overhead from token counting (~1-2ms per chunk)
- May create more API calls (smaller batches) but avoids failures
- Overall more reliable imports

## Migration

No migration needed - the fix is backwards compatible and works with existing data.

## Monitoring

Watch for:
- "Chunk with X estimated tokens exceeds limit" warnings (chunk splitting)
- "Single message exceeds token limit, truncating" warnings (data truncation)
- Batch statistics in debug logs

## Future Improvements

1. Implement more accurate token counting (e.g., using tiktoken library)
2. Add metrics collection for batch sizes and token usage
3. Implement retry logic for individual chunks rather than full batches
4. Consider streaming API for very large conversations