#!/usr/bin/env python3
"""Entry point for running the importer as a module."""

import sys
import logging
from pathlib import Path

# Add parent directory to path for standalone execution
sys.path.insert(0, str(Path(__file__).parent.parent))

from importer.main import main

if __name__ == "__main__":
    sys.exit(main())