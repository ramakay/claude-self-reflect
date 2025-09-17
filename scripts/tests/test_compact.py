#!/usr/bin/env python3
"""Test file to trigger session quality tracker."""
import logging

logger = logging.getLogger(__name__)

def test_function():
    """Test function with improved patterns."""
    # Good pattern: docstring
    result = []

    # Fixed: using logger instead of print
    logger.info("Testing")

    # Good pattern: list comprehension
    data = [x * 2 for x in range(10)]

    # Fixed: specific exception handling
    try:
        something()
    except ValueError as e:
        logger.error(f"Error occurred: {e}")

    return data