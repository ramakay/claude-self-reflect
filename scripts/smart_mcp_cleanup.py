#!/usr/bin/env python3
"""Smart MCP cleanup - keeps only the newest instance of each MCP type."""

import subprocess
import re
from datetime import datetime
from collections import defaultdict

def get_mcp_processes():
    """Get all MCP-related processes with details."""
    # Use proper argument list instead of shell=True
    ps_result = subprocess.run(['ps', 'aux'], capture_output=True, text=True, timeout=5)
    grep_pattern = 'mcp|context7|playwright|zen-mcp|memento|mantis|blender'

    # Filter lines manually instead of using shell pipe
    lines = ps_result.stdout.strip().split('\n')
    filtered_lines = [line for line in lines if any(keyword in line.lower()
                     for keyword in ['mcp', 'context7', 'playwright', 'zen-mcp',
                                     'memento', 'mantis', 'blender'])]

    processes = []
    for line in filtered_lines:
        if 'grep' in line:
            continue

        parts = line.split()
        if len(parts) < 11:
            continue

        # Extract process info
        pid = parts[1]
        start_time = parts[8]  # Time started
        command = ' '.join(parts[10:])

        # Identify MCP type
        mcp_type = None
        if 'context7' in command:
            mcp_type = 'context7'
        elif 'playwright' in command:
            mcp_type = 'playwright'
        elif 'zen-mcp' in command:
            mcp_type = 'zen'
        elif 'memento' in command:
            mcp_type = 'memento'
        elif 'mantis' in command:
            mcp_type = 'mantis'
        elif 'blender' in command:
            mcp_type = 'blender'
        elif 'shopify' in command:
            mcp_type = 'shopify'

        if mcp_type:
            processes.append({
                'pid': pid,
                'type': mcp_type,
                'start_time': start_time,
                'command': command[:80]  # Truncate for display
            })

    return processes

def group_by_type(processes):
    """Group processes by MCP type."""
    grouped = defaultdict(list)
    for proc in processes:
        grouped[proc['type']].append(proc)
    return grouped

def cleanup_duplicates(grouped):
    """Kill all but the most recent process of each type."""
    killed = []
    kept = []

    for mcp_type, procs in grouped.items():
        if len(procs) <= 1:
            if procs:
                kept.append(procs[0])
            continue

        # Sort by PID (higher PID = more recent generally)
        procs.sort(key=lambda x: int(x['pid']), reverse=True)

        # Keep the first (newest), kill the rest
        kept.append(procs[0])
        for proc in procs[1:]:
            try:
                # Use proper argument list instead of shell=True
                subprocess.run(['kill', '-TERM', proc['pid']], check=False, timeout=2)
                killed.append(proc)
            except:
                pass

    return killed, kept

def main():
    print("🔍 Analyzing MCP processes...")
    print()

    processes = get_mcp_processes()
    grouped = group_by_type(processes)

    # Show current state
    print("📊 Current MCP processes by type:")
    total = 0
    for mcp_type, procs in sorted(grouped.items()):
        count = len(procs)
        total += count
        status = "⚠️ DUPLICATES" if count > 1 else "✅"
        print(f"  {mcp_type:15} {count:2} instances {status}")

    print(f"\n  Total: {total} processes")

    if total == 0:
        print("\n✅ No MCP processes found")
        return

    # Check for duplicates
    has_duplicates = any(len(procs) > 1 for procs in grouped.values())

    if not has_duplicates:
        print("\n✅ No duplicates found!")
        return

    # Clean up
    print("\n🧹 Cleaning up duplicates...")
    killed, kept = cleanup_duplicates(grouped)

    if killed:
        print(f"\n❌ Killed {len(killed)} duplicate processes:")
        for proc in killed:
            print(f"    PID {proc['pid']:5} ({proc['type']}) started at {proc['start_time']}")

    print(f"\n✅ Kept {len(kept)} active processes:")
    for proc in kept:
        print(f"    PID {proc['pid']:5} ({proc['type']}) started at {proc['start_time']}")

    print("\n🎯 Cleanup complete!")
    print("\n💡 If typing is still laggy:")
    print("   1. Close all but current Claude window")
    print("   2. Run: claude mcp restart")
    print("   3. Restart Claude if needed")

if __name__ == "__main__":
    main()