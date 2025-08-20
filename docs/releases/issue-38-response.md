# GitHub Issue #38 Response

Thank you for the detailed bug report. The token limit issue has been addressed in the `fix/voyage-token-limit-issue-38` branch.

## Root Cause

The import script was using fixed batching based on message count (10 chunks × 10 messages = 100 messages per API call) without considering actual token content. Large conversations with extensive code blocks or verbose content would exceed Voyage AI's 120,000 token limit, causing import failures after multiple retry attempts.

## Solution Implemented

The fix introduces token-aware batching that dynamically adjusts batch sizes based on estimated token count. Key components:

### Token Estimation
- Content-aware heuristics that adjust for code and JSON content
- Configurable character-to-token ratio with sensible defaults
- Built-in safety margin to prevent edge case failures

### Dynamic Batching
- Accumulates chunks until approaching token limit
- Automatically splits oversized chunks along message boundaries
- Truncates single oversized messages as last resort with full tracking

### Safety Measures
- Recursion depth limits prevent stack overflow
- Environment variable validation ensures safe operating ranges
- Comprehensive error handling and logging

## Configuration

The solution is configurable via environment variables:
```bash
MAX_TOKENS_PER_BATCH=100000  # Maximum tokens per batch (default: 100000)
TOKEN_ESTIMATION_RATIO=3      # Characters per token estimate (default: 3)
USE_TOKEN_AWARE_BATCHING=true # Enable token-aware batching (default: true)
```

## Testing

To verify the fix:
1. Checkout the branch: `git checkout fix/voyage-token-limit-issue-38`
2. Run import with the problematic file mentioned in your report
3. Monitor logs for batch statistics and successful completion

The implementation includes detailed logging at DEBUG level to track batch creation and token estimates.

## Backward Compatibility

The fix includes a feature flag that allows reverting to legacy batching if needed. All existing imports remain unaffected.

## Next Steps

Please test this fix with your dataset, particularly the file that was failing (fbf99a90-f824-4c3b-8bad-4438690f0dbc.jsonl). The branch is ready for review and testing.

After verification, this will be merged and included in the next release. The fix ensures reliable imports even for conversations with extensive code content or large messages.

Let me know if you encounter any issues during testing.