# Local Mode Configuration Issue Analysis

## Date: 2025-09-15
## Status: ⚠️ ISSUE IDENTIFIED

### Problem Summary
When attempting to switch to local mode, reflections are still being stored in the `reflections_voyage` collection instead of `reflections_local`, despite setting `PREFER_LOCAL_EMBEDDINGS="true"`.

### Root Cause Analysis

#### 1. Environment Variable Loading Order
The `run-mcp.sh` script has this loading order:
1. Stores command-line values (from MCP -e flags)
2. **Loads .env file** (which contains VOYAGE_KEY)
3. Attempts to restore command-line values

**Issue**: The .env file contains `VOYAGE_KEY=[REDACTED]`, and once this is loaded, the embedding manager initializes with Voyage client available.

#### 2. Embedding Manager Initialization
In `embedding_manager.py`:
- The manager initializes BOTH models if keys are available
- Once initialized with VOYAGE_KEY present, the voyage client remains active
- Even with `PREFER_LOCAL_EMBEDDINGS=true`, if voyage client exists, it may be used

#### 3. Runtime Behavior
- The MCP server code changes require restart to take effect
- The embedding manager doesn't re-check preferences at runtime
- Collection selection was based on `model_type` not runtime preference

### Evidence

#### Collection Counts
```
reflections_local: 32 points (384 dimensions) - NOT increasing
reflections_voyage: 10 points (1024 dimensions) - Increasing with each test
```

#### Test Results
- Stored 4+ test reflections
- ALL went to `reflections_voyage` collection
- Despite `PREFER_LOCAL_EMBEDDINGS="true"` in MCP config

### Attempted Fixes

#### 1. Modified reflection_tools.py
Added runtime checking of PREFER_LOCAL_EMBEDDINGS environment variable:
```python
prefer_local = os.getenv('PREFER_LOCAL_EMBEDDINGS', 'true').lower() == 'true'
```

#### 2. Modified run-mcp.sh
Improved handling of empty VOYAGE_KEY:
```bash
if [ "${CMDLINE_VOYAGE_KEY+x}" ]; then
    export VOYAGE_KEY="$CMDLINE_VOYAGE_KEY"
```

#### 3. Added Collection Name to Response
Modified store_reflection to show which collection was used (not working yet)

### Why Fixes Haven't Worked
1. **Code changes require MCP restart** - Python process doesn't reload modules
2. **VOYAGE_KEY from .env persists** - Even when not passed via MCP
3. **Embedding manager caches clients** - Once initialized, doesn't re-check

### Recommended Solution

#### Option 1: Prevent VOYAGE_KEY Loading in Local Mode
Modify `run-mcp.sh` to NOT load VOYAGE_KEY from .env when PREFER_LOCAL_EMBEDDINGS=true:
```bash
if [ "$CMDLINE_PREFER_LOCAL" = "true" ]; then
    unset VOYAGE_KEY
    echo "[DEBUG] Local mode requested - VOYAGE_KEY cleared" >&2
fi
```

#### Option 2: Force Model Type in Embedding Manager
Modify `embedding_manager.py` to strictly respect PREFER_LOCAL_EMBEDDINGS:
```python
if self.prefer_local:
    self.model_type = 'local'
    # Don't initialize voyage even if key exists
```

#### Option 3: Separate Environment Files
- `.env.local` - No VOYAGE_KEY
- `.env.cloud` - With VOYAGE_KEY
- Load based on mode

### Current Workaround
To truly force local mode:
1. Temporarily rename .env file
2. Restart MCP without VOYAGE_KEY in environment
3. Test and verify
4. Restore .env when cloud mode needed

### Impact on Testing
- **CSR tests require manual intervention** for mode switching
- **Restarts are currently necessary** for mode changes
- **Automated testing is blocked** without proper mode switching

### Next Steps
1. Implement Option 1 (prevent VOYAGE_KEY loading)
2. Test without .env file interference
3. Verify reflections go to correct collection
4. Update test agent with working procedure