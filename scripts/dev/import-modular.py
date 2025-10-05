#!/usr/bin/env python3
"""
Wrapper script to use the new modular importer system.
This replaces the monolithic import-conversations-unified.py.
"""

import sys
from pathlib import Path

# Add scripts directory to path
scripts_dir = Path(__file__).parent
sys.path.insert(0, str(scripts_dir))

# Import and run the modular importer
from importer.main import main

if __name__ == "__main__":
    sys.exit(main())