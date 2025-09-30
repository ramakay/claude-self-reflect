# Import Script Refactoring - Baseline Metrics

## Date: 2025-09-25
## Branch: refactor/import-conversations-complexity

## Current Complexity Analysis

### High Complexity Functions

#### 1. `extract_metadata_single_pass` (Line 315)
- **Cyclomatic Complexity**: 49 (CRITICAL)
- **Lines of Code**: 173 lines
- **Main Issues**:
  - Deeply nested conditionals (4+ levels)
  - Multiple responsibilities in single function
  - Complex message type handling
  - Mixed extraction logic for different content types
  - Inline AST-GREP analysis

#### 2. `stream_import_file` (Line 489)
- **Cyclomatic Complexity**: 41 (CRITICAL)
- **Lines of Code**: 193 lines
- **Main Issues**:
  - Complex streaming logic with multiple decision points
  - Multiple message format handlers
  - Inline error handling throughout
  - Mixed concerns (streaming, parsing, uploading, cleanup)
  - Duplicate code for chunk processing

### Other Functions
- `process_and_upload_chunk`: ~100 lines, moderate complexity
- `extract_ast_elements`: ~38 lines, moderate complexity
- `extract_concepts`: ~25 lines, low complexity
- `ensure_collection`: ~10 lines, low complexity

## File Statistics
- **Total Lines**: 887 lines
- **Total Functions**: 13
- **Average Function Length**: 68 lines
- **Maximum Function Length**: 193 lines (stream_import_file)

## Performance Baseline
(To be measured with test imports)

### Import Speed
- Small file (100 messages): TBD
- Medium file (1000 messages): TBD
- Large file (5000 messages): TBD

### Memory Usage
- Peak memory during import: TBD
- Memory per message: TBD

## Refactoring Targets

### Primary Goals
1. Reduce maximum cyclomatic complexity from 49 to <10
2. Extract message processing into separate classes
3. Implement Strategy pattern for import strategies
4. Create service abstractions for embeddings
5. Maintain 100% backward compatibility
6. No performance degradation (±5%)

### Success Metrics
- [ ] All functions < 10 complexity
- [ ] 90%+ test coverage
- [ ] CSR tester passes all checks
- [ ] Performance within 5% of baseline