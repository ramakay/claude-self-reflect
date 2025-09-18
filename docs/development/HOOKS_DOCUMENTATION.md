# Claude Self-Reflect Hooks Documentation

## Overview
Hooks are shell scripts that execute automatically in response to Claude events, providing integration points for quality control, monitoring, and automation.

## Hook System Architecture

### Location
All hooks are stored in `.claude/hooks/` directory within the project root.

### Execution Context
- Hooks run in the project directory
- Have access to environment variables from Claude
- Can call Python scripts and other tools
- Should be non-blocking for responsiveness

## Available Hooks

### 1. pre-commit
**Location**: `.claude/hooks/pre-commit`
**Trigger**: Before any git commit operation
**Purpose**: Update code quality status before committing changes

**Functionality**:
- Updates quality status cache using AST-GREP analysis
- Runs `session_quality_tracker.py` to analyze current changes
- Updates project-wide quality metrics
- Non-blocking - always allows commit to proceed

**Key Operations**:
```bash
# Update quality cache
python3 scripts/session_quality_tracker.py
python3 scripts/update-quality-all-projects.py --project "$PROJECT_NAME"
```

**Environment Variables**:
- `PROJECT_ROOT`: Root directory of Claude Self-Reflect
- `PROJECT_NAME`: Name of current project

### 2. post-generation
**Location**: `.claude/hooks/post-generation`
**Trigger**: After Claude generates or modifies code
**Purpose**: Track edited files and run quality analysis on session changes

**Functionality**:
- Detects files modified in the last minute
- Updates session edit tracker JSON
- Runs quality checks asynchronously
- Non-blocking to maintain responsiveness

**Key Features**:
- Tracks only recent edits (last minute)
- Maintains session state in `~/.claude-self-reflect/current_session_edits.json`
- Deduplicates file paths automatically
- Runs quality analysis in background

**Tracker File Format**:
```json
{
  "session_start": "2025-09-17T10:00:00",
  "edited_files": [
    "/absolute/path/to/file1.py",
    "/absolute/path/to/file2.ts"
  ],
  "last_updated": "2025-09-17T10:15:00",
  "project": "claude-self-reflect",
  "version": "1.0",
  "lock_pid": null
}
```

**Concurrency Protection**:
- File writes are atomic using temp file + rename pattern
- Lock PID field prevents concurrent access
- Corrupted files trigger automatic recreation
- Session cleanup after 24 hours of inactivity

### 3. user-prompt-submit-hook (Planned)
**Purpose**: Validate and enhance user prompts before processing
**Potential Features**:
- Risk assessment for operations
- Automatic context loading
- Prompt enhancement suggestions
- Security validation

## Hook Configuration

### Making Hooks Executable
```bash
chmod +x .claude/hooks/pre-commit
chmod +x .claude/hooks/post-generation
```

### Hook Settings
Hooks can be configured globally or per-project in Claude settings.

### Disabling Hooks
If a hook is blocking operations:
1. Check hook configuration in Claude settings
2. Review hook scripts for errors
3. Temporarily rename or remove problematic hooks
4. Use `--no-hooks` flag if available

## Integration with Quality Systems

### AST-GREP Integration
Hooks trigger AST-GREP pattern analysis through:
- `ast_grep_unified_registry.py`: Pattern registry
- `ast_grep_final_analyzer.py`: Analysis engine
- `session_quality_tracker.py`: Session-specific tracking

### Quality Metrics Tracked
- Code complexity patterns
- Security vulnerabilities
- Performance issues
- Best practice violations
- Test coverage gaps

### Session Tracking
The post-generation hook maintains a session tracker that:
- Records all files edited in current session
- Enables focused quality analysis
- Prevents analyzing entire codebase
- Improves performance

## Best Practices

### 1. Performance Considerations
- Keep hooks lightweight and fast
- Use background processing for heavy operations
- Implement timeouts to prevent hanging
- Cache results where possible

### 2. Error Handling
- Always exit with code 0 unless blocking is intentional
- Log errors to separate files for debugging
- Use `2>/dev/null` to suppress stderr in production
- Implement fallback behavior

### 3. Security
- Validate all inputs
- Avoid executing untrusted code
- Use absolute paths to prevent path traversal
- Limit file system access

### 4. Logging
- Log to `~/.claude-self-reflect/logs/` for debugging
- Rotate logs to prevent disk usage issues
- Include timestamps and context
- Separate info/debug/error levels

## Troubleshooting

### Common Issues

#### Hook Not Executing
- Check file permissions (must be executable)
- Verify hook is in correct location
- Check Claude settings for hook configuration
- Review Claude logs for errors

#### Hook Blocking Operations
- Add timeout to long-running operations
- Move heavy processing to background
- Check for infinite loops or deadlocks
- Review error handling logic

#### Session Tracker Issues
- Check file permissions for tracker file
- Verify Python environment and dependencies
- Clear tracker file if corrupted
- Check disk space availability

### Debug Mode
Enable verbose logging by setting:
```bash
export CLAUDE_HOOK_DEBUG=1
```

### Testing Hooks
Test hooks manually:
```bash
# Test pre-commit
cd /path/to/project
.claude/hooks/pre-commit

# Test post-generation with mock edits
touch test.py
.claude/hooks/post-generation
```

## Advanced Features

### Custom Hook Variables
Hooks can access:
- `$CLAUDE_SESSION_ID`: Current session identifier
- `$CLAUDE_PROJECT`: Project name
- `$CLAUDE_USER`: User identifier
- `$CLAUDE_OPERATION`: Current operation type

### Hook Chaining
Hooks can trigger other hooks:
```bash
# In post-generation hook
if [ "$QUALITY_CHECK_PASSED" = "false" ]; then
    .claude/hooks/quality-failed
fi
```

### Conditional Execution
Hooks can check conditions:
```bash
# Only run for Python files
if [[ "$EDITED_FILES" == *.py ]]; then
    python3 scripts/python_specific_check.py
fi
```

## Security Considerations

### Input Validation
All hooks should validate inputs:
```bash
# Sanitize file paths
SAFE_PATH=$(realpath "$USER_INPUT" 2>/dev/null)
if [[ ! "$SAFE_PATH" =~ ^/expected/path ]]; then
    exit 1
fi
```

### Resource Limits
Implement resource constraints:
```bash
# Timeout after 30 seconds
timeout 30s python3 quality_check.py
```

### Audit Logging
Log security-relevant events:
```bash
echo "$(date): Hook executed by $USER for $PROJECT" >> ~/.claude-self-reflect/audit.log
```

## Future Enhancements

### Planned Hooks
1. **pre-edit**: Validate before file modifications
2. **post-commit**: Actions after successful commits
3. **pre-search**: Enhance search queries
4. **post-search**: Process search results
5. **quality-gate**: Block operations on quality failures

### Integration Points
- GitHub Actions integration
- CI/CD pipeline triggers
- External monitoring systems
- Team collaboration features

## Version History
- v1.0: Initial hook system with pre-commit and post-generation
- v1.1: Added session tracking and quality integration
- v1.2: Enhanced error handling and performance optimization