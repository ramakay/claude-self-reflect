# Narrative Quality Analysis

## What Claude Received (Event Timeline Format)

The event extraction produced this structure:

```
## User Goals
[Message 3] in the /about page - please remove rama - the entire card...

## Errors Encountered
[Message 10] ✅ Resolved
Error: [{"type": "tool_use", "id": "toolu_...", "name": "TodoWrite"...
Fix: [{"tool_use_id": "toolu_...", "type": "tool_result"...

[Message 20] ❌ Unresolved
Error: [{"type": "tool_result", "content": "### Result\nError: page.goto: net::ERR_CONNECTION_REFUSED...

## Key Moments (by importance)
[Message 6] user: [{"tool_use_id": "toolu_...", "type": "tool_result", "content": "     1→'use client';\n...
```

### Problems Identified:

1. **Raw JSON Dumps**: Tool results are shown as raw JSON strings `[{"type": "tool_use"...}]`
   - Not human-readable
   - Contains metadata noise (tool_use_id, type fields)
   - Mixed with actual content

2. **Truncated Content**: Messages cut off mid-sentence
   - `[Message 6] ...Building2,\n    21→  Brain,` (cuts off)
   - Makes it hard to understand context

3. **No Context for Tool Results**: Shows `tool_result` content but not what tool was being used
   - `[Message 15] user: [{"tool_use_id": "toolu_...", "type": "tool_result", "content": "> procsolve-website@1.0.0 build\n> next build...`
   - Should say "Build output:" or "npm run build result:"

4. **Missing File Modifications Section**: The extraction has `extract_file_modifications()` but output doesn't show "## Files Modified"
   - This section would be valuable for understanding what changed

## What Claude Produced (79-Message Test)

Based on the truncated preview in the report, Claude generated:

```markdown
## Problem Statement
The user requested removal of a team member card ("Rama") from the /about
page of a Next.js website. The task involved locating the card in the React
component structure, removing the associated data...
```

### Quality Assessment:

✅ **Positives:**
- Claude correctly identified the core problem despite messy input
- Followed the structured format (## Problem Statement header)
- Understood the context was a Next.js website
- Recognized it was about removing a team member card

❌ **Concerns:**
- We only have a 500-character preview, not the full narrative
- Can't assess if Claude generated all required sections:
  - Problem Statement ✅ (confirmed)
  - Context (unknown)
  - Timeline of Events (unknown)
  - Attempted Solutions (unknown)
  - Final Solution (unknown)
  - Outcome (unknown)
  - Lessons Learned (unknown)
  - Keywords (unknown)

## What We Expected (From SKILL.md)

The Skill instructions requested this format:

```markdown
## Problem Statement
[Clear, concise description of the problem]

## Context
[Relevant background, environment, constraints]

## Timeline of Events
- [Timestamp/Message] Initial problem discovered
- [Timestamp/Message] First approach attempted
- [Timestamp/Message] Error encountered
- [Timestamp/Message] Solution found

## Attempted Solutions
[What was tried that didn't work, with explanations]

## Final Solution
[What ultimately worked and why]

## Outcome
[Results, validation, any remaining issues]

## Lessons Learned
[Key insights, patterns, best practices discovered]

## Keywords
[Comma-separated search-optimized terms]
```

## Critical Issues for Production

### 1. Event Timeline Format Needs Improvement

**Current:** Raw JSON dumps
```
[Message 6] user: [{"tool_use_id": "toolu_012EV4X1kpqAWYZKtviWeC1o", "type": "tool_result"...
```

**Better:**
```
[Message 6] Read file: src/app/about/page.tsx
  → Found TeamMember component with Rama's card
  → Contains: Engineering Leader role, 20+ years experience
```

### 2. Missing Context Extraction

**Need:**
- File paths being modified (extract_file_modifications exists but not in output)
- Clear before/after states
- Stack traces for errors (not raw JSON)

### 3. Token Efficiency vs Clarity Trade-off

**Current approach:**
- Dumps raw content to minimize token count
- Saves tokens but loses readability

**Suggested approach:**
- Extract semantic meaning from tool results
- Example: "Edit file X: Removed lines 15-32 (Rama's profile section)"
- Slightly more tokens but MUCH better for LLM comprehension

## Recommended Improvements

### 1. Improve `extract_events.py` Output Format

```python
def format_tool_result(content: Dict) -> str:
    """Convert tool result to human-readable format."""
    if content.get("type") == "tool_result":
        tool_name = infer_tool_name(content)  # From tool_use_id context

        if "error" in content.get("content", "").lower():
            return f"❌ {tool_name} failed: {extract_error_message(content)}"
        elif "build" in content.get("content", "").lower():
            return f"✅ Build succeeded: {extract_build_summary(content)}"
        else:
            return f"→ {tool_name}: {summarize_content(content, max_chars=150)}"
```

### 2. Add File Modification Tracking

Currently missing from output! Should show:
```
## Files Modified
[Message 5] Edit: src/app/about/page.tsx
  Context: Removing Rama's team member card as requested

[Message 8] Edit: src/app/about/page.tsx
  Context: Fixing layout after card removal
```

### 3. Better Error Formatting

**Current:**
```
[Message 20] ❌ Unresolved
Error: [{"type": "tool_result", "content": "### Result\nError: page.goto: net::ERR_CONNECTION_REFUSED...
```

**Better:**
```
[Message 20] ❌ Unresolved
Error: Playwright page.goto failed
  → net::ERR_CONNECTION_REFUSED at http://localhost:3000/about
  → Root cause: Development server not running
  → Attempted fix: Started server with npm run dev (Message 23)
```

## Test: Manual Claude Analysis

To truly assess quality, we should:

1. Generate a clean, well-formatted event timeline
2. Send it to Claude with the Skill
3. Capture the FULL output (not truncated)
4. Verify all sections are present
5. Check if keywords are search-optimized

**Hypothesis:** Claude can produce excellent narratives IF the input format is cleaner.

## Cost vs Quality Trade-off

| Approach | Tokens | Cost | Quality |
|----------|--------|------|---------|
| Raw JSON dumps (current) | 1,549 | $0.073 | Low - Noisy, hard to parse |
| Semantic extraction (proposed) | ~2,500 | $0.115 | High - Clear, structured |
| Full conversation (Spike 1) | 439,000 | $1.35 | Highest - Complete context |

**Recommendation:** Accept 60% more tokens (1,549 → 2,500) for dramatically better quality.
- Still 99.4% smaller than full conversation
- Cost increases only $0.042/conversation ($0.073 → $0.115)
- Annual cost: $142 → $230 (still 94.7% savings vs Spike 1's $4,316)

## Next Steps

1. **Rewrite event formatting** in `extract_events.py`:
   - Human-readable tool results
   - Semantic extraction instead of JSON dumps
   - Include file modifications section

2. **Test with clean input**:
   - Generate improved timeline
   - Send to Claude
   - Capture full narrative output
   - Validate all sections present

3. **Add validation**:
   - Check narrative contains all required sections
   - Verify keywords are present
   - Ensure problem-solution flow is clear

4. **Benchmark search quality**:
   - Store narratives in Qdrant
   - Test semantic search accuracy
   - Compare retrieval quality vs full conversations
