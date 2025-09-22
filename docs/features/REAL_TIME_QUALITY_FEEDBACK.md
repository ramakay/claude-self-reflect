# Real-Time Quality Feedback for AI Agents

## Overview
The Real-Time Quality Feedback feature provides immediate code quality analysis when AI agents (Claude) edit files. This creates a feedback loop that prevents the accumulation of technical debt during AI-assisted development.

## How It Works

### Architecture
1. **PostToolUse Hook**: Triggers after Edit/Write/MultiEdit operations
2. **AST-GREP Analysis**: Runs pattern matching on edited files
3. **Quality Scoring**: Calculates quality score based on pattern matches
4. **Feedback Delivery**: Uses exit code 2 with stderr to make feedback visible to Claude

### Key Components
- `.claude/hooks/quality-check.py` - Hook script that processes tool events
- `scripts/ast_grep_final_analyzer.py` - Pattern analysis engine
- `scripts/unified_registry.json` - Pattern definitions (105 patterns)

## Pattern Categories

### Python Patterns
- **Anti-patterns**: print statements, bare except, global variables
- **Complexity**: Nested if statements, nested loops, complex conditions
- **Best Practices**: Type hints, docstrings, error handling

### TypeScript/JavaScript Patterns
- **Anti-patterns**: console.log, any type, var usage
- **Complexity**: Deep nesting, complex conditions
- **Best Practices**: Proper typing, async/await, const/let

## Quality Thresholds
- **70%+**: No feedback (good quality)
- **50-70%**: Warning with specific issues
- **<50%**: Strong warning with detailed feedback

## Configuration

### Hook Settings (`.claude/settings.json`)
```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Edit|Write|MultiEdit|NotebookEdit",
        "hooks": [
          {
            "type": "command",
            "command": "$CLAUDE_PROJECT_DIR/.claude/hooks/quality-check.py",
            "timeout": 3
          }
        ]
      }
    ]
  }
}
```

### Supported File Types
- Python (`.py`)
- TypeScript (`.ts`, `.tsx`)
- JavaScript (`.js`, `.jsx`)

## Example Feedback

When Claude edits a file with quality issues:

```
Code quality check failed for terrible_quality.py:
• Quality Score: 50.8% (threshold: 70%)

Top issues to fix:
• Replace print statements with logger
• Refactor deeply nested if statements
• Optimize nested loops for performance

Please fix these quality issues before proceeding.
```

## Benefits

### For Developers
- Prevents accumulation of technical debt
- Ensures consistent code quality
- Reduces manual code review burden
- Educates about best practices

### For AI Agents
- Immediate feedback on code quality
- Specific, actionable improvement suggestions
- Learning opportunity for better patterns
- Prevents propagation of anti-patterns

## Technical Details

### Hook Communication
The hook uses exit code 2 with stderr output to communicate with Claude:
- **Exit code 0**: Success, no issues
- **Exit code 2**: Quality issues detected, feedback sent to Claude
- **Other codes**: Errors, logged but don't block

### Performance
- Timeout: 3 seconds per analysis
- Minimal overhead: Only runs on code file edits
- Graceful degradation: Failures don't block operations

## Future Enhancements
- [ ] Configurable quality thresholds
- [ ] Custom pattern definitions per project
- [ ] Auto-fix for simple issues
- [ ] Integration with pre-commit hooks
- [ ] Quality trend tracking over sessions

## Troubleshooting

### Hook Not Triggering
1. Check hook is executable: `chmod +x .claude/hooks/quality-check.py`
2. Verify settings.json configuration
3. Ensure ast-grep-py is installed in venv

### No Feedback Visible
1. Verify quality score is below 70%
2. Check Claude Code version supports PostToolUse hooks
3. Review debug logs with `claude --debug`

## Credits
- AST-GREP for pattern matching
- Unified pattern registry (105 patterns)
- Claude Code hooks system