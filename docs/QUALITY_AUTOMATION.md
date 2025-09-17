# Quality Automation System

## Overview

Claude Self-Reflect includes a comprehensive code quality automation system that integrates AST-GREP pattern matching with automatic analysis, real-time monitoring, and safe auto-fixing capabilities.

## Components

### 1. AST-GREP Pattern Registry (`unified_registry.json`)

The system uses 96+ patterns across multiple languages to detect:
- **Complexity Issues**: Nested if statements, complex conditions, callback hell
- **Antipatterns**: Console.log, debugger statements, bare except blocks
- **Good Patterns**: Proper error handling, type annotations, async/await usage

#### Complexity Patterns (NEW)
We've added specialized patterns to detect cyclomatic complexity indicators:
- Deeply nested if statements (3+ levels)
- Complex conditionals (4+ AND/OR operators)
- Nested loops (performance risks)
- Multiple elif/switch branches
- Long functions (10+ statements)

### 2. Automated Analysis Integration

#### Streaming Watcher (`streaming-watcher.py`)
- Analyzes HOT files (recently edited) in real-time
- Runs AST-GREP on files mentioned in conversations
- Adds quality metadata to Qdrant points
- Updates quality cache for statusline display

#### Import Script (`import-conversations-unified.py`)
- Extracts tool usage and file references
- Runs quality analysis on edited/analyzed files
- Stores pattern analysis in conversation metadata
- Calculates average quality scores

### 3. Real-time Monitoring

#### Post-Generation Hook (`.claude/hooks/post-generation`)
- Triggers after Claude generates responses
- Analyzes recently edited files
- Provides immediate quality feedback
- Non-blocking for optimal performance

#### Statusline Integration (`csr-status`)
- Shows quality indicators: 🔴🟠🟡🟢✅✨
- Categorizes issues by severity
- Updates in real-time from quality cache
- Format: `[100%][🟠:8·228]` (8 critical, 228 total)

### 4. Safe Auto-Fix System

#### `/fix-quality` Slash Command
Safely fixes code quality issues with regression testing:

```bash
/fix-quality --file src/main.py --severity medium
```

#### Quality-Fixer Subagent
The specialized subagent that:
1. **Runs baseline tests** - Ensures tests pass before fixing
2. **Applies fixes incrementally** - One at a time
3. **Tests after each fix** - Immediate regression detection
4. **Reverts on failure** - Automatic rollback
5. **Reports results** - Detailed summary

#### Safety Guarantees
- **No fixes without tests** - Refuses to proceed without test suite
- **Incremental application** - Single fix at a time
- **Immediate validation** - Tests run after each change
- **Automatic reversion** - Rollback on test failure
- **Safe patterns only** - Conservative fix selection

## Pattern Categories

### Python
- `python_complexity` - Cyclomatic complexity indicators
- `python_antipatterns` - Common bad practices
- `python_error_handling` - Exception handling patterns
- `python_logging` - Print vs logger usage
- `python_typing` - Type annotation patterns

### TypeScript/JavaScript
- `typescript_complexity` - Nested callbacks, complex logic
- `typescript_antipatterns` - Console.log, any type, var usage
- `javascript_complexity` - Similar to TypeScript
- `javascript_antipatterns` - Common JS issues

## Quality Scoring Algorithm

The system uses a penalty-based scoring approach:

```python
def calculate_quality_score(issues, loc):
    kloc = max(1.0, loc / 1000.0)
    total_penalty = sum(abs(issue.weight) * issue.count for issue in issues)
    issues_per_kloc = total_penalty / kloc
    penalty = min(0.7, 0.15 * math.log1p(issues_per_kloc))
    score = max(0.0, min(1.0, 1.0 - penalty))
    return score
```

## Usage Examples

### Manual Quality Check
```bash
python scripts/session_quality_tracker.py --project-path . --project-name myproject
```

### View Current Status
```bash
python3 scripts/csr-status
```

### Run Quality Fixes
```bash
# Fix issues in specific file
/fix-quality --file src/main.py

# Dry run to see what would be fixed
/fix-quality --dry-run

# Fix only high-severity issues
/fix-quality --severity high
```

## Configuration

### Adding Custom Patterns
Edit `scripts/unified_registry.json` to add patterns:

```json
{
  "id": "custom-pattern",
  "pattern": "your AST-GREP pattern",
  "description": "What this detects",
  "quality": "bad",
  "weight": -3,
  "severity": "medium",
  "language": "python",
  "category": "python_custom"
}
```

### Adjusting Thresholds
Modify severity thresholds in `scripts/csr-status`:
- Critical: weight <= -4
- Medium: weight -2 to -3
- Low: weight >= -1

## Integration Points

1. **Import Pipeline**: Automatic during conversation import
2. **Streaming Watcher**: Real-time for HOT files
3. **Post-Generation Hook**: After Claude edits
4. **Statusline**: Continuous display
5. **Slash Commands**: On-demand fixes

## Performance Considerations

- Pattern matching: ~13ms per chunk
- Quality analysis: <100ms for 10 files
- Hook execution: Non-blocking, <2s
- Auto-fix: Depends on test suite speed

## Future Enhancements

- [ ] Machine learning-based pattern discovery
- [ ] Cross-file complexity analysis
- [ ] Historical quality trends
- [ ] Team quality metrics dashboard
- [ ] Custom fix strategies per project

## Troubleshooting

### No Quality Score Showing
- Check if AST-GREP is installed: `which ast-grep`
- Verify patterns loaded: Check `unified_registry.json`
- Check cache: `~/.claude-self-reflect/quality_cache.json`

### Fixes Not Working
- Ensure test suite exists and passes
- Check subagent is registered: `/agents`
- Verify file has fixable patterns
- Review test output for failures

### Performance Issues
- Reduce pattern count for large files
- Adjust cache TTL in analyzers
- Disable real-time analysis if needed