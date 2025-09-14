# AST-GREP Analysis Report (Python API)

**File**: /Users/ramakrishnanannaswamy/projects/claude-self-reflect/mcp-server/src/server.py
**Timestamp**: 2025-09-14T10:25:30.691486
**Engine**: ast-grep-py

## Summary
- **Quality Score**: 🟡 58.2%
- **Good Patterns**: 17 (3 unique)
- **Bad Patterns**: 28 (2 unique)
- **Critical Issues**: 3

## Good Patterns Found

### ✅ Function with type hints
- **Count**: 6
- **Examples**:
  - Line 324: `def normalize_path(path_str: str) -> str:
    """N...`
  - Line 499: `async def get_all_collections() -> List[str]:
    ...`
  - Line 506: `async def generate_embedding(text: str, force_type...`

### ✅ Using logger instead of print
- **Count**: 6
- **Examples**:
  - Line 321: `logger.info(f"MCP Server starting - Log file: {LOG...`
  - Line 322: `logger.info(f"Configuration: QDRANT_URL={QDRANT_UR...`
  - Line 398: `logger.debug(f"Failed to read state file {path}: {...`

### ✅ Async function definition
- **Count**: 5
- **Examples**:
  - Line 224: `async def get_import_stats():
    """Current impor...`
  - Line 237: `async def get_collection_list():
    """List of al...`
  - Line 265: `async def get_system_health():
    """System healt...`

## Anti-Patterns Detected

### ⚪ Using print instead of logger
- **Severity**: LOW
- **Count**: 25
- **Examples**:
  - Line 139: `print(f"[INFO] Embedding manager initialized: {emb...`
  - Line 149: `print(f"[ERROR] Failed to initialize embeddings: {...`
  - Line 155: `print(f"[STARTUP] MCP Server starting at {startup_...`

### 🔴 Sync file I/O in async context
- **Severity**: HIGH
- **Count**: 3
- **Examples**:
  - Line 390: `with open(path, 'r') as f:
                       ...`
  - Line 410: `with open(path, 'r') as f:
                       ...`
  - Line 432: `with open(cloud_watcher_path, 'r') as f:
         ...`

## Enforcement
✅ This analysis used ONLY ast-grep-py (Python package)
✅ No regex fallbacks or simplifications
✅ Cross-platform compatible (pip installable)