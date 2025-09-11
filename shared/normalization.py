"""Shared normalization utilities for claude-self-reflect.

This module provides the single source of truth for project name normalization,
ensuring consistent hashing across import scripts and the MCP server.
"""

from pathlib import Path


def normalize_project_name(project_path: str, _depth: int = 0) -> str:
    """
    Normalize project name for consistent hashing across import/search.
    
    This is the authoritative normalization function used by both:
    - Import scripts (import-conversations-unified.py)
    - MCP server (server.py)
    
    Examples:
        '/Users/name/.claude/projects/-Users-name-projects-myproject' -> 'myproject'
        '-Users-name-projects-myproject' -> 'myproject'
        '/path/to/myproject' -> 'myproject'
        'myproject' -> 'myproject'
        
    Special handling for Claude's dash-separated format:
        When a path component starts with '-' and contains 'projects',
        we extract everything after 'projects-' as the project name.
        This handles dashes in project names correctly.
        
    Args:
        project_path: Project path or name in any format
        _depth: Internal recursion depth counter (for backwards compatibility)
        
    Returns:
        Normalized project name suitable for consistent hashing
    """
    if not project_path:
        return ""
    
    path = Path(project_path.rstrip('/'))
    
    # Extract the final directory name
    final_component = path.name
    
    # If it's Claude's dash-separated format, extract project name
    if final_component.startswith('-') and 'projects' in final_component:
        # Find the last occurrence of 'projects-' to handle edge cases
        # This correctly extracts 'claude-self-reflect' from:
        # '-Users-ramakrishnanannaswamy-projects-claude-self-reflect'
        idx = final_component.rfind('projects-')
        if idx != -1:
            return final_component[idx + len('projects-'):]
    
    # For regular paths, just return the directory name
    return final_component if final_component else path.parent.name