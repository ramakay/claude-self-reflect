---
name: Analyzing Past Conversations
description: Transforms conversation search results into actionable narratives with problem-solution patterns, technical details, and reusable knowledge. Use when searching past conversations, analyzing development patterns, or helping users find solutions from previous work. Activates for reflect_on_past, quick_search, or any conversation analysis queries.
allowed-tools: mcp__claude-self-reflect__csr_reflect_on_past, mcp__claude-self-reflect__csr_quick_check, mcp__claude-self-reflect__csr_search_insights, mcp__claude-self-reflect__csr_get_more, mcp__claude-self-reflect__get_recent_work, mcp__claude-self-reflect__search_by_recency, mcp__claude-self-reflect__get_timeline, mcp__claude-self-reflect__get_full_conversation, mcp__claude-self-reflect__get_next_results, mcp__claude-self-reflect__store_reflection, Read, Grep
---

# Analyzing Past Conversations

Transform fragmented search results into comprehensive narratives that reveal patterns and solutions.

## Core Mission

**Raw CSR results are incomplete fragments**. Users see:
- Excerpts starting mid-sentence ("g what needs...")
- No problem context
- No solution details
- Duplicate results from same conversation

**Your mission**: Get full conversations and extract actionable knowledge.

## Available Tools (10 Verified)

### Primary Search (Choose ONE)

**csr_reflect_on_past** - Default for most queries
```python
csr_reflect_on_past(
    query="Docker volume issues",
    project="claude-self-reflect",  # or "all" for cross-project
    use_decay=0,  # CRITICAL: Always 0 for old conversations
    limit=3,
    mode="full"  # or "quick" for count only, "summary" for insights
)
```

**csr_quick_check** - Fast existence check
```python
csr_quick_check(
    query="authentication",
    project="claude-self-reflect"
)
# Returns: count + top match only
```

**csr_search_insights** - Pattern aggregation
```python
csr_search_insights(
    query="docker",
    project="claude-self-reflect"
)
# Returns: aggregated patterns, no individual results
```

### Time-Based Search

**search_by_recency** - Recent work
```python
search_by_recency(
    query="import pipeline errors",
    project="claude-self-reflect",
    time_range="last week",  # or "yesterday", "last month"
    limit=5
)
```

**get_recent_work** - Activity overview
```python
get_recent_work(
    limit=10,
    group_by="conversation",  # or "day", "session"
    include_reflections=True
)
```

**get_timeline** - Activity timeline
```python
get_timeline(
    project="claude-self-reflect",
    time_range="last week",
    granularity="day",  # or "hour", "week", "month"
    include_stats=True
)
```

### Retrieval & Context

**get_full_conversation** - CRITICAL for details
```python
get_full_conversation(
    conversation_id="abc123",
    project="claude-self-reflect"
)
# Returns file path - use Read to get JSONL
```

**csr_get_more** - Pagination
```python
csr_get_more(
    query="docker",
    offset=3,
    limit=3,
    project="claude-self-reflect"
)
```

**get_next_results** - Alternative pagination
```python
get_next_results(
    query="docker",
    offset=3,
    limit=3,
    project="claude-self-reflect"
)
```

**store_reflection** - Save insights
```python
store_reflection(
    content="Solution: Redis for session management works best",
    tags=["architecture", "sessions", "redis"]
)
```

## Tool Selection Decision Tree

```
Query Type:
├─ "Have we discussed X?" → csr_quick_check
├─ "What patterns for X?" → csr_search_insights
├─ "Recent work on X" → search_by_recency + get_recent_work
├─ "Show timeline" → get_timeline
├─ "What did we work on?" → get_recent_work
└─ Default: "Find X" → csr_reflect_on_past
```

## Multi-Tool Strategies

**Use 2-3 tools per query for comprehensive analysis.**

### Strategy 1: Deep Investigation
```
1. csr_reflect_on_past("docker issues") → Find conversations
2. csr_search_insights("docker") → Understand patterns
3. get_full_conversation(top_cid) → Complete details
4. Read(jsonl_path) → Extract events
```

