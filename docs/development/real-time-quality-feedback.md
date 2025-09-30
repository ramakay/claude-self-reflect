# Real-Time Quality Feedback Feature

## Overview
The Real-Time Quality Feedback feature provides immediate code quality analysis when Claude or any AI agent edits code files. It uses the AST-GREP pattern registry to detect anti-patterns and quality issues, providing actionable feedback directly in the Claude interface.

## How It Works

### Architecture
1. **PostToolUse Hook**: Triggers after Edit/Write/MultiEdit operations
2. **AST-GREP Analysis**: Runs pattern matching against unified registry (105+ patterns)
3. **Quality Scoring**: Calculates quality percentage based on good vs bad patterns
4. **Feedback Delivery**: Uses JSON output with `decision: block` to show feedback

### Key Components

#### Hook Configuration (`.claude/settings.json`)
```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Edit|Write|MultiEdit|NotebookEdit",
        "hooks": [{
          "type": "command",
          "command": "$CLAUDE_PROJECT_DIR/.claude/hooks/quality-check.py",
          "timeout": 3
        }]
      }
    ]
  }
}
```

#### Quality Check Script (`.claude/hooks/quality-check.py`)
- Reads PostToolUse event data from stdin
- Extracts edited file path
- Runs AST-GREP analyzer on the file
- If quality score < 70%, outputs formatted feedback
- Uses JSON output format for visibility

## Features

### Supported Languages
- Python (.py)
- TypeScript (.ts, .tsx)
- JavaScript (.js, .jsx)

### Quality Patterns Detected
- **Anti-patterns**: Print statements, bare except, global variables
- **Complexity Issues**: Nested if statements (3+ levels), nested loops
- **Code Smells**: Long functions, complex conditions
- **Best Practices**: Proper error handling, logging usage

### Feedback Format
When quality issues are detected:
```
Code quality check failed for [filename]:
• Quality Score: XX.X% (threshold: 70%)

Top issues to fix:
• Replace print statements with logger
• Refactor deeply nested if statements
• Optimize nested loops for performance

Please fix these quality issues before proceeding.
```

## Configuration

### Quality Threshold
Default: 70% - Files with quality scores below this trigger feedback

To adjust, modify line 72 in `.claude/hooks/quality-check.py`:
```python
if has_critical or has_issues or (quality_score and quality_score < 70):
```

### Timeout
Default: 3 seconds - Maximum time for analysis

Adjust in `.claude/settings.json`:
```json
"timeout": 3
```

## Testing

### Manual Testing
```bash
# Test the analyzer directly
./venv/bin/python scripts/ast_grep_final_analyzer.py test_file.py

# Test the hook
echo '{"tool_name": "Edit", "tool_input": {"file_path": "/path/to/file.py"}}' | \
  ./venv/bin/python .claude/hooks/quality-check.py
```

### Verification Checklist
- ✅ Hook triggers on Edit/Write/MultiEdit operations
- ✅ Quality feedback appears for files with <70% score
- ✅ No feedback for files with >70% score
- ✅ Handles syntax errors gracefully
- ✅ Works across Python, TypeScript, JavaScript

## Troubleshooting

### Feedback Not Visible
1. Ensure hook script is executable: `chmod +x .claude/hooks/quality-check.py`
2. Check Claude settings: `/hooks` command
3. Verify ast-grep-py is installed: `pip install ast-grep-py`
4. Run Claude with debug: `claude --debug`

### Hook Not Triggering
1. Restart Claude Code after modifying settings
2. Check file extensions are supported (.py, .ts, .js, etc.)
3. Verify project has venv with ast-grep-py installed

## Implementation Notes

### Key Discovery
PostToolUse hooks with stdout are NOT visible to Claude. The solution uses JSON output with `decision: "block"` to make feedback visible to both Claude and the user.

### Pattern Registry
- 105+ patterns across Python, TypeScript, JavaScript
- Weighted scoring system
- Unified registry in `scripts/unified_registry.json`

## Future Enhancements
- Add support for more languages (Go, Rust, Java)
- Configurable per-project quality thresholds
- Integration with project-specific linting rules
- Automatic fix suggestions for common patterns