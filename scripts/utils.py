"""Utility functions for Claude Self-Reflect."""

import hashlib
import re
from pathlib import Path


def normalize_project_name(path: str) -> str:
    """
    Normalize project name from path for consistent collection naming.
    
    Args:
        path: File path or project path
        
    Returns:
        Normalized project name
    """
    # Extract project name from path
    path_obj = Path(path)
    
    # Look for common project indicators
    parts = path_obj.parts
    
    # Find the project root (usually after 'projects' or similar)
    project_name = None
    for i, part in enumerate(parts):
        if part in ['projects', 'repos', 'code', 'src']:
            if i + 1 < len(parts):
                project_name = parts[i + 1]
                break
    
    # If no project indicator found, use the last meaningful directory
    if not project_name:
        for part in reversed(parts):
            if part and not part.startswith('.') and part not in ['home', 'Users', 'var', 'tmp']:
                project_name = part
                break
    
    # Fallback to 'default' if nothing found
    if not project_name:
        project_name = 'default'
    
    # Normalize the name (lowercase, replace special chars)
    normalized = re.sub(r'[^a-z0-9]+', '_', project_name.lower())
    normalized = normalized.strip('_')
    
    return normalized or 'default'


def get_project_hash(project_name: str) -> str:
    """Get MD5 hash of project name for collection naming."""
    return hashlib.md5(project_name.encode()).hexdigest()[:8]