### Strategy 2: Recent Troubleshooting
```
1. search_by_recency("auth", time_range="last week") → Recent work
2. get_timeline(time_range="last week") → Activity context
3. get_full_conversation(cid) → Full details
```

### Strategy 3: Pattern Discovery
```
1. csr_search_insights("performance") → Aggregated patterns
2. get_timeline(time_range="last 3 months") → Evolution
3. csr_reflect_on_past("performance", limit=5) → Specific cases
4. get_full_conversation(key_cids) → Solutions
```

## Workflow Checklist

```
Analysis Progress:
- [ ] Step 1: Select 2-3 appropriate tools
- [ ] Step 2: Execute primary search
- [ ] Step 3: Get supporting context (timeline/insights)
- [ ] Step 4: Retrieve full conversation (top 2-3 only)
- [ ] Step 5: Read JSONL and extract events
- [ ] Step 6: Generate enhanced narrative
- [ ] Step 7: Present actionable results
```

## Step-by-Step Process

### Step 1: Select Tools

Match query to strategy:
- Simple search → csr_reflect_on_past alone
- Recent issue → search_by_recency + get_timeline
- Pattern analysis → csr_search_insights + csr_reflect_on_past
- Activity check → get_recent_work + get_timeline

### Step 2: Execute Primary Search

Always use `use_decay=0` for old conversations:

```python
results = csr_reflect_on_past(
    query="your search terms",
    project="claude-self-reflect",
    use_decay=0,  # REQUIRED for conversations >1 week old
    limit=3
)
```

Note conversation IDs (cid) from results.

### Step 3: Get Supporting Context

**For timeline context**:
```python
timeline = get_timeline(
    project="claude-self-reflect",
    time_range="last month",
    granularity="week"
)
```

**For pattern insights**:
```python
insights = csr_search_insights(
    query="your search terms",
    project="claude-self-reflect"
)
```

### Step 4: Retrieve Full Conversations

**CRITICAL**: Only get full conversations for top 2-3 results (token management).

```python
# Get file path
conv = get_full_conversation(
    conversation_id="abc123",
    project="claude-self-reflect"
)

# Read the JSONL if file path returned
if conv contains file path:
    Read(file_path)
else:
    # Conversation not imported, work with excerpt
```

### Step 5: Extract Key Events

Scan full JSONL for:

**User Request** - What problem/goal?
**Solution Approach** - Which method used?
**Implementation** - Which files, what changes?
**Error Recovery** - What failed, how fixed?
**Validation** - Tests passed? Build succeeded?
**Outcome** - Final status?

### Step 6: Generate Enhanced Narrative

Structure your response:

```markdown
## Search Summary (1-2 sentences)
Problem + solution + outcome in concise form

## Problem-Solution Mapping
**Request**: Clear user ask
**Solution Type**: creation | edit | debugging | analysis
**Tools Used**: List critical tools
**Files Modified**: Paths with brief descriptions

## Technical Pattern (if reusable)
Pattern Name: [descriptive name]

When to use: [scenario]

Steps:
1. [Step 1]
2. [Step 2]
3. [Step 3]

## Implementation Details
- Approach and why
- Specific commands/code used
- Multiple iterations (if any)

## Validation & Outcome
- Test results
- Build status
- Error recovery
- Final completion status

## Search Keywords
Primary: 4-6 specific terms
Secondary: 6-10 variants, versions, errors
Frameworks: Technologies used
Pattern Tags: Reusable identifiers
```

### Step 7: Present Results

Show users:

```
Searched using: csr_reflect_on_past + get_timeline + csr_search_insights

Found 3 conversations about "Docker issues" (scores: 0.652-0.524)

Timeline: Activity in weeks 2025-09-15, 2025-09-22, 2025-10-01

[Rank 1] Score: 0.652 - claude-self-reflect project

## Search Summary
Fixed Docker volume mounting issue...
[complete narrative with all sections]

---

[Rank 2] Score: 0.542 - claude-self-reflect project
...

---

## Patterns Detected (from csr_search_insights):
- Common issue: Tilde expansion in paths
- Typical fix: Use absolute paths
- Related: Docker Compose, macOS Homebrew conflicts
```

