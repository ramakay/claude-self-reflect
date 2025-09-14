# AST-GREP Pattern Analysis Report

**File**: /Users/ramakrishnanannaswamy/projects/claude-self-reflect/mcp-server/src/server.py
**Language**: python
**Timestamp**: 2025-09-14T10:41:20.464045
**Engine**: ast-grep-py + unified registry

## Quality Overview
- **Quality Score**: 🟡 64.5%
- **Good Practices**: 26
- **Issues Found**: 31
- **Unique Patterns Matched**: 9

## Recommendations
- 🟢 Good: Code follows most best practices
- Replace 25 print statements with logger
- Fix 6 anti-patterns in python_antipatterns

## Pattern Matches by Category

### python_async (2 patterns, 12 matches)
- ⚪ **await-call**: 7 instances
  - Awaited async call
  - Example (line 226): `await update_indexing_status()...`
- ✅ **async-function**: 5 instances
  - Async function definition
  - Example (line 224): `async def get_import_stats():
    """Current impor...`

### python_logging (2 patterns, 31 matches)
- ❌ **print-call**: 25 instances
  - Print statement
  - Example (line 139): `print(f"[INFO] Embedding manager initialized: {emb...`
- ✅ **logger-call**: 6 instances
  - Logger usage
  - Example (line 321): `logger.info(f"MCP Server starting - Log file: {LOG...`

### python_typing (3 patterns, 15 matches)
- ✅ **type-annotation**: 7 instances
  - Variable type annotation
  - Example (line 178): `conversation_id: Optional[str] = None...`
- ✅ **typed-function**: 6 instances
  - Function with return type
  - Example (line 324): `def normalize_path(path_str: str) -> str:
    """N...`
- ✅ **typed-async**: 2 instances
  - Async function with return type
  - Example (line 499): `async def get_all_collections() -> List[str]:
    ...`

### python_antipatterns (2 patterns, 6 matches)
- ❌ **sync-open**: 3 instances
  - Sync file open (should use aiofiles)
  - Example (line 390): `open(path, 'r')...`
- ❌ **global-var**: 3 instances
  - Global variable usage
  - Example (line 136): `global embedding_manager, voyage_client, local_emb...`

## Pattern Registry Statistics
- **Patterns Available**: 23
- **Patterns Matched**: 9
- **Categories Found**: python_async, python_logging, python_typing, python_antipatterns

## Compliance
✅ Using unified AST-GREP registry (custom + catalog)
✅ Using ast-grep-py for AST matching
✅ NO regex patterns or fallbacks
✅ Production-ready pattern analysis