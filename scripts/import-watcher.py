#!/usr/bin/env python3
"""Simple watcher that runs import periodically."""

import time
import subprocess
import os
import sys
from datetime import datetime

WATCH_INTERVAL = int(os.getenv('WATCH_INTERVAL', '60'))

print(f"[Watcher] Starting import watcher with {WATCH_INTERVAL}s interval", flush=True)

while True:
    try:
        print(f"[Watcher] Running import at {datetime.now().isoformat()}", flush=True)
        result = subprocess.run([
            sys.executable, 
            "/scripts/import-conversations-unified.py"
        ], capture_output=True, text=True)
        
        if result.returncode == 0:
            print(f"[Watcher] Import completed successfully", flush=True)
        else:
            print(f"[Watcher] Import failed with code {result.returncode}", flush=True)
            if result.stderr:
                print(f"[Watcher] Error: {result.stderr}", flush=True)
                
    except Exception as e:
        print(f"[Watcher] Error: {e}", flush=True)
    
    print(f"[Watcher] Sleeping for {WATCH_INTERVAL} seconds...", flush=True)
    time.sleep(WATCH_INTERVAL)