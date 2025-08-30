#!/usr/bin/env python3
"""Monitor the safe-watcher container and track indexing progress."""

import subprocess
import json
import time
import psutil
from datetime import datetime

def get_container_stats():
    """Get Docker container stats."""
    try:
        result = subprocess.run(
            ["docker", "stats", "--no-stream", "--format", "json", "claude-reflection-safe-watcher"],
            capture_output=True, text=True, check=True
        )
        if result.stdout:
            return json.loads(result.stdout)
        return None
    except:
        return None

def get_indexing_status():
    """Get current indexing status from claude-self-reflect."""
    try:
        result = subprocess.run(
            ["claude-self-reflect", "status"],
            capture_output=True, text=True, check=True
        )
        return json.loads(result.stdout)
    except:
        return None

def monitor():
    """Monitor the watcher and indexing progress."""
    print("=" * 80)
    print("MONITORING SAFE-WATCHER INDEXING PROGRESS")
    print("=" * 80)
    
    # Get initial status
    initial_status = get_indexing_status()
    if initial_status:
        overall = initial_status['overall']
        print(f"Starting: {overall['percentage']:.1f}% ({overall['indexed']}/{overall['total']})")
        print("-" * 80)
    
    # Monitor every 5 seconds
    while True:
        # Check if container is running
        container_stats = get_container_stats()
        
        if not container_stats:
            print("\n❌ Container not running!")
            break
            
        # Parse container stats
        cpu_percent = container_stats.get('CPUPerc', '0%').rstrip('%')
        mem_usage = container_stats.get('MemUsage', 'N/A')
        
        # Get current indexing status
        status = get_indexing_status()
        
        if status:
            overall = status['overall']
            percentage = overall['percentage']
            indexed = overall['indexed']
            total = overall['total']
            backlog = overall.get('backlog', 0)
            
            # Print status line
            timestamp = datetime.now().strftime("%H:%M:%S")
            print(f"\r[{timestamp}] Progress: {percentage:.1f}% ({indexed}/{total}) | "
                  f"Backlog: {backlog} | CPU: {cpu_percent}% | Mem: {mem_usage}", end='', flush=True)
            
            # Check if complete
            if percentage >= 100.0:
                print(f"\n\n✅ INDEXING COMPLETE! 100% ({indexed}/{total})")
                break
                
            # Alert if CPU is too high
            if float(cpu_percent) > 200:
                print(f"\n⚠️  WARNING: High CPU usage: {cpu_percent}%")
        
        time.sleep(5)

if __name__ == "__main__":
    monitor()