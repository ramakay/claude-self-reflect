#!/usr/bin/env python3
"""
Pytest configuration for Claude Self-Reflect tests.
"""

import pytest
import os


def pytest_configure(config):
    """Configure pytest with custom markers."""
    config.addinivalue_line(
        "markers", "requires_qdrant: mark test as requiring Qdrant connection"
    )
    config.addinivalue_line(
        "markers", "requires_docker: mark test as requiring Docker services"
    )
    config.addinivalue_line(
        "markers", "slow: mark test as slow running"
    )


@pytest.fixture(scope="session")
def test_env():
    """Set up test environment variables."""
    return {
        'QDRANT_URL': os.getenv('QDRANT_URL', 'http://localhost:6333'),
        'ANTHROPIC_API_KEY': os.getenv('ANTHROPIC_API_KEY', ''),
    }
