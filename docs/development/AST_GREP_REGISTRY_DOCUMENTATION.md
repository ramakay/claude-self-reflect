# AST-GREP Unified Registry Documentation

## Overview
The AST-GREP Unified Registry is a comprehensive pattern management system that combines custom AST patterns with the official AST-GREP catalog for automated code quality analysis and fixes.

## System Architecture

### Core Components

#### 1. UnifiedASTGrepRegistry Class
**Location**: `scripts/ast_grep_unified_registry.py`
**Purpose**: Central pattern registry combining multiple sources

**Key Features**:
- Loads custom Python patterns
- Integrates official AST-GREP catalog
- Supports TypeScript/JavaScript patterns
- Dynamic pattern merging
- JSON persistence

**Pattern Sources**:
1. Static patterns defined in Python code
2. Official catalog from `unified_registry.json`
3. Custom project-specific patterns

#### 2. Pattern Storage
**Location**: `scripts/unified_registry.json`
**Format**: JSON with categorized patterns

```json
{
  "patterns": {
    "python-code-smells": [...],
    "python-security": [...],
    "python-performance": [...],
    "python-testing": [...],
    "typescript-patterns": [...]
  },
  "metadata": {
    "version": "1.0",
    "updated": "2025-09-17",
    "source": "unified"
  }
}
```

## Pattern Categories

### Python Patterns

#### 1. Code Smells (`python-code-smells`)
Detects anti-patterns and poor practices:
- Mutable default arguments
- Bare except clauses
- Unused variables
- Complex nested structures
- Magic numbers
- Long parameter lists

#### 2. Security (`python-security`)
Identifies security vulnerabilities:
- SQL injection risks
- Command injection
- Path traversal
- Insecure randomness
- Hardcoded credentials
- Unsafe deserialization

#### 3. Performance (`python-performance`)
Finds performance bottlenecks:
- Inefficient loops
- Unnecessary list comprehensions
- String concatenation in loops
- Repeated computations
- Missing caching opportunities

#### 4. Testing (`python-testing`)
Improves test quality:
- Missing assertions
- Test without cleanup
- Hardcoded test data
- Flaky time-based tests
- Missing edge cases

### TypeScript/JavaScript Patterns

#### 1. React Patterns
- Unused state variables
- Missing useEffect dependencies
- Direct state mutations
- Missing key props
- Inefficient re-renders

#### 2. General TypeScript
- Any type usage
- Missing null checks
- Implicit any returns
- Unused imports
- Console.log statements

## Pattern Structure

### AST Pattern Format
Each pattern follows the AST-GREP specification:

```yaml
id: pattern-unique-id
language: python|typescript|javascript
message: Description of the issue
severity: critical|high|medium|low|info
rule:
  pattern: |
    AST pattern to match
fix: |
  Replacement pattern (optional)
metadata:
  category: code-smell|security|performance|testing
  tags: [tag1, tag2]
```

### Example Pattern
```python
{
  "id": "mutable-default-arg",
  "pattern": "def $FUNC($$$ARGS, $PARAM = []):\n  $$$BODY",
  "message": "Mutable default argument detected",
  "severity": "warning",
  "fix": "def $FUNC($$$ARGS, $PARAM = None):\n  if $PARAM is None:\n    $PARAM = []\n  $$$BODY",
  "language": "python"
}
```

## Integration Points

### 1. Quality Analysis
**Script**: `scripts/ast_grep_final_analyzer.py`
- Uses registry patterns for analysis
- Generates quality reports
- Calculates severity scores

### 2. Automated Fixes
**Agent**: `quality-fixer`
- Applies safe pattern fixes
- Validates changes
- Runs regression tests

### 3. Session Tracking
**Script**: `scripts/session_quality_tracker.py`
- Tracks quality in current session
- Uses registry for real-time analysis
- Updates quality metrics

### 4. Pre-commit Hooks
**Hook**: `.claude/hooks/pre-commit`
- Runs pattern analysis before commits
- Updates quality cache
- Prevents quality degradation

## Pattern Management

### Adding Custom Patterns

#### Method 1: Python Code
Add to `_load_unified_patterns()` method:
```python
def _load_unified_patterns(self):
    patterns = {
        "custom-category": [
            {
                "id": "custom-pattern",
                "pattern": "AST pattern here",
                "message": "Issue description",
                "severity": "high",
                "fix": "Replacement pattern"
            }
        ]
    }
```

#### Method 2: JSON File
Update `unified_registry.json`:
```json
{
  "patterns": {
    "custom-category": [
      {
        "id": "custom-pattern",
        ...
      }
    ]
  }
}
```

### Pattern Validation
Patterns are validated for:
- Unique IDs within categories
- Valid AST syntax
- Appropriate severity levels
- Optional fix patterns

