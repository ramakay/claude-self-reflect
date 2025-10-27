# V1 vs V2 Comparison

## Token Reduction

| Version | Tokens | Improvement |
|---------|--------|-------------|
| V1 | 1,549 | baseline |
| V2 | 886 | **43% reduction** |

**Savings: 663 tokens (43%)**

## Changes Made in V2

### ✅ 1. Fixed File Modifications Bug

**V1:** Section never appeared (0 modifications found)

**V2:**
```markdown
## Files Modified
[Msg 8] MultiEdit: ...naswamy/projects/procsolve-website/src/app/about/page.tsx
  Why: [shows context]
```

**Fix:**
- Checked for `role == "assistant"` instead of `type == "tool_use"` at message level
- Added "MultiEdit" to detection (not just "Edit")
- Excluded TodoWrite from file modifications

### ✅ 2. Removed tool_use_id

**V1:**
```
[Message 6] user: [{"tool_use_id": "toolu_012EV4X1kpqAWYZKtviWeC1o", "type": "tool_result"...
```

**V2 (Key Moments):**
```
[Msg 10] assistant: TodoWrite: Updated task list
```

**Improvement:** Much cleaner, no useless IDs

### ✅ 3. Deduplicated Errors

**V1:** TodoWrite "errors" appeared 4 times (Messages 10, 12, 17, etc.)

**V2:** Deduplicated - same error signature only appears once

### ✅ 4. Separated Resolved vs Unresolved Errors

**V1:**
```
## Errors Encountered
[Message 10] ✅ Resolved
[Message 12] ✅ Resolved
[Message 15] ✅ Resolved
[Message 17] ✅ Resolved
[Message 20] ❌ Unresolved
```

**V2:**
```
## Unresolved Errors
[Msg 20] ❌ ...
[Msg 43] ❌ ...
[Msg 56] ❌ ...
```

**Rationale:** Resolved errors don't need to be in the timeline - they're noise. Only unresolved issues matter.

### ✅ 5. Compressed Message References

**V1:** `[Message 10]`
**V2:** `[Msg 10]`

Small but adds up across 20+ references.

### ✅ 6. Truncated Build Outputs

**V1:** Full build output (200+ lines)

**V2:**
```
> next build
...
✓ Compiled successfully in 10.0s
```

Only keeps essential information.

## Remaining Issues to Fix

### ❌ Issue 1: User Goals Still Has Raw JSON

**Current:**
```
[Msg 6] [{'tool_use_id': 'toolu_012EV4X1kpqAWYZKtviWeC1o', 'type': 'tool_result'...
```

**Problem:** Message 6 is a `tool_result` (role: user), not an actual user request.

**Fix:** `extract_user_goals()` should skip tool_result messages, only keep genuine user text.

### ❌ Issue 2: Files Modified "Why" Still Has Raw JSON

**Current:**
```
  Why: [{'tool_use_id': 'toolu_012EV4X1kpqAWYZKtviWeC1o'...
```

**Fix:** Apply the same formatting logic used in Key Moments.

## Additional Compression Opportunities

### 1. Remove "type" Fields
All tool_result items have `"type": "tool_result"` - redundant, can infer from context.

### 2. Collapse Sequential TodoWrite Calls
```
[Msg 10] assistant: TodoWrite: Updated task list
[Msg 12] assistant: TodoWrite: Updated task list
[Msg 17] assistant: TodoWrite: Updated task list
```
Could become:
```
[Msg 10,12,17] assistant: TodoWrite: Updated task list (3x)
```

### 3. Remove Line Numbers from Code Snippets
```
     1→'use client';
     2→
     3→import React from 'react';
```
Could become:
```
'use client';

import React from 'react';
```

Saves ~10 chars per line.

### 4. Extract Just Filenames (not full paths)
```
/Users/username/projects/procsolve-website/src/app/about/page.tsx
```
Could become:
```
src/app/about/page.tsx
```

Already implemented in V2 with "..." prefix.

### 5. Use Acronyms for Common Terms
- `assistant` → `asst` or `A`
- `user` → `U`
- `TodoWrite` → `Todo`

This might hurt readability though.

## Estimated Additional Compression

| Improvement | Token Savings |
|-------------|--------------|
| Fix User Goals (remove tool_results) | -200 tokens |
| Fix Files Modified "Why" | -100 tokens |
| Remove "type" fields | -50 tokens |
| Collapse sequential TodoWrites | -80 tokens |
| Remove code line numbers | -120 tokens |

**Total potential: V2 (886) → V3 (336 tokens) = 78% compression vs V1**

## Cost Analysis

| Version | Tokens | Cost/conversation | Annual (3,200) |
|---------|--------|-------------------|----------------|
| V1 | 1,549 | $0.073 | $232 |
| V2 | 886 | $0.041 | $132 |
| V3 (projected) | 336 | $0.016 | $51 |

**V3 would be well under the $0.10/conversation budget!**

## Recommendation

Implement V3 with all optimizations:
1. Fix User Goals filtering (exclude tool_results)
2. Fix Files Modified "Why" formatting
3. Remove "type" fields
4. Collapse sequential identical tool calls
5. Remove code line numbers

This would achieve **maximum compression** while maintaining clarity.
