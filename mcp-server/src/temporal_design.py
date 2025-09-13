"""
Temporal Query API Design for Claude Self Reflect
==================================================

This document outlines the design for new temporal query capabilities.

## New MCP Tools

### 1. get_recent_work
Purpose: Answer "What did we work on last?" queries
Parameters:
- limit: int (default=10) - Number of recent conversations to return
- project: Optional[str] - Specific project or 'all' for cross-project
- include_reflections: bool (default=True) - Include stored reflections
- group_by: str (default='conversation') - Group by 'conversation', 'day', or 'session'

Returns:
- List of recent conversations/sessions with:
  - timestamp (ISO format)
  - relative_time (e.g., "2 hours ago", "yesterday")
  - project_name
  - summary (auto-generated from content)
  - main_topics (extracted concepts)
  - files_worked_on (if any)
  - conversation_id

### 2. search_by_recency
Purpose: Time-constrained semantic search ("docker issues last week")
Parameters:
- query: str - Semantic search query
- time_range: str - Natural language time ("last week", "yesterday", "past 3 days")
  OR
- since: Optional[str] - ISO timestamp or relative time
- until: Optional[str] - ISO timestamp or relative time
- limit: int (default=10)
- min_score: float (default=0.3)
- project: Optional[str]

Returns:
- Search results filtered and sorted by time
- Each result includes relative_time for context

### 3. get_timeline
Purpose: Show activity timeline for a project or across all projects
Parameters:
- time_range: str (default="last_week") - Natural language or specific range
- project: Optional[str] - Specific project or 'all'
- granularity: str (default='day') - 'hour', 'day', 'week', 'month'
- include_stats: bool (default=True) - Include activity statistics

Returns:
- Timeline with activity grouped by time period
- Statistics: messages_count, files_edited, concepts_discussed
- Activity heat map data
- Peak activity times

## Natural Language Time Parsing

Supported formats:
- Relative: "today", "yesterday", "last week", "past 3 days", "this month"
- Specific: "January 2025", "last Monday", "2 days ago"
- Ranges: "last 7 days", "past month", "since yesterday"

Implementation using dateparser library or custom parser with:
- Timezone awareness (use user's timezone if available)
- Fuzzy matching for common phrases
- ISO 8601 fallback for precise queries

## Timestamp Indexing Optimization

1. Create Qdrant index on timestamp field for faster sorting
2. Use server-side filtering for time ranges
3. Implement caching for recent queries (5-minute TTL)
4. Pre-compute relative times during import

## Integration with Existing Tools

### Enhanced reflect_on_past:
- Add optional `sort_by` parameter: 'relevance' (default) or 'recency'
- Add optional `since` parameter for time filtering
- Include relative_time in all results

### Backward Compatibility:
- All existing tools continue to work unchanged
- New parameters are optional with sensible defaults
- Response format extensions are additive only

## Performance Considerations

1. Index timestamp field in all collections
2. Use Qdrant's native date filtering when possible
3. Implement result caching for common queries
4. Limit default results to prevent overwhelming responses

## Edge Cases to Handle

1. Empty time periods (no conversations)
2. Timezone differences between conversations
3. Ambiguous time expressions ("last Friday" on a Monday vs Tuesday)
4. Very old conversations (show year in relative time)
5. Conversations spanning multiple days
6. Deleted or moved conversations

## Example Usage

```python
# What did we work on last?
result = await get_recent_work(limit=5, group_by='session')

# When did we last discuss Docker?
result = await search_by_recency(
    query="docker containers",
    time_range="past month",
    limit=5
)

# Show my activity this week
timeline = await get_timeline(
    time_range="this week",
    granularity="day",
    include_stats=True
)
```

## Testing Strategy

1. Unit tests for time parsing functions
2. Integration tests with mock Qdrant data
3. Edge case tests for timezone handling
4. Performance tests with large datasets
5. User acceptance tests with natural language queries
"""