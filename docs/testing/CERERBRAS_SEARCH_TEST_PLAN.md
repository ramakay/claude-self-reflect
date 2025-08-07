# Cererbras Search Test Plan

## Overview
Comprehensive test plan to verify that conversations containing the "cererbras" typo and related content are properly searchable in both Voyage AI and FastEmbed embedding modes.

## Context Discovery
Based on analysis of conversation files, we found:

### Target Files
- **6e38221d-df4c-4c19-a1be-e19472ecbb48.jsonl**
  - Contains test script references to "cererbras" 
  - Context: Testing search for 'cererbras' (with typo) content
  
- **d7f32965-9749-4fae-9b94-df83284537b6.jsonl** 
  - Contains actual conversation about "openrouter/cererbras"
  - Context: "claude code router supports using other LLM'swtih claude i used openorouter/cererbras"

### Key Search Terms
- **cererbras** (exact typo)
- **Cerebras** (correct spelling - should match semantically)  
- **openrouter** (service provider)
- **claude code router** (primary tool discussed)
- **Qwen** (related model)
- **"using other LLMs with Claude"** (semantic concept)

### Expected Collections
- **conv_7f6df0fc_voyage** (Voyage AI embeddings, 1024 dimensions)
- **conv_7f6df0fc_local** (FastEmbed embeddings, 384 dimensions)

## Test Scripts

### 1. Content Verification Script
**File**: `/scripts/verify-cererbras-content.py`
- Quick check if expected content exists in collections
- Scans first 100 points for matching terms
- Provides immediate pass/fail result

**Usage**:
```bash
cd claude-self-reflect
source .venv/bin/activate
python scripts/verify-cererbras-content.py
```

### 2. Comprehensive Search Test Suite
**File**: `/scripts/test-cererbras-search.py` 
- Tests all query variations across both embedding models
- Multiple similarity thresholds and search limits
- Detailed verification of expected content matches
- Generates comprehensive test report

**Usage**:
```bash
python scripts/test-cererbras-search.py           # Full test
python scripts/test-cererbras-search.py --report  # Report only
python scripts/test-cererbras-search.py --help    # Show help
```

### 3. Threshold Optimization Script
**File**: `/scripts/optimize-search-thresholds.py`
- Finds optimal similarity thresholds for each embedding model
- Tests multiple threshold/limit combinations
- Provides configuration recommendations

**Usage**:
```bash
python scripts/optimize-search-thresholds.py
```

## Test Methodology

### Query Test Matrix
| Query Type | Query | Expected Matches | Test Purpose |
|------------|-------|------------------|--------------|
| Exact Typo | "cererbras" | cererbras, openrouter/cererbras | Direct typo matching |
| Correct Spelling | "Cerebras" | cererbras, Cerebras | Semantic similarity |
| Service Context | "openrouter" | openrouter, cererbras | Context matching |
| Tool Context | "claude code router" | claude code router, cererbras | Topic matching |
| Related Model | "Qwen" | Qwen, cererbras | Related content |
| Semantic | "using other LLMs with Claude" | claude code router, cererbras | Concept matching |

### Threshold Testing Strategy
**Voyage AI (1024 dims)**:
- Test range: 0.3 - 0.9
- Current baseline: 0.7
- Expected optimal: 0.6 - 0.8

**FastEmbed (384 dims)**:
- Test range: 0.2 - 0.8  
- Expected optimal: 0.4 - 0.6 (lower thresholds typically needed)

### Search Limit Testing
- Test limits: 5, 10, 20, 50
- Evaluate: result count, relevance, performance

## Success Criteria

### Primary Success
- [ ] Both target files imported to appropriate collections
- [ ] "cererbras" typo found with exact match search
- [ ] "Cerebras" correct spelling finds typo content (semantic match)
- [ ] All related terms (openrouter, claude code router, Qwen) found

### Secondary Success  
- [ ] Optimal thresholds identified for each embedding model
- [ ] Cross-collection search performs within acceptable latency
- [ ] Search quality metrics meet baseline expectations

### Performance Targets
- **Search Latency**: < 200ms for single collection
- **Cross-Collection Overhead**: < 100ms additional
- **Accuracy Target**: > 66.1% (current baseline)

## Execution Sequence

### Phase 1: Pre-Import Verification
1. Check current collection status
2. Verify target files exist in source location
3. Document current search performance baseline

### Phase 2: Import Execution  
1. Run import for specific target files
2. Monitor import logs for processing of target content
3. Verify collections created with expected point counts

### Phase 3: Content Verification
1. Run `verify-cererbras-content.py` 
2. Quick pass/fail check for expected content
3. If failed, debug import process

### Phase 4: Comprehensive Testing
1. Run `test-cererbras-search.py`
2. Test all query variations and configurations
3. Generate detailed test report
4. Document any failures or suboptimal results

### Phase 5: Threshold Optimization
1. Run `optimize-search-thresholds.py`
2. Find optimal settings for each embedding model
3. Update configuration recommendations
4. Re-test with optimized settings

### Phase 6: Performance Validation
1. Measure search latency under optimal settings
2. Verify cross-collection search performance
3. Compare results to baseline accuracy metrics
4. Document final recommendations

## Configuration Recommendations

### Current Defaults
```env
SIMILARITY_THRESHOLD=0.7
SEARCH_LIMIT=10  
CROSS_COLLECTION_LIMIT=5
```

### Expected Optimizations
**Voyage AI**:
```env
VOYAGE_SIMILARITY_THRESHOLD=0.65  # Likely lower for better recall
VOYAGE_SEARCH_LIMIT=15
```

**FastEmbed**:
```env
LOCAL_SIMILARITY_THRESHOLD=0.45   # Lower for 384-dim embeddings  
LOCAL_SEARCH_LIMIT=20
```

## Monitoring & Validation

### Key Metrics to Track
1. **Search Success Rate**: % of queries returning relevant results
2. **Average Similarity Score**: Quality of matches  
3. **Result Count Distribution**: Balance between precision/recall
4. **Latency Percentiles**: p50, p95, p99 search times
5. **Cross-Model Consistency**: Compare results between embedding types

### Regression Testing
After any configuration changes:
1. Re-run full test suite
2. Compare metrics to previous baseline
3. Verify no degradation in unrelated searches
4. Document any trade-offs (precision vs recall, speed vs accuracy)

## Troubleshooting Guide

### If Content Not Found
1. Check import logs for target files
2. Verify JQ filters not excluding content  
3. Check if files marked as already imported
4. Clear import state and re-run

### If Search Quality Poor
1. Lower similarity thresholds gradually
2. Increase search limits
3. Check embedding model compatibility
4. Test with manual embedding generation

### If Performance Issues
1. Check Qdrant resource usage
2. Monitor embedding generation latency
3. Profile cross-collection search overhead
4. Consider search limit optimizations

## Expected Deliverables

1. **Test Results**: JSON files with complete test metrics
2. **Configuration Recommendations**: Optimized threshold/limit settings
3. **Performance Report**: Latency and accuracy measurements  
4. **Troubleshooting Documentation**: Common issues and solutions
5. **Regression Test Suite**: Automated tests for future validation

## Integration with MCP

### MCP Search Testing
Once content verified, test via MCP interface:
```typescript
// Test MCP search functionality
mcp__claude-self-reflect__reflect_on_past("cererbras")
mcp__claude-self-reflect__reflect_on_past("claude code router")
```

### Expected MCP Behavior
- Should find typo content through semantic matching
- Should handle cross-collection search automatically
- Should apply memory decay appropriately
- Should return relevant conversation context

---

**Note**: This test plan focuses on verification phase. Import execution should be run separately before testing begins.