"""Utility functions and ProjectResolver for Claude Self-Reflect MCP server."""

import os
import hashlib
import re
from pathlib import Path
from typing import Optional, List, Tuple
from config import logger, CLAUDE_PROJECTS_PATH

class ProjectResolver:
    """Resolves project names and paths for Claude conversations."""
    
    @staticmethod
    def get_current_project() -> Optional[str]:
        """Get the current project name from working directory."""
        cwd = os.getcwd()
        
        # Check if we're in a known project directory
        if '/projects/' in cwd or '/repos/' in cwd or '/code/' in cwd:
            # Extract project name from path
            parts = cwd.split('/')
            for i, part in enumerate(parts):
                if part in ['projects', 'repos', 'code'] and i + 1 < len(parts):
                    return parts[i + 1]
        
        # Fall back to last directory name
        return Path(cwd).name
    
    @staticmethod
    def normalize_project_name(project_name: str) -> str:
        """Normalize project name for consistent matching."""
        # Remove common prefixes/suffixes
        name = project_name
        for prefix in ['/Users/', '/home/', 'projects/', 'repos/', 'code/']:
            if name.startswith(prefix):
                name = name[len(prefix):]
        
        # Convert path separators to underscores
        name = name.replace('/', '_').replace('-', '_')
        
        # Remove trailing underscores
        name = name.rstrip('_')
        
        return name
    
    @staticmethod
    def get_project_hash(project_name: str) -> str:
        """Get hash for project name (used in collection naming)."""
        normalized = ProjectResolver.normalize_project_name(project_name)
        return hashlib.md5(normalized.encode()).hexdigest()[:8]
    
    @staticmethod
    def find_project_collections(
        all_collections: List[str], 
        project_name: str
    ) -> List[str]:
        """Find collections belonging to a specific project."""
        normalized = ProjectResolver.normalize_project_name(project_name)
        project_hash = ProjectResolver.get_project_hash(project_name)
        
        matching = []
        for collection in all_collections:
            # Check if collection matches project hash
            if collection.startswith(f"conv_{project_hash}_"):
                matching.append(collection)
            # Also check for project name in collection
            elif normalized in collection.replace('-', '_'):
                matching.append(collection)
        
        return matching
    
    @staticmethod
    def extract_project_from_collection(collection_name: str) -> str:
        """Extract project name from collection name."""
        # Remove conv_ prefix and suffixes
        name = collection_name
        if name.startswith('conv_'):
            name = name[5:]
        
        # Remove hash prefix if present
        if '_' in name and len(name.split('_')[0]) == 8:
            # Likely a hash, remove it
            parts = name.split('_', 1)
            if len(parts) > 1:
                name = parts[1]
        
        # Remove embedding type suffix
        for suffix in ['_voyage', '_local']:
            if name.endswith(suffix):
                name = name[:-len(suffix)]
        
        return name

def parse_natural_language_time(time_str: str) -> Tuple[Optional[str], Optional[str]]:
    """Parse natural language time strings into ISO timestamps."""
    from datetime import datetime, timedelta, timezone
    
    now = datetime.now(timezone.utc)
    time_str_lower = time_str.lower().strip()
    
    # Handle relative times
    if 'yesterday' in time_str_lower:
        start = (now - timedelta(days=1)).replace(hour=0, minute=0, second=0)
        end = start + timedelta(days=1)
        return start.isoformat(), end.isoformat()
    
    if 'today' in time_str_lower:
        start = now.replace(hour=0, minute=0, second=0)
        return start.isoformat(), now.isoformat()
    
    # Parse "last X" patterns
    last_match = re.match(r'last (\d+) (hour|day|week|month)', time_str_lower)
    if last_match:
        amount = int(last_match.group(1))
        unit = last_match.group(2)
        
        if unit == 'hour':
            delta = timedelta(hours=amount)
        elif unit == 'day':
            delta = timedelta(days=amount)
        elif unit == 'week':
            delta = timedelta(weeks=amount)
        elif unit == 'month':
            delta = timedelta(days=amount * 30)
        else:
            delta = timedelta(days=7)
        
        start = now - delta
        return start.isoformat(), now.isoformat()
    
    # Parse "past X" patterns
    past_match = re.match(r'past (\d+) (hour|day|week|month)', time_str_lower)
    if past_match:
        amount = int(past_match.group(1))
        unit = past_match.group(2)
        
        if unit == 'hour':
            delta = timedelta(hours=amount)
        elif unit == 'day':
            delta = timedelta(days=amount)
        elif unit == 'week':
            delta = timedelta(weeks=amount)
        elif unit == 'month':
            delta = timedelta(days=amount * 30)
        else:
            delta = timedelta(days=7)
        
        start = now - delta
        return start.isoformat(), now.isoformat()
    
    # Default to last week
    if 'week' in time_str_lower:
        start = now - timedelta(days=7)
        return start.isoformat(), now.isoformat()
    
    # Default to last 24 hours
    start = now - timedelta(days=1)
    return start.isoformat(), now.isoformat()

def escape_xml(text: str, attr: bool = False) -> str:
    """Escape text for XML output."""
    from xml.sax.saxutils import escape
    
    if attr:
        # For attributes, also escape quotes
        return escape(text, {'"': '&quot;'})
    return escape(text)