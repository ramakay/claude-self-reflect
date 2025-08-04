#!/usr/bin/env python3
"""Enhanced watcher that runs import periodically and supports manual triggers."""

import time
import subprocess
import os
import sys
from datetime import datetime
from pathlib import Path

WATCH_INTERVAL = int(os.getenv('WATCH_INTERVAL', '60'))
SIGNAL_FILE = Path("/tmp/claude-self-reflect-import-current")
CHECK_INTERVAL = 1  # Check for signal file every second

print(f"[Watcher] Starting enhanced import watcher with {WATCH_INTERVAL}s interval", flush=True)
print(f"[Watcher] Monitoring signal file: {SIGNAL_FILE}", flush=True)

last_import = 0

while True:
    current_time = time.time()
    
    # Check for manual trigger signal
    if SIGNAL_FILE.exists():
        print(f"[Watcher] Signal detected! Running immediate import...", flush=True)
        try:
            # Read conversation ID if provided
            conversation_id = None
            try:
                conversation_id = SIGNAL_FILE.read_text().strip()
            except:
                pass
            
            # Remove signal file to prevent re-triggering
            SIGNAL_FILE.unlink()
            
            # Run import with special flag for current conversation only
            cmd = [sys.executable, "/scripts/import-conversations-unified.py"]
            if conversation_id:
                cmd.extend(["--conversation-id", conversation_id])
            else:
                # Import only today's conversations for manual trigger
                cmd.extend(["--days", "1"])
            
            # Write progress indicator
            progress_file = Path("/tmp/claude-self-reflect-import-progress")
            progress_file.write_text("🔄 Starting import...")
            
            print(f"[Watcher] Running command: {' '.join(cmd)}", flush=True)
            result = subprocess.run(cmd, capture_output=True, text=True)
            
            if result.returncode == 0:
                print(f"[Watcher] Manual import completed successfully", flush=True)
                # Create completion signal
                Path("/tmp/claude-self-reflect-import-complete").touch()
            else:
                print(f"[Watcher] Manual import failed with code {result.returncode}", flush=True)
                if result.stderr:
                    print(f"[Watcher] Error: {result.stderr}", flush=True)
            
            last_import = current_time
                    
        except Exception as e:
            print(f"[Watcher] Error during manual import: {e}", flush=True)
    
    # Regular scheduled import
    elif current_time - last_import >= WATCH_INTERVAL:
        try:
            print(f"[Watcher] Running scheduled import at {datetime.now().isoformat()}", flush=True)
            result = subprocess.run([
                sys.executable, 
                "/scripts/import-conversations-unified.py"
            ], capture_output=True, text=True)
            
            if result.returncode == 0:
                print(f"[Watcher] Scheduled import completed successfully", flush=True)
            else:
                print(f"[Watcher] Scheduled import failed with code {result.returncode}", flush=True)
                if result.stderr:
                    print(f"[Watcher] Error: {result.stderr}", flush=True)
            
            last_import = current_time
                    
        except Exception as e:
            print(f"[Watcher] Error during scheduled import: {e}", flush=True)
    
    # Short sleep to check for signals frequently
    time.sleep(CHECK_INTERVAL)