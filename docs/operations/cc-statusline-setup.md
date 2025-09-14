# Claude Code Statusline Setup Guide

## Overview
The CC statusline integration provides real-time visibility into:
1. **Import Status** - How much of your conversation history is indexed
2. **Session Health** - Code quality metrics for files edited in current session

## Installation

### 1. Add to Your Shell Configuration

For **zsh** (most common on macOS):
```bash
# Add to ~/.zshrc
alias csr-status="python ~/projects/claude-self-reflect/scripts/cc-statusline-unified.py"
```

For **bash**:
```bash
# Add to ~/.bashrc or ~/.bash_profile
alias csr-status="python ~/projects/claude-self-reflect/scripts/cc-statusline-unified.py"
```

### 2. Configure Claude Code Statusline

In Claude Code settings, add a custom statusline command:
```
csr-status
```

Or use the full path if alias doesn't work:
```
python /Users/YOUR_USERNAME/projects/claude-self-reflect/scripts/cc-statusline-unified.py
```

## Available Scripts

### 1. **cc-statusline-unified.py** (Recommended)
Cycles between import status and session health every 5 seconds.

```bash
# Default (cycles automatically)
python scripts/cc-statusline-unified.py

# Force specific view
python scripts/cc-statusline-unified.py --import  # Show import status
python scripts/cc-statusline-unified.py --health  # Show session health
```

**Output Examples:**
- `✅ Indexed: 100% (473/473)` - All conversations indexed
- `🔄 Indexed: 67% (300/450)` - Import in progress
- `🟢 Code: A+ Clean` - Excellent code quality, no issues
- `🟡 Code: B (5 issues)` - Good quality with some issues
- `🔴 Code: D (15 issues)` - Poor quality, needs attention

### 2. **cc-statusline-quick.py** (Fast, cached)
Shows only session health from cached data (updates every 2 minutes).

```bash
python scripts/cc-statusline-quick.py
```

### 3. **cc-statusline-health.py** (Full analysis)
Performs real-time session analysis (slower but always current).

```bash
python scripts/cc-statusline-health.py
```

## Understanding the Metrics

### Import Status Indicators
- **✅ 95-100%** - Fully indexed, search will find everything
- **🔄 50-94%** - Partially indexed, newer conversations may be missing
- **⏳ 0-49%** - Early import stage, many conversations not searchable

### Session Health Grades
- **A+/A (🟢)** - Excellent code quality (90%+ score)
- **B/C (🟡)** - Good quality with minor issues (60-89% score)
- **D/F (🔴)** - Poor quality, needs attention (<60% score)

### Common Issues Detected
- Print statements instead of logging
- Bare except clauses
- Synchronous file operations in async code
- Console.log in production code
- Missing type annotations

## Troubleshooting

### "No session data" or "Session: Inactive"
- No files have been edited in the current Claude session
- Run `python scripts/session_quality_tracker.py` to generate initial data

### "Import: Error" or incorrect counts
- Check if Qdrant is running: `docker ps | grep qdrant`
- Verify import state: `python mcp-server/src/status.py`

### Statusline not updating
- The cycle state is stored in `~/.claude-self-reflect/statusline_cycle.json`
- Delete this file to reset the cycle: `rm ~/.claude-self-reflect/statusline_cycle.json`

## Performance

- **Unified script**: ~50ms (cycles between two cached metrics)
- **Quick script**: ~10ms (reads cached JSON)
- **Health script**: ~200ms (loads analyzer, may be slower first time)

## Integration with CI/CD

You can use these metrics in your CI pipeline:

```bash
# Check session quality before committing
quality=$(python scripts/cc-statusline-unified.py --health)
if [[ $quality == *"🔴"* ]]; then
    echo "Code quality issues detected: $quality"
    exit 1
fi
```

## Future Enhancements

Planned improvements:
- [ ] Historical quality trends graph
- [ ] Per-file quality breakdown
- [ ] Integration with pre-commit hooks
- [ ] Custom quality thresholds
- [ ] Team-wide quality dashboards