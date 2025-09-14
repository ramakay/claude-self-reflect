# MANDATORY AST-GREP + Pattern Registry Analysis

**File**: /Users/ramakrishnanannaswamy/projects/claude-self-reflect/mcp-server/src/server.py
**Engine**: ast-grep-py + pattern_registry.py
**Timestamp**: 2025-09-14T10:31:26.900674

## Pattern Registry Statistics
- **Total Patterns from Registry**: 23
- **Patterns Successfully Matched**: 1
- **Patterns with Errors**: 2
- **Categories Found**: testing

## Quality Summary
- **Quality Score**: 🟢 100.0%
- **Good Patterns**: 5
- **Bad Patterns**: 0

## Matches by Category

### testing (1 patterns)
- **async-test**: 5 instances
  - Line 224: `async def get_import_stats():
    """Cur...`

## Pattern Conversion Issues
Some regex patterns from registry need AST conversion:
- specific-exception (error.handling)
- useState-hook (react.hooks)

## Enforcement
✅ Using pattern_registry.py - NO hardcoded patterns
✅ Using ast-grep-py - NO regex matching
✅ MANDATORY components only - NO fallbacks