### Pattern Testing
Test patterns using:
```bash
# Test single pattern
ast-grep --pattern 'def $FUNC($$$ARGS, $PARAM = [])' file.py

# Test with fix
ast-grep --pattern 'pattern' --fix 'replacement' file.py
```

## Quality Scoring

### Severity Weights
- **Critical**: 10 points
- **High**: 5 points
- **Medium**: 3 points
- **Low**: 1 point
- **Info**: 0.5 points

### Score Calculation
```python
total_score = sum(severity_weight * count for each issue)
normalized_score = total_score / lines_of_code * 100
```

### Quality Thresholds
Quality scores are normalized per 100 lines of code. Thresholds represent issues per 100 LOC:
- **Excellent**: Score < 5 (fewer than 5 weighted issues per 100 lines)
- **Good**: Score < 10 (5-10 weighted issues per 100 lines)
- **Fair**: Score < 20 (10-20 weighted issues per 100 lines)
- **Poor**: Score >= 20 (20+ weighted issues per 100 lines)

Example: A 500-line file with score 8 has ~40 weighted issues total (8 * 5)

## Automation Features

### 1. Auto-fix Application
The quality-fixer agent can automatically:
- Apply safe fixes (severity <= medium)
- Create backup before changes
- Validate fixes with tests
- Rollback on failure

### 2. Batch Processing
Process multiple files:
```python
registry = UnifiedASTGrepRegistry()
for file in project_files:
    issues = registry.analyze(file)
    registry.apply_fixes(file, issues)
```

### 3. CI/CD Integration
```yaml
# GitHub Actions example
- name: Run AST-GREP Analysis
  run: |
    python scripts/ast_grep_final_analyzer.py
    python scripts/quality-gate.py --threshold 10
```

## Performance Optimization

### Caching Strategy
- Pattern compilation cached in memory
- Analysis results cached per file
- Session-based incremental analysis

### Parallel Processing
- Multi-threaded file analysis
- Batched pattern matching
- Async I/O for large projects

## Troubleshooting

### Common Issues

#### Pattern Not Matching
- Verify AST structure with `ast-grep --debug`
- Check language specification
- Test pattern in isolation

#### Fix Not Applying
- Ensure fix pattern matches replacement context
- Check for variable capture groups
- Validate replacement syntax

#### Performance Issues
- Enable pattern caching
- Use file filtering
- Run incremental analysis

### Debug Mode
Enable verbose output:
```python
registry = UnifiedASTGrepRegistry()
registry.debug = True
registry.analyze(file_path)
```

## Best Practices

### 1. Pattern Design
- Keep patterns specific but flexible
- Use variable capture for reusability
- Provide helpful error messages
- Include fix patterns when safe

### 2. Category Organization
- Group related patterns
- Use consistent naming
- Document pattern intent
- Tag for searchability

### 3. Fix Safety
- Only auto-fix low-risk patterns
- Always validate after fixes
- Maintain rollback capability
- Log all changes

### 4. Performance
- Cache compiled patterns
- Use incremental analysis
- Filter unnecessary files
- Batch operations

## Command Line Interface

### Basic Usage
```bash
# Analyze current directory
python scripts/ast_grep_final_analyzer.py

# Analyze specific file
python scripts/ast_grep_final_analyzer.py --file path/to/file.py

# Apply fixes
python scripts/ast_grep_final_analyzer.py --fix

# Custom threshold
python scripts/ast_grep_final_analyzer.py --threshold 15
```

### Advanced Options
```bash
# Use custom patterns
--patterns custom_patterns.json

# Output format
--format json|markdown|html

# Severity filter
--severity critical,high

# Category filter
--category security,performance
```

## Integration with Claude

### Automatic Invocation
- Post-generation hook triggers analysis
- Pre-commit hook updates quality
- Quality-fixer agent applies fixes

### Manual Commands
```
/fix-quality  # Run quality fixer agent
/analyze     # Run AST-GREP analysis
/patterns    # List available patterns
```

## Future Enhancements

### Planned Features
1. **Machine Learning Integration**: Learn patterns from codebase
2. **Custom Rule Builder**: GUI for creating patterns
3. **Cross-language Patterns**: Unified patterns across languages
4. **Smart Fix Suggestions**: Context-aware fix generation
5. **Team Collaboration**: Shared pattern libraries

### Pattern Evolution
- Community-contributed patterns
- Industry-specific rule sets
- Framework-specific patterns
- Security vulnerability database integration

## Version History
- v1.0: Initial unified registry
- v1.1: Added TypeScript patterns
- v1.2: Integrated official catalog
- v1.3: Added auto-fix capabilities
- v1.4: Performance optimizations and caching