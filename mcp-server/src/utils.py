"""Shared utilities for claude-self-reflect MCP server and scripts."""

from pathlib import Path


def normalize_project_name(project_path: str) -> str:
    """
    Normalize project name for consistent hashing across import/search.
    
    Handles various path formats:
    - Claude logs format: -Users-kyle-Code-claude-self-reflect -> claude-self-reflect
    - File paths in Claude logs: /path/to/-Users-kyle-Code-claude-self-reflect/file.jsonl -> claude-self-reflect  
    - Regular file paths: /path/to/project/file.txt -> project
    - Regular paths: /path/to/project -> project
    - Already normalized: project -> project
    
    Args:
        project_path: Project path or name in any format
        
    Returns:
        Normalized project name suitable for consistent hashing
    """
    if not project_path:
        return ""
    
    # Remove trailing slashes
    project_path = project_path.rstrip('/')
    
    # Handle Claude logs format (starts with dash)
    if project_path.startswith('-'):
        # For paths like -Users-kyle-Code-claude-self-reflect
        # We want to extract the actual project name which may contain dashes
        # Strategy: Find common parent directories and extract what comes after
        
        # Remove leading dash and convert back to path-like format
        path_str = project_path[1:].replace('-', '/')
        path_parts = Path(path_str).parts
        
        # Look for common project parent directories
        project_parents = {'projects', 'code', 'Code', 'repos', 'repositories', 
                          'dev', 'Development', 'work', 'src', 'github'}
        
        # Find the project name after a known parent directory
        for i, part in enumerate(path_parts):
            if part.lower() in project_parents and i + 1 < len(path_parts):
                # Everything after the parent directory is the project name
                # Join remaining parts with dash if project name has multiple components
                remaining = path_parts[i + 1:]
                return '-'.join(remaining)
        
        # Fallback: just use the last component
        return path_parts[-1] if path_parts else project_path
    
    # Check if this is a file path that contains a Claude logs directory
    # Pattern: /path/to/-Users-...-projects-..../filename
    path_obj = Path(project_path)
    
    # Look for a parent directory that starts with dash (Claude logs format)
    for parent in path_obj.parents:
        parent_name = parent.name
        if parent_name.startswith("-"):
            # Found a Claude logs directory, process it
            return normalize_project_name(parent_name)
    
    # Handle regular paths - if it's a file, get the parent directory
    # Otherwise use the directory/project name itself
    if path_obj.suffix:  # It's a file (has an extension)
        # Use the parent directory name
        return path_obj.parent.name
    else:
        # Use the directory name itself
        return path_obj.name