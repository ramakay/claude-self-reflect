#!/usr/bin/env python3
"""
Session index runner - handles the main indexing logic for session startup.
This is called by session-start-index.sh.
"""

import sys
import os
import json
import time
from pathlib import Path

def write_status(message, project_path, is_active=True):
    """Write status to a project-scoped file that statusline can read.
    
    Args:
        message: Status message to write (None to clear)
        project_path: Project path for scoping the status file
        is_active: Whether the operation is still active
    """
    # Derive a stable, filesystem-safe project identifier
    proj_id = str(Path(project_path).resolve()).replace('/', '-').lstrip('-')
    status_file = Path(f"/tmp/csr-indexing-status.{proj_id}.json")
    
    # Clear status if message is None
    if message is None:
        try:
            if status_file.exists():
                status_file.unlink()
        except Exception:
            pass
        return
    
    try:
        status_data = {
            "message": message,
            "active": is_active,
            "timestamp": time.time(),
            "project": str(Path(project_path).resolve())
        }
        
        # Write atomically using temp file + rename
        tmp_file = status_file.with_suffix(status_file.suffix + '.tmp')
        tmp_file.write_text(json.dumps(status_data))
        os.replace(str(tmp_file), str(status_file))
    except Exception as e:
        print(f"Status write failed: {e}", file=sys.stderr)


def main():
    """Main function to check and import missing files."""
    if len(sys.argv) < 3:
        print("Error: Missing required arguments", file=sys.stderr)
        sys.exit(1)
    
    project_path = sys.argv[1]
    csr_root = sys.argv[2]
    
    # Check if we should be quiet
    quiet_mode = os.environ.get('CSR_QUIET_MODE', 'true') == 'true'
    
    # Configuration: batch size threshold (can be made configurable)
    BATCH_SIZE_THRESHOLD = int(os.environ.get('CSR_BATCH_SIZE_THRESHOLD', '10'))
    
    # Add scripts to path
    sys.path.insert(0, os.path.join(csr_root, 'scripts'))
    
    try:
        from session_index_helper import (
            check_project_status,
            import_files,
            is_watcher_running
        )
    except ImportError as e:
        print(f"Warning: Could not import session_index_helper: {e}", file=sys.stderr)
        sys.exit(0)
    
    # Write initial status
    write_status("🔄 Checking project...", project_path)
    
    # Check project status
    try:
        total, imported, missing_files = check_project_status(project_path)
    except Exception as e:
        print(f"Error checking project status: {e}", file=sys.stderr)
        write_status("❌ Error checking status", project_path, False)
        sys.exit(1)
    
    if total == 0:
        if not quiet_mode:
            print("No conversation files to import")
        write_status("✅ No files to import", project_path, False)
        return
    
    # Only print details in verbose mode
    if not quiet_mode:
        print(f"Total JSONL files: {total}")
        print(f"Already imported: {imported}")
        print(f"Missing: {len(missing_files)}")
    
    if len(missing_files) == 0:
        # Silent when already at 100% in quiet mode
        if not quiet_mode:
            print("✅ Project is already at 100% indexing!")
        # Clear the status file when at 100%
        write_status(None, project_path)
        return
    
    # Calculate percentage
    percentage = (imported / total * 100) if total > 0 else 100
    
    # Silent - no output needed, statusline will show progress
    
    # Clear any old status
    write_status(None, project_path)
    
    # Decision logic: Use targeted import for small batches, watcher for continuous
    if len(missing_files) <= BATCH_SIZE_THRESHOLD:
        # Silent import
        write_status(None, project_path)
        
        # Import the missing files
        try:
            success = import_files(missing_files, limit=BATCH_SIZE_THRESHOLD)
            
            # Silent - no messages needed
            write_status(None, project_path)
        except Exception:
            # Silent error handling
            write_status(None, project_path)
    else:
        # Silent - watcher will handle
        write_status(None, project_path)
        
        # Check if watcher is active
        try:
            # Silent - let watcher work in background
            write_status(None, project_path)
        except Exception:
            # Silent error handling
            write_status(None, project_path)

if __name__ == '__main__':
    main()