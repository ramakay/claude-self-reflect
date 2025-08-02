# Release v2.4.14 - Project Search Fix

## Overview
This release fixes a critical issue where project-scoped searches required users to explicitly specify `project: "all"` to find conversations. Thanks to @kylesnowschwartz for the excellent debugging and root cause analysis in issue #27.

## What's Fixed
- **Project name mismatch** between import and search operations
- Search now correctly identifies project-specific collections without requiring "all"
- Added shared normalization function for consistent project name hashing

## Technical Details
The issue was caused by different project name resolution strategies:
- **Import**: Used full directory names like `-Users-kyle-Code-claude-self-reflect`
- **Search**: Used only the basename like `claude-self-reflect`

This created different hashes, preventing project-scoped search from finding the correct collections.

## Upgrade Instructions

### For npm users:
```bash
npm update -g claude-self-reflect
```

### For Docker users:
Since Python files have been modified, you need to rebuild the Docker images:

```bash
# Stop running containers
docker compose down

# Rebuild images with the fix
docker compose build --no-cache

# Start services
docker compose up -d
```

### For development/source users:
```bash
git pull
# No additional steps needed if using Python directly
```

## Important Notes

### Backward Compatibility
- **Existing collections remain fully functional** - no re-import needed
- Old collections are still searchable using `project: "all"`
- New imports will create properly normalized collections

### Do I Need to Re-import?
**No**, unless you specifically want project-scoped search to work without specifying "all". The system maintains full backward compatibility.

If you do want to re-import for a specific project:
```bash
cd /path/to/your/project
source ~/claude-self-reflect/venv/bin/activate
python ~/claude-self-reflect/scripts/import-conversations-unified.py
```

## Testing the Fix
After upgrading, project-scoped searches should work naturally:
- ❌ Before: `Search for past conversations about X` → No results  
- ✅ After: `Search for past conversations about X` → Finds project-specific results

## Also Try: v2.5.0-RC
We have an exciting Release Candidate that adds enhanced code reference tracking:
- `search_by_file("/path/to/file.py")` - Find conversations by actual file interactions
- `search_by_concept("docker")` - Search by development concepts

Learn more: https://github.com/ramakay/claude-self-reflect/discussions/24

## Acknowledgments
Special thanks to @kylesnowschwartz for the detailed bug report and debugging assistance that made this fix possible.