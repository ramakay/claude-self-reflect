# AST-GREP Feature Analysis for Claude Self-Reflect

## Overview
The AST-GREP feature in Claude Self-Reflect enriches conversation data with code quality metrics and pattern analysis. It uses the `ast-grep-py` library to perform structural pattern matching on code, providing deeper insights than simple text matching.

## How It Works

### 1. **Data Enrichment During Import**
- **Location**: `scripts/importer/processors/ast_extractor.py`
- **Process**: When conversations are imported, the AST extractor:
  - Extracts code blocks from conversation messages
  - Identifies programming language (Python, JavaScript, TypeScript)
  - Parses code using Python's AST module (with regex fallback for partial code)
  - Extracts structural elements: functions, classes, methods
  - Adds `ast_elements` field to conversation metadata in Qdrant

### 2. **Pattern Registry System**
- **Unified Registry**: `scripts/ast_grep_unified_registry.py`
- **Pattern Sources**:
  - Custom patterns defined in `ast_grep_pattern_registry.py`
  - Official AST-GREP catalog patterns from GitHub
  - Combined into a unified registry with 100+ patterns

### 3. **Pattern Categories**
The system analyzes code for patterns in these categories:
- **async_patterns**: Async/await usage, parallel execution
- **error_handling**: Exception handling quality
- **logging_patterns**: Logging vs print statements
- **security_patterns**: Security vulnerabilities
- **performance_patterns**: Performance optimizations
- **type_patterns**: Type hints and safety
- **testing_patterns**: Test coverage and quality

### 4. **Quality Scoring**
- **Calculator**: `ast_grep_unified_registry.calculate_quality_score()`
- **Scoring Logic**:
  - Good patterns add positive weight (e.g., +3 for proper error handling)
  - Bad patterns add negative weight (e.g., -3 for bare except clauses)
  - Score normalized by lines of code for fair comparison
  - Final score ranges from 0-100

### 5. **Session Quality Tracking**
- **Tracker**: `scripts/session_quality_tracker.py`
- **Real-time Analysis**:
  - Monitors files edited in current Claude session
  - Finds active session from `~/.claude/projects/*/`
  - Extracts edited files from JSONL conversation data
  - Runs AST-GREP analysis on edited files
  - Generates quality report with actionable insights

### 6. **Statusline Integration**
- **Location**: `~/.claude/statusline.sh`
- **Display Format**: Shows quality metrics in Claude Code's statusline
- **Quality Icons**:
  - 🟢 A+: Score 95-100 (Exceptional)
  - 🟢 A: Score 90-95 (Excellent)
  - 🟢 B: Score 80-90 (Good)
  - 🟡 C: Score 60-80 (Fair)
  - 🔴 D: Score 40-60 (Needs Improvement)
  - 🔴 F: Score 0-40 (Poor)

## Current Coverage Status

Based on my analysis of the Qdrant collections:
- **AST Elements**: Currently 0% coverage (feature may not be fully deployed)
- **Pattern Analysis**: Currently 0% coverage
- **Collections**: Using legacy naming convention (not CSR-prefixed)

## Key Components

### Core Files:
1. **AST Extractor**: `/scripts/importer/processors/ast_extractor.py`
   - Extracts AST elements from code blocks
   - Handles Python, JavaScript, TypeScript

2. **Pattern Registry**: `/scripts/ast_grep_pattern_registry.py`
   - Defines AST patterns for quality analysis
   - No regex patterns - only structural AST patterns

3. **Unified Registry**: `/scripts/ast_grep_unified_registry.py`
   - Combines custom + catalog patterns
   - Calculates quality scores

4. **Final Analyzer**: `/scripts/ast_grep_final_analyzer.py`
   - Production-ready analyzer
   - Uses ast-grep-py for pattern matching
   - Generates detailed quality reports

5. **Session Tracker**: `/scripts/session_quality_tracker.py`
   - Tracks files edited in current session
   - Provides real-time quality feedback

## How to Enable/Use

### For Import Pipeline:
```python
# The AST extractor is integrated into the import pipeline
# It automatically enriches conversations during import
python scripts/import-conversations-unified.py
```

### For Session Analysis:
```bash
# Analyze current session quality
python scripts/session_quality_tracker.py

# Run quality analysis on specific files
python scripts/ast_grep_final_analyzer.py --file path/to/file.py
```

### For Statusline:
The statusline automatically shows quality metrics if available.
Quality data is pulled from the session tracker.

## Benefits

1. **Code Quality Insights**: Identify anti-patterns and best practices
2. **Historical Analysis**: Track quality improvements over time
3. **Real-time Feedback**: See quality scores while coding
4. **Pattern Learning**: Discover common patterns in codebase
5. **Automated Reviews**: Pre-commit quality gates

## Technical Details

### AST-GREP vs Regex:
- **AST-GREP**: Structural pattern matching on syntax trees
- **Benefits**: More accurate, handles variations in formatting
- **Example**: `except $ERROR: $$$` matches any specific exception handling

### Pattern Weight System:
- Patterns have weights from -5 to +5
- Good practices: positive weights
- Anti-patterns: negative weights
- Score calculation: `sum(weight * count) / lines_of_code * 100`

## Future Enhancements

1. **Retroactive Enrichment**: Apply AST analysis to existing Qdrant data
2. **Custom Pattern Learning**: Learn patterns from high-quality code
3. **IDE Integration**: Real-time quality feedback in editor
4. **Team Metrics**: Aggregate quality metrics across team
5. **Auto-fix Suggestions**: Suggest fixes for detected issues

## Conclusion

The AST-GREP feature provides deep code quality analysis by:
- Extracting structural elements from code
- Matching against a comprehensive pattern library
- Calculating normalized quality scores
- Providing real-time feedback through the statusline

While the feature is implemented, it appears not fully deployed to production Qdrant collections yet. The infrastructure is ready for activation and retroactive enrichment of existing conversation data.