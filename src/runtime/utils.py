"""Shared utilities for claude-self-reflect MCP server and scripts."""

from pathlib import Path


def normalize_project_name(project_path: str, _depth: int = 0) -> str:
    """
    Simplified project name normalization for consistent hashing.
    
    Examples:
        '/Users/name/.claude/projects/-Users-name-projects-myproject' -> 'myproject'
        '-Users-name-projects-myproject' -> 'myproject'
        '/path/to/myproject' -> 'myproject'
        'myproject' -> 'myproject'
        
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
        idx = final_component.rfind('projects-')
        if idx != -1:
            return final_component[idx + len('projects-'):]
    
    # For regular paths, just return the directory name
    return final_component if final_component else path.parent.name