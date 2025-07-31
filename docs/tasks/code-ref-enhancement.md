# Code Reference Enhancement Plan

## Overview
Enhance Claude Self-Reflect to extract and store tool usage data from JSONL files, enabling queries that link abstract concepts to specific files and code interactions.

## Problem Statement
Currently, when Claude analyzes code without editing, we lose track of:
- Which files were examined
- What patterns were searched for
- Which commands were executed
- The relationship between discussions and code

## Solution: Extract Tool Usage from JSONL

### What JSONL Contains (Beyond Chunks)
1. **Complete tool invocations** with parameters
2. **File paths** accessed via Read/Grep/Edit
3. **Search patterns** and their results
4. **Commands executed** via Bash
5. **Timing information** for each operation
6. **Tool result data** (success/failure, output)

## Implementation Plan (Single Release)

### Phase 1: Core Implementation

#### 1.1 Update Import Script
```python
# scripts/import-conversations-unified.py

def extract_tool_usage_from_jsonl(jsonl_path):
    """Extract all tool usage from a conversation."""
    tool_usage = {
        "files_read": [],
        "files_edited": [],
        "files_created": [],
        "grep_searches": [],
        "bash_commands": [],
        "glob_patterns": [],
        "tools_summary": {},
        "concepts": set(),
        "timing": {}
    }
    
    with open(jsonl_path, 'r') as f:
        for line in f:
            data = json.loads(line)
            if 'message' in data and 'content' in data['message']:
                for content in data['message']['content']:
                    if content.get('type') == 'tool_use':
                        extract_single_tool_use(content, tool_usage)
    
    return tool_usage

def extract_single_tool_use(tool_data, usage_dict):
    """Parse individual tool usage."""
    tool_name = tool_data.get('name')
    inputs = tool_data.get('input', {})
    tool_id = tool_data.get('id')
    
    # Track tool frequency
    usage_dict['tools_summary'][tool_name] = usage_dict['tools_summary'].get(tool_name, 0) + 1
    
    if tool_name == 'Read':
        usage_dict['files_read'].append({
            'path': inputs.get('file_path'),
            'lines': [inputs.get('offset', 0), inputs.get('limit', -1)],
            'tool_id': tool_id
        })
    elif tool_name == 'Grep':
        usage_dict['grep_searches'].append({
            'pattern': inputs.get('pattern'),
            'path': inputs.get('path', '.'),
            'glob': inputs.get('glob'),
            'output_mode': inputs.get('output_mode', 'files_with_matches')
        })
    elif tool_name == 'Edit':
        usage_dict['files_edited'].append({
            'path': inputs.get('file_path'),
            'old_string': inputs.get('old_string', '')[:50],  # Truncate for storage
            'operation': 'edit'
        })
    elif tool_name == 'Write':
        usage_dict['files_created'].append(inputs.get('file_path'))
    elif tool_name == 'Bash':
        cmd = inputs.get('command', '')
        usage_dict['bash_commands'].append({
            'command': cmd.split()[0] if cmd else 'unknown',  # Store command name only
            'description': inputs.get('description', '')
        })
    elif tool_name == 'Glob':
        usage_dict['glob_patterns'].append({
            'pattern': inputs.get('pattern'),
            'path': inputs.get('path', '.')
        })
```

#### 1.2 Enhanced Chunk Creation
```python
def create_enhanced_chunk(messages, chunk_index, tool_usage, conversation_metadata):
    """Create chunk with tool usage metadata."""
    chunk_text = extract_chunk_text(messages)
    
    # Extract concepts from text
    concepts = extract_concepts(chunk_text, tool_usage)
    
    # Deduplicate and clean file paths
    files_analyzed = list(set([
        normalize_path(f['path']) 
        for f in tool_usage['files_read'] + tool_usage['files_edited']
        if f.get('path')
    ]))
    
    return {
        "text": chunk_text,
        "conversation_id": conversation_metadata['id'],
        "timestamp": conversation_metadata['timestamp'],
        "project": conversation_metadata['project'],
        
        # NEW: Tool usage metadata
        "files_analyzed": files_analyzed[:20],  # Limit to prevent payload bloat
        "files_edited": [normalize_path(f) for f in tool_usage['files_edited']][:10],
        "search_patterns": [s['pattern'] for s in tool_usage['grep_searches']][:10],
        "concepts": list(concepts)[:15],
        "tool_summary": tool_usage['tools_summary'],
        "analysis_only": len(tool_usage['files_edited']) == 0,
        
        # Additional context
        "commands_used": [c['command'] for c in tool_usage['bash_commands']][:10],
        "has_security_check": any('security' in str(s).lower() for s in tool_usage['grep_searches']),
        "has_performance_check": any('performance' in str(s).lower() for s in tool_usage['grep_searches'])
    }
```

