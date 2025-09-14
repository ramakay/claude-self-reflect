# Code Quality Monitoring System

## Overview
The Claude Self-Reflect statusline shows real-time code quality metrics in the format:
`[100%][🟡:B/53]`

- **[100%]** - Percentage of conversations indexed
- **[🟡:B/53]** - Quality grade (B) with issue count (53)

## Understanding Your Grade

### What the Numbers Mean
- **53 issues** = Specific code quality issues found in recently edited files
- **B grade** = Good quality, but room for improvement
- Issues are from **YOUR current project files**, not global

### Grade Thresholds
- **A+** (🟢): < 10 issues - Excellent code quality
- **A** (🟢): < 25 issues - Very good quality
- **B** (🟡): < 50 issues - Good quality
- **C** (🟡): < 100 issues - Needs improvement
- **D** (🔴): 100+ issues - Significant issues

## How to Improve Your Grade

### 1. Generate Quality Report
```bash
cd claude-self-reflect
source venv/bin/activate
python scripts/quality-report.py
```

This shows:
- Exact files with issues
- Specific problems to fix
- Commands to auto-fix common issues
- Path to reach A+ grade

### 2. Common Quick Fixes

#### Replace print statements with logging (most common issue):
```bash
# Auto-fix print statements in a file
sed -i '' 's/print(/logger.info(/g' scripts/your_file.py
```

#### Fix bare except clauses:
```python
# Bad
try:
    something()
except:
    pass

# Good
try:
    something()
except ValueError as e:
    logger.error(f"Error: {e}")
```

#### Use async file operations:
```python
# Bad
with open('file.txt', 'r') as f:
    data = f.read()

# Good
import aiofiles
async with aiofiles.open('file.txt', 'r') as f:
    data = await f.read()
```

### 3. Monitor Progress
After making fixes:
```bash
# Regenerate quality metrics
python scripts/session_quality_tracker.py

# Check new grade
csr-status --compact
```

## Automated Monitoring

### Start Background Monitor
```bash
# Run quality checks every 30 minutes
./scripts/quality-monitor.sh &

# Or run once
./scripts/quality-monitor.sh --once
```

### View Quality History
```bash
# Check monitoring logs
tail -f ~/.claude-self-reflect/quality-monitor.log

# View saved reports
ls ~/.claude-self-reflect/quality-reports/
```

## What Gets Analyzed

The system analyzes files edited in your current Claude session:
- Python files (`.py`)
- TypeScript/JavaScript (`.ts`, `.js`, `.tsx`, `.jsx`)
- Looks for 77+ code patterns (good and bad)
- Weighs issues by severity

### Good Patterns That Improve Score
- Docstrings
- Type hints
- Error handling
- List comprehensions
- Async/await usage

### Bad Patterns That Lower Score
- Print statements (use logging)
- Bare except clauses
- Global variables
- Sync file operations
- Missing docstrings

## Quick Reference

| Statusline | Meaning | Action |
|------------|---------|--------|
| `[100%][🟢:A+/5]` | Excellent! 5 minor issues | Maintain quality |
| `[100%][🟡:B/53]` | Good, 53 issues to fix | Run quality report |
| `[85% 2h][🔴:D/150]` | Behind & poor quality | Fix issues urgently |
| `[100%][⏳:...]` | Quality being calculated | Wait a moment |

## Integration with CI/CD

Add to your pre-commit hook:
```bash
#!/bin/bash
# .git/hooks/pre-commit

# Check code quality before commit
cd claude-self-reflect
source venv/bin/activate
python scripts/session_quality_tracker.py

# Get current grade
GRADE=$(cat ~/.claude-self-reflect/session_quality.json | jq -r '.summary.quality_grade')
ISSUES=$(cat ~/.claude-self-reflect/session_quality.json | jq -r '.summary.total_issues')

# Fail if below B grade
if [[ "$ISSUES" -gt 100 ]]; then
    echo "❌ Code quality too low: Grade $GRADE with $ISSUES issues"
    echo "Run: python scripts/quality-report.py"
    exit 1
fi

echo "✅ Code quality acceptable: Grade $GRADE"
```

## Tips for Maintaining A+ Grade

1. **Fix issues immediately** when statusline shows problems
2. **Run quality report** weekly to catch drift
3. **Use the monitor** to track quality over time
4. **Add to CI** to enforce standards
5. **Focus on high-impact fixes** (print → logger is easy win)

## Troubleshooting

### Quality grade not showing
```bash
# Regenerate quality data
python scripts/session_quality_tracker.py

# Check cache file exists
ls -la ~/.claude-self-reflect/session_quality.json
```

### Grade seems wrong
- Grade adjusts based on issue count
- 50+ issues automatically downgrades from A to B
- Run `python scripts/quality-report.py` for details

### Want to exclude files
Edit `scripts/session_quality_tracker.py` to add exclusions:
```python
# Skip test files or generated code
if 'test_' in file_path or 'generated' in file_path:
    continue
```