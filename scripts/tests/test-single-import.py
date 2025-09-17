#!/usr/bin/env python3
"""
Test importing a single file with summary messages
"""

import sys
import subprocess
from pathlib import Path

# The Memento conversation file
test_file = Path.home() / '.claude/projects/-Users-ramakrishnanannaswamy-projects-claude-self-reflect/c072a61e-aebb-4c85-960b-c5ffeafa7115.jsonl'

if not test_file.exists():
    print(f"Test file not found: {test_file}")
    sys.exit(1)

print(f"Testing import of: {test_file.name}")
print(f"This file contains Memento MCP references from August 2025\n")

# Run the unified importer with just this file
cmd = [
    sys.executable,
    'scripts/import-conversations-unified.py',
    '--file', str(test_file)
]

print(f"Running: {' '.join(cmd)}")
result = subprocess.run(cmd, capture_output=True, text=True)

print("\n--- Output ---")
print(result.stdout)
if result.stderr:
    print("\n--- Errors ---")
    print(result.stderr)

print(f"\n--- Exit code: {result.returncode} ---")

if result.returncode == 0:
    print("\n✅ Import completed successfully!")
    print("Now search for 'memento mcp' in MCP to verify it was imported.")
else:
    print("\n❌ Import failed!")