#### 1.3 Concept Extraction
```python
def extract_concepts(text, tool_usage):
    """Extract high-level concepts from conversation and tool usage."""
    concepts = set()
    
    # Common development concepts
    concept_patterns = {
        'security': r'(security|vulnerability|CVE|injection|sanitize|escape)',
        'performance': r'(performance|optimization|speed|memory|efficient)',
        'testing': r'(test|pytest|unittest|coverage|TDD)',
        'docker': r'(docker|container|compose|dockerfile)',
        'api': r'(API|REST|GraphQL|endpoint|webhook)',
        'database': r'(database|SQL|query|migration|schema)',
        'authentication': r'(auth|login|token|JWT|session)',
        'debugging': r'(debug|error|exception|traceback|log)',
        'refactoring': r'(refactor|cleanup|improve|restructure)',
        'deployment': r'(deploy|CI/CD|release|production)'
    }
    
    # Check text content
    for concept, pattern in concept_patterns.items():
        if re.search(pattern, text, re.IGNORECASE):
            concepts.add(concept)
    
    # Check tool usage
    all_tool_text = str(tool_usage).lower()
    for concept, pattern in concept_patterns.items():
        if re.search(pattern, all_tool_text, re.IGNORECASE):
            concepts.add(concept)
    
    return concepts
```

### Phase 2: Search Enhancement

#### 2.1 Update MCP Server Search
```python
# mcp-server/src/server.py

@mcp.tool()
async def search_by_file(
    ctx: Context,
    file_path: str,
    limit: int = 10
) -> str:
    """Search for conversations that analyzed a specific file."""
    # Normalize file path
    normalized_path = normalize_path(file_path)
    
    # Search with file filter
    results = await search_with_filter(
        filter=Filter(
            must=[
                FieldCondition(
                    key="files_analyzed",
                    match=MatchAny(any=[normalized_path])
                )
            ]
        ),
        limit=limit
    )
    
    return format_file_search_results(results, file_path)

@mcp.tool()
async def search_by_concept(
    ctx: Context,
    concept: str,
    include_files: bool = True,
    limit: int = 10
) -> str:
    """Search for conversations about a specific concept."""
    # First, do semantic search for the concept
    query_embedding = await generate_embedding(concept)
    
    # Then filter by concept tag if available
    results = await qdrant_client.search(
        collection_name=get_collection_name(),
        query_vector=query_embedding,
        query_filter=Filter(
            should=[
                FieldCondition(
                    key="concepts",
                    match=MatchAny(any=[concept.lower()])
                )
            ]
        ),
        limit=limit,
        with_payload=True
    )
    
    return format_concept_results(results, include_files)
```

### Phase 3: Testing & Validation

#### 3.1 Test Import Script
```bash
# Test with last 2 days of data to avoid full re-import
python scripts/test-enhanced-import.py --days 2 --limit 10

# Verify tool extraction
python scripts/verify-tool-extraction.py /path/to/test.jsonl
```

#### 3.2 Test Search Capabilities
```python
# Test file-based search
mcp__claude-self-reflect__search_by_file("/mcp-server/src/server.py")

# Test concept search
mcp__claude-self-reflect__search_by_concept("security")

# Test combined search
mcp__claude-self-reflect__reflect_on_past("security audit server.py")
```

### Phase 4: Deployment

#### 4.1 Update Reflection Agent
Create specialized prompts for the reflection agent to use new capabilities:
```markdown
# .claude/agents/reflection-specialist/README.md

## New Capabilities
- Search by specific files: `search_by_file("/path/to/file.py")`
- Search by concepts: `search_by_concept("security")`
- Find analysis-only conversations: Use `analysis_only: true` filter
```

#### 4.2 Performance Monitoring
```python
# Add timing logs to import script
import time

def import_with_timing(jsonl_file):
    start_time = time.time()
    
    # Extract phase
    extract_start = time.time()
    tool_usage = extract_tool_usage_from_jsonl(jsonl_file)
    extract_time = time.time() - extract_start
    
    # Chunk phase
    chunk_start = time.time()
    chunks = create_enhanced_chunks(messages, tool_usage)
    chunk_time = time.time() - chunk_start
    
    # Embed phase
    embed_start = time.time()
    embeddings = generate_embeddings(chunks)
    embed_time = time.time() - embed_start
    
    # Store phase
    store_start = time.time()
    store_in_qdrant(chunks, embeddings)
    store_time = time.time() - store_start
    
    total_time = time.time() - start_time
    
    logger.info(f"Import timing for {jsonl_file}:")
    logger.info(f"  Extract: {extract_time:.2f}s")
    logger.info(f"  Chunk: {chunk_time:.2f}s")
    logger.info(f"  Embed: {embed_time:.2f}s")
    logger.info(f"  Store: {store_time:.2f}s")
    logger.info(f"  Total: {total_time:.2f}s")
```

## Current Import Logging

