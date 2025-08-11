# MCP Reference Guide - CRITICAL FOR ALL AGENTS

This document contains CRITICAL information that ALL agents MUST follow when working with MCPs (Model Context Protocol servers).

## 🚨 CRITICAL MCP RULES - NEVER VIOLATE THESE

### 1. MCP Tool Naming Convention
**CORRECT**: When calling MCP tools in Claude Code, ALWAYS use the namespaced format:
```
mcp__<server-name>__<tool-name>
```

**Example for claude-self-reflect**:
- ✅ CORRECT: `mcp__claude-self-reflect__reflect_on_past`
- ✅ CORRECT: `mcp__claude-self-reflect__store_reflection`
- ❌ WRONG: `reflect_on_past` (missing namespace)
- ❌ WRONG: `mcp__claude-self-reflect__reflect_on_past` (wrong server name)

### 2. MCP Management Commands

#### ✅ CORRECT Commands (ONLY use these):
```bash
# List all MCPs and their connection status
claude mcp list

# Add MCP with environment variables (ORDER MATTERS: name, command, then flags)
claude mcp add <name> <command-or-path> -e KEY=value -e KEY2=value2

# Remove an MCP
claude mcp remove <name>

# Restart MCP (built-in command)
claude mcp restart <name>
```

#### ❌ INCORRECT Commands (NEVER use these):
```bash
claude mcp status <name>     # ❌ NO SUCH COMMAND
claude mcp logs <name>       # ❌ NO SUCH COMMAND  
claude mcp add <name>        # ❌ MISSING required commandOrUrl
```

### 3. Adding claude-self-reflect MCP

**CORRECT full command**:
```bash
claude mcp add claude-self-reflect "/Users/ramakrishnanannaswamy/projects/claude-self-reflect/mcp-server/run-mcp.sh" -e VOYAGE_KEY="<actual-key>" -e QDRANT_URL="http://localhost:6333"
```

**Common mistakes to AVOID**:
- ❌ Wrong order: `claude mcp add -e KEY=value claude-self-reflect /path`
- ❌ Missing command: `claude mcp add claude-self-reflect`
- ❌ Wrong path: Using relative paths instead of absolute
- ❌ Missing env vars: Not including required VOYAGE_KEY

### 4. MCP Tool Availability

**IMPORTANT**: After adding an MCP:
1. Tools may NOT be immediately available in current session
2. May need to restart Claude Code for tools to appear
3. Always verify with `claude mcp list` first
4. If tools don't work, check the namespaced format

### 5. Testing MCP Functionality

**Correct testing sequence**:
1. Check if MCP is connected: `claude mcp list`
2. Use namespaced tool names: `mcp__claude-self-reflect__reflect_on_past`
3. If tools fail, don't assume they don't exist - check namespace
4. If still failing, restart Claude Code

## 🎯 Quick Reference for claude-self-reflect

### Available Tools:
- `mcp__claude-self-reflect__reflect_on_past` - Search past conversations
- `mcp__claude-self-reflect__store_reflection` - Store insights
- `mcp__claude-self-reflect__search_by_file` - Search conversations by file path (v2.5.6+)
- `mcp__claude-self-reflect__search_by_concept` - Search conversations by concept (v2.5.6+)
- `mcp__claude-self-reflect__quick_search` - Quick search with minimal results
- `mcp__claude-self-reflect__search_summary` - Get aggregated insights without details
- `mcp__claude-self-reflect__get_more_results` - Pagination for search results

