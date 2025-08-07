# Performance & Usage Guide

## 🚀 Lightning Fast Search
Optimized to deliver results in **200-350ms** (10-40x faster than v2.4.4)

## 🎯 Recommended Usage: Through Reflection-Specialist Agent

**Why use the agent instead of direct MCP tools?**
- **Preserves your main conversation context** - Search results don't clutter your working memory
- **Rich formatted responses** - Clean markdown instead of raw XML in your conversation
- **Better user experience** - Real-time streaming feedback and progress indicators
- **Proper tool counting** - Shows actual tool usage instead of "0 tool uses"
- **Automatic cross-project search** - Agent suggests searching across projects when relevant
- **Specialized search tools** - Access to quick_search, search_summary, and pagination

**Context Preservation Benefit:**
When you use the reflection-specialist agent, all the search results and processing happen in an isolated context. This means:
- Your main conversation stays clean and focused
- No XML dumps or raw data in your chat history
- Multiple searches won't exhaust your context window
- You get just the insights, not the implementation details

**Example:**
```
You: "What Docker issues did we solve?"
[Claude automatically spawns reflection-specialist agent]
⏺ reflection-specialist(Search Docker issues)
  ⎿ Searching 57 collections...
  ⎿ Found 5 relevant conversations
  ⎿ Done (1 tool use · 12k tokens · 2.3s)
[Returns clean, formatted insights without cluttering your context]
```

## ⚡ Performance Baselines

| Method | Search Time | Total Time | Context Impact | Best For |
|--------|------------|------------|----------------|----------|
| Direct MCP | 200-350ms | 200-350ms | Uses main context | Programmatic use, when context space matters |
| Via Agent | 200-350ms | 24-30s* | Isolated context | Interactive use, exploration, multiple searches |

*Note: The 24-30s includes context preservation overhead, which keeps your main conversation clean

**Note**: The specialized tools (`quick_search`, `search_summary`, `get_more_results`) only work through the reflection-specialist agent due to MCP protocol limitations.

## Memory Usage

- **Docker Memory**: 2GB minimum (4GB recommended for initial setup)
- **First Import**: May take 2-7 minutes to process all conversations
- **Subsequent Imports**: <60 seconds (only processes new/changed files)

## Search Quality

### Local vs Cloud Embeddings

| Mode | Speed | Quality | Privacy | Cost |
|------|-------|---------|---------|------|
| Local (Default) | Fast | Good | 100% Private | Free |
| Cloud (Voyage) | Fast | Better | Data sent to API | Free tier available |

### Score Interpretation
- **0.0-0.05**: Low relevance but can still be useful
- **0.05-0.15**: Moderate relevance (often contains good results)
- **0.15-0.3**: Good similarity (usually highly relevant)
- **0.3-0.5**: Strong similarity (very relevant matches)
- **0.5-1.0**: Excellent match (rare in practice)

Real-world semantic search scores are often lower than expected. Start with min_score=0.0 to see all results, then adjust based on quality.