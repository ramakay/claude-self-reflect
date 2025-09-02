#!/usr/bin/env python3
"""
Shared utilities for Claude Self Reflect.
Contains common functions used across multiple modules.
"""

import os
from pathlib import Path
from typing import Optional


def get_claude_projects_dir() -> Path:
    """Get the Claude projects directory, checking multiple possible locations.
    
    Returns:
        Path to the Claude projects directory
    """
    # Check environment variable first
    if 'CLAUDE_PROJECTS_DIR' in os.environ:
        return Path(os.environ['CLAUDE_PROJECTS_DIR'])
    
    # Default location
    return Path.home() / ".claude" / "projects"


def get_csr_config_dir() -> Path:
    """Get the Claude Self Reflect config directory.
    
    Returns:
        Path to the CSR config directory
    """
    # Check environment variable first
    if 'CSR_CONFIG_DIR' in os.environ:
        return Path(os.environ['CSR_CONFIG_DIR'])
    
    # Check common locations in order of preference
    possible_locations = [
        Path.home() / '.claude-self-reflect' / 'config',
        Path('/config'),  # Docker location
        Path.home() / '.config' / 'claude-self-reflect',
    ]
    
    for location in possible_locations:
        if location.exists():
            return location
    
    # Default to the first option if none exist
    return possible_locations[0]


def normalize_file_path(file_path: str) -> str:
    """Normalize file paths to handle Docker vs local path differences.
    
    Converts:
    - /logs/PROJECT_DIR/file.jsonl -> ~/.claude/projects/PROJECT_DIR/file.jsonl
    - Already normalized paths remain unchanged
    
    This is the single source of truth for path normalization.
    """
    if file_path.startswith("/logs/"):
        # Convert Docker path to local path
        projects_dir = str(get_claude_projects_dir())
        return file_path.replace("/logs/", projects_dir + "/", 1)
    return file_path


def extract_project_name_from_path(file_path: str) -> str:
    """Extract project name from JSONL file path.
    
    Handles paths like:
    - ~/.claude/projects/-Users-username-projects-claude-self-reflect/file.jsonl
    - /logs/-Users-username-projects-n8n-builder/file.jsonl
    """
    # Get the directory name containing the JSONL file
    path_obj = Path(file_path)
    dir_name = path_obj.parent.name
    
    # Extract project name from dash-encoded path
    # Format: -Users-username-projects-PROJECT_NAME (PROJECT_NAME can have dashes)
    if dir_name.startswith('-') and 'projects' in dir_name:
        parts = dir_name.split('-')
        # Find 'projects' and take everything after it as the project name
        try:
            projects_idx = parts.index('projects')
            if projects_idx + 1 < len(parts):
                # Join all parts after 'projects' to handle multi-part project names
                # like "claude-self-reflect", "n8n-builder", etc.
                project_parts = parts[projects_idx + 1:]
                return '-'.join(project_parts)
        except ValueError:
            pass
    
    # Fallback: use the directory name as-is
    return dir_name.lstrip('-')