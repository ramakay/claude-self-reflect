#!/usr/bin/env python3
"""
Fast quality check hook - minimal lag version.
Only checks critical issues, uses heavy caching.
"""
import sys
import os
import json
import time
from pathlib import Path

# Quick exit for non-code files
if len(sys.argv) < 2:
    sys.exit(0)

file_path = sys.argv[1]

# Skip non-code files immediately
if not any(file_path.endswith(ext) for ext in ['.py', '.ts', '.js', '.tsx', '.jsx']):
    sys.exit(0)

# Cache check - if file was checked in last 30 seconds, skip
cache_dir = Path.home() / ".claude-self-reflect" / "quality_cache"
cache_dir.mkdir(parents=True, exist_ok=True)

file_hash = str(hash(file_path))
cache_file = cache_dir / f"{file_hash}.json"

if cache_file.exists():
    try:
        cache_data = json.loads(cache_file.read_text())
        if time.time() - cache_data.get('timestamp', 0) < 30:
            # Use cached result
            sys.exit(0)
    except:
        pass

# Quick quality check (no subprocess, no AST-GREP)
try:
    with open(file_path, 'r') as f:
        content = f.read()

    # Very basic checks only
    issues = []
    if 'print(' in content and file_path.endswith('.py'):
        issues.append("Uses print statements")
    if 'console.log' in content and file_path.endswith(('.ts', '.js', '.tsx', '.jsx')):
        issues.append("Uses console.log")

    # Cache result
    cache_data = {
        'timestamp': time.time(),
        'issues': issues,
        'score': 100 - len(issues) * 10
    }
    cache_file.write_text(json.dumps(cache_data))

except Exception:
    # Silent fail - don't block operations
    pass

sys.exit(0)