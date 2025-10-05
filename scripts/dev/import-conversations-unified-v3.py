#!/usr/bin/env python3
"""
Compatibility wrapper for v3.0 modular importer.
This file maintains backward compatibility for existing Docker and scripts
that reference import-conversations-unified.py.
"""

import sys
from pathlib import Path

# Add scripts directory to path
scripts_dir = Path(__file__).parent
sys.path.insert(0, str(scripts_dir))

# Import and run the modular importer
try:
    from importer.main import main
    sys.exit(main())
except ImportError as e:
    print(f"Error: Could not import modular importer: {e}", file=sys.stderr)
    print("Make sure the scripts/importer/ directory is present", file=sys.stderr)
    sys.exit(1)