## Critical Parameters

**use_decay=0** - ALWAYS set for old conversations
- Memory decay affects scores for conversations >1 week old
- Set to 0 to disable decay and find old solutions

**project scope**:
- Specific project: `project="claude-self-reflect"`
- All projects: `project="all"`

**limit**:
- Search: 3-5 results (avoid noise)
- get_full_conversation: Top 2-3 only (token cost)

## Quality Standards

### Must Include
✅ Clear problem statement
✅ Explicit solution approach
✅ Validation/outcome status

### Should Include (when available)
✅ Reusable technical pattern
✅ Error recovery details
✅ Specific commands/code used
✅ Timeline context

### Must Avoid
❌ Starting mid-sentence
❌ Generic descriptions ("fixed bug")
❌ Missing context
❌ Raw excerpts without enhancement

## When NOT to Enhance

Skip full analysis for:
- User asks for "quick" or "raw" results
- Simple yes/no existence checks (use csr_quick_check)
- Exploratory browsing (show excerpts only)
- Very long conversations (>500 messages) without clear relevance

## Error Handling

**"Conversation ID not found"** in get_full_conversation:
- Conversation hasn't been imported yet
- Work with excerpt from search results
- Mention: "Full conversation not imported, analysis based on available excerpt"

**Empty results**:
- Try broader query terms
- Check use_decay=0 is set
- Try project="all" for cross-project search
- Suggest: "No matches found. Try broader terms or check project name."

**Duplicate conversation IDs**:
- CSR may return same conversation multiple times with different scores
- Detect duplicates by checking cid
- Mention: "Note: Results X, Y, Z are from same conversation (duplicate detection issue)"

## Examples

### Example 1: Good Enhanced Output

```
Searched using: csr_reflect_on_past + csr_search_insights + get_timeline

Found 3 conversations about "Next.js team member removal" (scores: 0.681-0.512)

Timeline: All activity in week of 2025-09-15

[Rank 1] Score: 0.681 - procsolve-website project

## Search Summary
Removed team member profile card from Next.js About page using cascade
updates pattern with 12 coordinated changes in single MultiEdit operation.

## Problem-Solution Mapping
**Request**: Remove Rama's team member card from /about page
**Solution Type**: edit
**Tools Used**: MultiEdit (cascade updates), Next.js build, Playwright
**Files Modified**: src/app/about/page.tsx (12 coordinated edits)

## Technical Pattern
Pattern: Atomic array item removal with cascade updates

When to use: Removing item from React array while maintaining consistency

Steps:
1. Identify all references (data, UI, text)
2. Use MultiEdit for atomic batch deletion
3. Validate with production build
4. Test page rendering

## Validation
✅ 4 successful production builds
✅ Playwright tests passed
✅ Zero deployment errors

Keywords: Next.js team removal, React profile deletion, cascade updates,
MultiEdit patterns, atomic refactoring
```

### Example 2: Bad Raw Output (What to Avoid)

```
Found 3 conversations:

[Rank 1] Score: 0.439
"he page and see what's broken.

A: The page appears to be working..."
```

**Problems**: Mid-sentence start, no context, no solution, unusable.

## Performance Tips

- Use `mode="quick"` in csr_reflect_on_past for fast counts
- Get full conversations only for top 2-3 results
- For very long JSONL (>10,000 lines): Read first 100, skip to end
- Use csr_search_insights first to understand if deep dive is needed
- Cache conversation IDs for pagination with csr_get_more

## Remember

1. **Always use multiple tools** (2-3 minimum)
2. **Always set use_decay=0** for old conversations
3. **Always get full conversation** for top results
4. **Never show raw excerpts** - enhance them first
5. **Always validate** - check if conversation was imported
6. **Detect duplicates** - same cid appearing multiple times
