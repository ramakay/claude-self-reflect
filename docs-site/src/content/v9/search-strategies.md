---
title: Search Strategies
---

## Natural Language Works Best

"How did we handle rate limiting?" finds "API throttling with token bucket."

## Right Tool for the Job

| Scenario | Tool |
|----------|------|
| "How did we fix X?" | `csr_reflect_on_past` |
| "Did we discuss X?" | `csr_quick_check` |
| "What happened this week?" | `get_recent_work` |
| "What changed in config.yaml?" | `csr_search_by_file` |
| "Security patterns across projects" | `csr_search_by_concept` |

## Tips

- **Be specific**: "Docker container OOM during build" > "Docker issue"
- **Include outcomes**: "how we decided between JWT and sessions"
- **Cross-project**: `project: "all"` when solutions might exist anywhere
- **File search**: When you remember the file, not the conversation

## Understanding Scores

| Score | Meaning |
|-------|---------|
| 0.6+ | Strong match |
| 0.4-0.6 | Good match |
| 0.3-0.4 | Weak match |
| <0.3 | Filtered |

## Poor Results?

1. Rephrase with different words
2. Run enrichment: `csr-engine --enrich`
3. Check status: `csr-engine status`
