#!/usr/bin/env python3
"""Trigger immediate import with progress feedback."""

import sys
import time
from pathlib import Path
import argparse

TRIGGER_FILE = Path("/tmp/claude-self-reflect-import-current")
COMPLETE_FILE = Path("/tmp/claude-self-reflect-import-complete")
PROGRESS_FILE = Path("/tmp/claude-self-reflect-import-progress")

def cleanup_files():
    """Clean up signal files."""
    for file in [COMPLETE_FILE, PROGRESS_FILE]:
        if file.exists():
            file.unlink()

def monitor_progress(timeout=30):
    """Monitor import progress with detailed feedback."""
    print("📤 Importing current conversation to Qdrant...")
    
    start_time = time.time()
    last_progress = ""
    spinner = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏']
    spinner_idx = 0
    
    while time.time() - start_time < timeout:
        # Check if import completed
        if COMPLETE_FILE.exists():
            sys.stdout.write("\r✅ Import completed successfully!" + " " * 50 + "\n")
            sys.stdout.flush()
            cleanup_files()
            return True
        
        # Check progress file for updates
        if PROGRESS_FILE.exists():
            try:
                progress = PROGRESS_FILE.read_text().strip()
                if progress != last_progress:
                    sys.stdout.write(f"\r{progress}" + " " * 20)
                    sys.stdout.flush()
                    last_progress = progress
            except:
                pass
        else:
            # Show spinner if no progress updates
            elapsed = int(time.time() - start_time)
            sys.stdout.write(f"\r{spinner[spinner_idx]} Processing... ({elapsed}s)" + " " * 10)
            sys.stdout.flush()
            spinner_idx = (spinner_idx + 1) % len(spinner)
        
        time.sleep(0.1)
    
    # Timeout
    sys.stdout.write("\r⚠️  Import timeout after 30s - proceeding anyway" + " " * 30 + "\n")
    sys.stdout.flush()
    cleanup_files()
    TRIGGER_FILE.unlink(missing_ok=True)
    return False

def main():
    parser = argparse.ArgumentParser(description='Trigger immediate conversation import')
    parser.add_argument('--conversation-id', help='Specific conversation ID to import')
    parser.add_argument('--no-wait', action='store_true', help='Don\'t wait for completion')
    args = parser.parse_args()
    
    # Clean up old files
    cleanup_files()
    
    # Create trigger file
    if args.conversation_id:
        TRIGGER_FILE.write_text(args.conversation_id)
    else:
        TRIGGER_FILE.touch()
    
    if args.no_wait:
        print("🚀 Import triggered (not waiting for completion)")
        return
    
    # Monitor progress
    success = monitor_progress()
    
    # Return appropriate exit code
    sys.exit(0 if success else 1)

if __name__ == "__main__":
    main()