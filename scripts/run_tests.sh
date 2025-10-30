#!/bin/bash
# Run automated tests for Claude Self-Reflect
# Tests batch automation (v7.0 primary feature)

set -e

echo "Running Claude Self-Reflect Test Suite"
echo "========================================"
echo

# Activate virtual environment if it exists
if [ -d "venv" ]; then
    source venv/bin/activate
fi

# Run pytest
echo "Running pytest tests..."
python3 -m pytest tests/ -v --tb=short

echo
echo "Test suite complete!"
