# Session Startup Hook for Auto-Indexing

## Overview

The Claude Self Reflect system now includes a session startup hook that automatically ensures your current project reaches 100% conversation indexing when you start a Claude session. This feature intelligently chooses between targeted imports for small batches or leveraging the background watcher for larger imports.

## How It Works

When a Claude session starts, the hook:

1. **Detects Current Project**: Identifies which project you're working in
2. **Checks Indexing Status**: Determines how many conversations are already indexed
3. **Smart Import Decision**:
   - For ≤10 missing files: Imports them directly (fast, synchronous)
   - For >10 missing files: Relies on the background watcher (non-blocking)
4. **Provides Feedback**: Shows progress in the session startup log

## Setup Instructions

### 1. Install the Hook Script

The hook script is already created at:
```
hooks/session-start-index.sh
```

### 2. Configure Claude to Use the Hook

Add this to your Claude settings file (`~/.claude/settings.json`):

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "*",
        "hooks": [
          {
            "type": "command",
            "command": "/path/to/claude-self-reflect/hooks/session-start-index.sh",
            "timeout": 30
          }
        ]
      }
    ]
  }
}
```

Replace `/path/to/claude-self-reflect` with the actual path to your installation.

**Note**: The hook configuration goes in `~/.claude/settings.json`, not `config.json`.

### Alternative: Use Claude CLI

You can also configure the hook using the Claude CLI:

```bash
claude config set hooks.SessionStart[0].hooks[0].type command
claude config set hooks.SessionStart[0].hooks[0].command "/path/to/claude-self-reflect/hooks/session-start-index.sh"
```

## Features

### Intelligent Import Strategy

The hook uses different strategies based on the number of missing files:

- **Small Batch (≤10 files)**: Direct import using `import-conversations-unified.py`
  - Fast and synchronous
  - Completes before session fully starts
  - Ideal for catching up on recent conversations

- **Large Batch (>10 files)**: Background watcher handles imports
  - Non-blocking session startup
  - Gradual import over time
  - Prevents session startup delays

### Generic Design

The hook is designed to work with ANY project, not just claude-self-reflect:

- Automatically detects the Claude Self Reflect configuration directory
- Finds the appropriate project folder based on current working directory
- No hardcoded paths to specific projects

### Helper Module

The `scripts/session_index_helper.py` module provides reusable functions:

- `check_project_status()`: Get indexing status for any project
- `import_files()`: Import specific files
- `is_watcher_running()`: Check if background watcher is active
- `find_claude_self_reflect_config()`: Locate config directory dynamically

## Statusline Integration

The indexing percentage is automatically displayed in your Claude statusline if you have the statusline wrapper configured. You'll see:
- 🔍 96.9% - Shows the current overall indexing percentage
- Color coding: Red (<25%), Yellow (<50%), Cyan (<75%), Green (≥75%)
- Updates every 60 seconds (cached for performance)

## Monitoring

### Check Status Manually

You can check the indexing status of your current project:

```bash
python scripts/session_index_helper.py
```

Or for a specific project:

```bash
python scripts/session_index_helper.py /path/to/project
```

### View Hook Logs

Hook output appears in Claude's session startup logs. Look for lines like:

```
[2025-09-01 10:00:00] Session start hook triggered for project: my-project
[2025-09-01 10:00:01] Total JSONL files: 50
[2025-09-01 10:00:01] Already imported: 45
[2025-09-01 10:00:01] Missing: 5
[2025-09-01 10:00:01] Current indexing: 90.0%
[2025-09-01 10:00:01] Importing 5 missing files directly...
[2025-09-01 10:00:05] ✅ Import completed successfully
```

## Troubleshooting

### Hook Not Running

1. Check that the script is executable:
   ```bash
   chmod +x hooks/session-start-index.sh
   ```

2. Verify Claude configuration includes the hook
3. Check Claude logs for hook execution errors

### Import Failures

If imports fail, the hook will fall back to the watcher. Check:

1. Virtual environment is set up correctly
2. Import scripts are present in `scripts/` directory
3. Qdrant is running and accessible
4. State files aren't corrupted

### Performance Considerations

- The hook adds minimal overhead to session startup
- Direct imports (≤10 files) typically complete in <5 seconds
- Large imports are handled asynchronously by the watcher

## Benefits

1. **Always Up-to-Date**: Never miss searching past conversations
2. **Automatic**: No manual import commands needed
3. **Smart**: Chooses the best import strategy automatically
4. **Non-Intrusive**: Doesn't block session for large imports
5. **Project-Agnostic**: Works with any Claude project

## Future Enhancements

Potential improvements for future versions:

- Configurable batch size threshold (currently hardcoded at 10)
- Progress notifications during import
- Automatic watcher startup if not running
- Project-specific import priorities
- Import scheduling based on file age/importance