### Required Environment Variables:
- `VOYAGE_KEY`: API key for Voyage AI embeddings (required)
- `QDRANT_URL`: Vector database URL (default: http://localhost:6333)

### Optional Environment Variables:
- `ENABLE_MEMORY_DECAY`: Enable time-based relevance (default: false)
- `DECAY_WEIGHT`: Weight for decay calculation (default: 0.3)
- `DECAY_SCALE_DAYS`: Half-life for decay in days (default: 90)
- `OPENAI_API_KEY`: Fallback for embeddings if Voyage fails

### Common Operations:

**Search for past conversations**:
```javascript
mcp__claude-self-reflect__reflect_on_past({
  query: "your search query",
  limit: 5,
  minScore: 0.7,
  useDecay: false  // Optional: enable memory decay (default: false)
})
```

**Search with memory decay enabled**:
```javascript
mcp__claude-self-reflect__reflect_on_past({
  query: "recent debugging fixes",
  limit: 10,
  minScore: 0.6,
  useDecay: true  // Prioritize recent conversations
})
```

**Store a reflection**:
```javascript
mcp__claude-self-reflect__store_reflection({
  content: "Important insight or solution",
  tags: ["tag1", "tag2", "tag3"]
})
```

**Search by file path** (v2.5.6+):
```javascript
mcp__claude-self-reflect__search_by_file({
  file_path: "src/utils.py",
  limit: 10,
  project: "claude-self-reflect"  // Optional: filter by project
})
// Finds conversations that analyzed, edited, or mentioned this file in git outputs
```

**Search by concept** (v2.5.6+):
```javascript
mcp__claude-self-reflect__search_by_concept({
  concept: "docker",  // Searches for docker-related conversations
  limit: 10,
  include_files: true,  // Include file information
  project: "all"  // Search across all projects
})
// Returns conversations tagged with this concept
```

**Quick search for overview**:
```javascript
mcp__claude-self-reflect__quick_search({
  query: "authentication bug",
  minScore: 0.7,
  project: null  // Uses current project
})
// Returns count and top result only
```

## 📊 Memory Decay Feature

### What is Memory Decay?
Memory decay is a feature that prioritizes recent conversations over older ones using exponential decay. It helps find more relevant, up-to-date information by reducing the scores of older search results.

### How Memory Decay Works:
- **Formula**: `final_score = base_score * (decay_weight + (1 - decay_weight) * e^(-age/scale))`
- **Default half-life**: 90 days (configurable via DECAY_SCALE_DAYS)
- **Default weight**: 0.3 (configurable via DECAY_WEIGHT)

### Memory Decay Timeline:
- **< 1 week old**: ~95% of original score retained
- **1 month old**: ~75% of original score
- **3 months old**: ~50% of original score (half-life)
- **6 months old**: ~25% of original score
- **1 year old**: ~6% of original score

### When to Use Memory Decay:
✅ **USE when**:
- Finding recent solutions to current problems
- Tracking latest project decisions
- Getting up-to-date implementation patterns
- Debugging recent issues
- Finding current best practices

❌ **DON'T USE when**:
- Researching historical decisions
- Creating audit trails
- Finding original rationale for features
- Tracking long-term patterns
- Searching for foundational knowledge

### Configuring Memory Decay:
```bash
# Enable globally via environment variables
export ENABLE_MEMORY_DECAY=true
export DECAY_WEIGHT=0.3
export DECAY_SCALE_DAYS=90

# Or pass useDecay parameter in search
mcp__claude-self-reflect__reflect_on_past({
  query: "authentication bugs",
  useDecay: true  // Override global setting
})
```

## 🛠️ Troubleshooting Guide

### Issue: "No such tool available"
1. Check you're using namespaced format: `mcp__server-name__tool-name`
2. Verify MCP is connected: `claude mcp list`
3. Restart Claude Code if needed

### Issue: "Missing required argument 'commandOrUrl'"
- You forgot the command/path in `claude mcp add`
- Correct: `claude mcp add <name> <path> -e KEY=value`

### Issue: MCP not connecting
1. Check Qdrant is running: `docker ps | grep qdrant`
2. Verify environment variables are correct
3. Check run-mcp.sh has execute permissions
4. Verify VOYAGE_KEY is valid

### Issue: Search returns no results
1. Check if collections exist: `python scripts/check-collections.py`
2. Verify conversations were imported
3. Try lowering minScore parameter
4. Check if using correct project name

## 📋 Agent Checklist

Before working with MCPs, ALWAYS:
- [ ] Know the exact MCP server name (e.g., `claude-self-reflect`)
- [ ] Use namespaced tool format: `mcp__<server>__<tool>`
- [ ] Include all required parameters when adding MCPs
- [ ] Verify connection with `claude mcp list`
- [ ] Remember that `claude mcp status` and `claude mcp logs` DON'T EXIST
- [ ] Understand when to use memory decay vs. standard search

## 🚀 Copy-Paste Commands

```bash
# Check if Qdrant is running
docker ps | grep qdrant

# Start Qdrant if not running
cd /Users/ramakrishnanannaswamy/projects/claude-self-reflect
docker-compose up -d qdrant

# Remove and re-add claude-self-reflect (update VOYAGE_KEY)
claude mcp remove claude-self-reflect
claude mcp add claude-self-reflect "/Users/ramakrishnanannaswamy/projects/claude-self-reflect/mcp-server/run-mcp.sh" -e VOYAGE_KEY="your-actual-key" -e QDRANT_URL="http://localhost:6333"
claude mcp list

# Test MCP tools (after restart if needed)
# In Claude Code, use the tools directly
```

## 📚 Additional Resources

- **Project README**: `/Users/ramakrishnanannaswamy/projects/claude-self-reflect/README.md`
- **CLAUDE.md**: Project-specific rules and guidelines
- **Agents Directory**: `.claude/agents/` (NOT `/agents/`)
- **Test Scripts**: `scripts/test/` directory

---

**REMEMBER**: When in doubt, refer to this document. These rules are ABSOLUTE and must NEVER be violated by any agent. Memory decay is a powerful feature but should be used judiciously based on the use case.