### Do We Currently Log Import Timing?
**No**, the current import scripts do not log detailed timing information. They only log:
- Basic progress (which file is being processed)
- Number of chunks imported
- Errors if they occur

This is a gap we should address in the enhanced version.

## Additional JSONL Data to Extract

### 1. Timing Information
```python
# Track how long each tool operation took
"timing": {
    "Read": {"count": 5, "total_ms": 234},
    "Grep": {"count": 2, "total_ms": 1502}
}
```

### 2. Error Tracking
```python
# Track failed operations
"errors": [
    {"tool": "Read", "error": "File not found", "path": "/missing.py"}
]
```

### 3. Result Summaries
```python
# Store grep match counts, test results, etc.
"tool_results": {
    "grep_matches": {"security": 15, "injection": 0},
    "tests_run": {"passed": 48, "failed": 2}
}
```

### 4. Context Flow
```python
# Track conversation flow
"context_flow": [
    {"step": 1, "action": "read_file", "target": "server.py"},
    {"step": 2, "action": "search", "query": "authentication"},
    {"step": 3, "action": "analyze", "finding": "potential issue"}
]
```

### 5. Tool Result Data
```python
# Extract actual results from tool responses
"tool_results": {
    "files_found": ["auth.py", "login.py"],  # From Glob results
    "grep_files_matched": 15,  # From Grep results
    "test_output": "48 passed, 2 failed",  # From Bash pytest
    "git_status": "modified"  # From git commands
}
```

### 6. Code Blocks and Language
```python
# Extract code blocks from conversations
"code_blocks": [
    {
        "language": "python",
        "purpose": "example",  # example, fix, suggestion
        "length": 45,  # lines
        "functions": ["authenticate", "validate_token"]
    }
]
```

### 7. Session Metadata
```python
# Extract session-level information
"session_info": {
    "git_branch": "main",
    "working_directory": "/projects/claude-self-reflect",
    "session_duration_ms": 45000,
    "total_tool_calls": 23,
    "user_interruptions": 2
}
```

### 8. Dependency Tracking
```python
# Track relationships between files
"file_relationships": [
    {"from": "server.py", "to": "auth.py", "type": "import"},
    {"from": "test_auth.py", "to": "auth.py", "type": "tests"}
]
```

## Task List

### Implementation Checklist (Single Day Deployment)

#### Morning: Core Development
- [ ] Implement extract_tool_usage_from_jsonl function
- [ ] Test extraction with 2-day subset of conversations
- [ ] Update chunk creation to include tool usage metadata
- [ ] Create enhanced import script (import-conversations-enhanced.py)
- [ ] Add concept detection logic for common development topics
- [ ] Add timing and performance logging to import script

#### Afternoon: Integration & Testing
- [ ] Update MCP server with search_by_file and search_by_concept tools
- [ ] Remove and re-add MCP with new capabilities
- [ ] Test with both local FastEmbed and Voyage embeddings
- [ ] Performance test with full dataset
- [ ] Update reflection agent documentation

#### Evening: Release
- [ ] Final testing with production data
- [ ] Create v2.5.0 GitHub release
- [ ] Update npm package and publish
- [ ] Write announcement with examples
- [ ] Update main README with new features

### Critical Path & Dependencies

1. **Must Complete First**:
   - extract_tool_usage_from_jsonl (blocks everything else)
   - Test with 2-day subset (validates extraction logic)

2. **Can Parallelize**:
   - Enhanced import script + concept detection
   - MCP server updates (can develop while import runs)

3. **Sequential Requirements**:
   - Test extraction → Enhanced import → Full import
   - MCP updates → Remove/re-add MCP → Test tools
   - All testing → GitHub release → npm publish

### Quick Test Commands
```bash
# Test extraction only (no import)
python -c "from enhanced_import import extract_tool_usage_from_jsonl; print(extract_tool_usage_from_jsonl('test.jsonl'))"

# Test 2-day import
python scripts/import-conversations-enhanced.py --days 2 --test-mode

# Verify enhanced search
claude mcp list  # Ensure connected
# Then in Claude: search_by_file("/path/to/file.py")
```

## Success Metrics
- Import performance: <10% slower than current
- Search latency: <100ms additional overhead
- Storage increase: <2x current size
- Query accuracy: Can find files from abstract concepts
- User value: "Which files did I analyze?" queries work

## Rollback Plan
If issues arise:
1. Keep original import script as fallback
2. Store enhanced metadata in separate collection
3. Feature flag for new search capabilities
4. Gradual rollout to test users first

## Future Enhancements
1. **Code snippet extraction**: Store actual code blocks discussed
2. **Dependency graphs**: Track which files commonly appear together
3. **Time-based analysis**: "What did I work on last week?"
4. **Pattern learning**: Identify common investigation patterns
5. **Integration with Code-Context MCP**: Unified code + conversation search