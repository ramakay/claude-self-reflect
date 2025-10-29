#!/usr/bin/env python3
"""
Centralized configuration for Claude Self-Reflect runtime.
All paths and environment variables are defined here.
"""

import os
from pathlib import Path
from typing import Optional


def get_env_path(env_var: str, default: str) -> Path:
    """Get a path from environment variable, expanding ~ and making absolute."""
    path_str = os.getenv(env_var, default)
    return Path(path_str).expanduser().resolve()


# Core directories
CSR_HOME = get_env_path("CSR_HOME", "~/.claude-self-reflect")
CSR_CONFIG_DIR = get_env_path("CSR_CONFIG_DIR", f"{CSR_HOME}/config")
CSR_BATCH_STATE_DIR = get_env_path("CSR_BATCH_STATE_DIR", f"{CSR_HOME}/batch_state")
CSR_BATCH_QUEUE_DIR = get_env_path("CSR_BATCH_QUEUE_DIR", f"{CSR_HOME}/batch_queue")

# Claude projects directory
CLAUDE_PROJECTS_DIR = get_env_path("CLAUDE_PROJECTS_DIR", "~/.claude/projects")

# Qdrant configuration
QDRANT_URL = os.getenv("QDRANT_URL", "http://localhost:6333")
QDRANT_API_KEY = os.getenv("QDRANT_API_KEY", "")

# Batch watcher configuration
BATCH_SIZE_TRIGGER = int(os.getenv("BATCH_SIZE_TRIGGER", "10"))
BATCH_TIME_TRIGGER_MINUTES = int(os.getenv("BATCH_TIME_TRIGGER_MINUTES", "30"))

# Watcher timing
HOT_WINDOW_MINUTES = int(os.getenv("HOT_WINDOW_MINUTES", "5"))
WARM_WINDOW_HOURS = int(os.getenv("WARM_WINDOW_HOURS", "24"))
MAX_COLD_FILES = int(os.getenv("MAX_COLD_FILES", "5"))

HOT_CHECK_INTERVAL_S = int(os.getenv("HOT_CHECK_INTERVAL_S", "2"))
NORMAL_CHECK_INTERVAL_S = int(os.getenv("NORMAL_CHECK_INTERVAL_S", "60"))

# Subprocess timeout (30 minutes for batch operations)
SUBPROCESS_TIMEOUT_SECONDS = int(os.getenv("SUBPROCESS_TIMEOUT_SECONDS", "1800"))


def ensure_directories():
    """Ensure all required directories exist."""
    directories = [
        CSR_HOME,
        CSR_CONFIG_DIR,
        CSR_BATCH_STATE_DIR,
        CSR_BATCH_QUEUE_DIR,
    ]

    for directory in directories:
        directory.mkdir(parents=True, exist_ok=True)


# Create directories on import
ensure_directories()
