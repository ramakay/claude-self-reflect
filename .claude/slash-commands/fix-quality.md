# /fix-quality Slash Command

## Purpose
Automatically fix common code quality issues using AST-GREP patterns with regression testing to ensure safety.

## Usage
```
/fix-quality [options]
```

## Options
- `--file <path>` - Fix issues in specific file
- `--project` - Fix issues in entire project (limited to 10 files)
- `--dry-run` - Show what would be fixed without making changes
- `--severity <level>` - Only fix issues of specified severity (low/medium/high)

## How It Works

The command delegates to the `quality-fixer` subagent which:
1. **Establishes baseline** - Runs existing tests to ensure they pass
2. **Applies fixes incrementally** - One fix at a time
3. **Tests after each fix** - Immediately runs regression tests
4. **Reverts on failure** - Automatically undoes changes that break tests
5. **Reports results** - Shows what was fixed and what couldn't be fixed

## Safety Features
- **No fixes without tests** - Refuses to proceed if no test suite exists
- **Incremental approach** - Applies one fix at a time with testing
- **Automatic reversion** - Reverts any fix that causes test failures
- **Safe patterns only** - Only fixes well-understood, safe patterns

## Fixable Issues

### Python
1. **Remove print statements**
   - Pattern: `print($$$)`
   - Fix: Remove line

2. **Remove debug prints**
   - Pattern: `print(f$$$)`
   - Fix: Remove line

3. **Fix bare except**
   - Pattern: `except:`
   - Fix: `except Exception:`

4. **Remove unused imports**
   - Pattern: `import $MODULE` (if not used)
   - Fix: Remove line

### TypeScript/JavaScript
1. **Remove console.log**
   - Pattern: `console.log($$$)`
   - Fix: Remove line

2. **Remove debugger statements**
   - Pattern: `debugger`
   - Fix: Remove line

3. **Replace var with let**
   - Pattern: `var $VAR = $$$`
   - Fix: `let $VAR = $$$`

4. **Remove alert calls**
   - Pattern: `alert($$$)`
   - Fix: Remove line

5. **Fix empty catch blocks**
   - Pattern: `catch ($ERR) { }`
   - Fix: `catch ($ERR) { console.error($ERR); }`

## Implementation

The command will:
1. Run AST-GREP to detect fixable issues
2. Apply safe, automated fixes
3. Show a summary of changes
4. Optionally commit changes with descriptive message

## Safety Features
- Only fixes well-defined, safe patterns
- Creates backup before making changes
- Shows preview in dry-run mode
- Limits scope to prevent large-scale changes
- Skips files with syntax errors

## Example Output
```
🔧 Fix Quality Report
━━━━━━━━━━━━━━━━━━━
Files analyzed: 5
Issues found: 12
Issues fixed: 8

Fixed:
✅ Removed 3 console.log statements
✅ Replaced 2 var declarations with let
✅ Fixed 2 empty catch blocks
✅ Removed 1 debugger statement

Skipped:
⚠️ 4 issues require manual review (complex patterns)

Run 'git diff' to review changes
```