"""Extract tool usage and file references from conversations."""

import re
import logging
from typing import Dict, Any, Set, List
from pathlib import Path

logger = logging.getLogger(__name__)


class ToolUsageExtractor:
    """Extract files analyzed, edited, and tools used from conversations."""
    
    def __init__(self):
        # Patterns for file operations
        self.file_patterns = {
            'analyzed': [
                re.compile(r'(?:reading|analyzing|examining|looking at|checking)\s+(?:file\s+)?([/\w\-\.]+\.\w+)', re.IGNORECASE),
                re.compile(r'(?:Read|read)\s+([/\w\-\.]+\.\w+)', re.IGNORECASE),
                re.compile(r'(?:in|from)\s+file\s+([/\w\-\.]+\.\w+)', re.IGNORECASE)
            ],
            'edited': [
                re.compile(r'(?:editing|modifying|updating|changing|writing to)\s+(?:file\s+)?([/\w\-\.]+\.\w+)', re.IGNORECASE),
                re.compile(r'(?:Edit|Write)\s+([/\w\-\.]+\.\w+)', re.IGNORECASE),
                re.compile(r'(?:changes to|modified|updated)\s+([/\w\-\.]+\.\w+)', re.IGNORECASE)
            ],
            'created': [
                re.compile(r'(?:creating|created|new file)\s+([/\w\-\.]+\.\w+)', re.IGNORECASE),
                re.compile(r'(?:Write|Create)\s+new\s+file\s+([/\w\-\.]+\.\w+)', re.IGNORECASE)
            ]
        }
        
        # Tool patterns
        self.tool_patterns = [
            re.compile(r'(?:using|running|executing)\s+(\w+)\s+(?:tool|command)', re.IGNORECASE),
            re.compile(r'(?:Tool:|Command:)\s*(\w+)', re.IGNORECASE),
            re.compile(r'```(?:bash|shell|sh)\n([a-z]+)', re.IGNORECASE),
            re.compile(r'\$\s+([a-z]+)\s+', re.IGNORECASE),  # Command line
            re.compile(r'(?:npm|yarn|pip|cargo|go|cargo)\s+([\w\-]+)', re.IGNORECASE)
        ]
        
        # MCP tool pattern
        self.mcp_pattern = re.compile(r'mcp__([a-zA-Z0-9\-_]+)__([a-zA-Z0-9\-_]+)')
    
    def extract(self, text: str) -> Dict[str, Any]:
        """
        Extract tool usage information from text.
        
        Returns:
            Dictionary with files_analyzed, files_edited, tools_used
        """
        files_analyzed = set()
        files_edited = set()
        files_created = set()
        tools_used = set()
        
        # Extract file operations
        for pattern in self.file_patterns['analyzed']:
            matches = pattern.findall(text)
            for match in matches:
                file_path = self._normalize_file_path(match)
                if file_path:
                    files_analyzed.add(file_path)
        
        for pattern in self.file_patterns['edited']:
            matches = pattern.findall(text)
            for match in matches:
                file_path = self._normalize_file_path(match)
                if file_path:
                    files_edited.add(file_path)
        
        for pattern in self.file_patterns['created']:
            matches = pattern.findall(text)
            for match in matches:
                file_path = self._normalize_file_path(match)
                if file_path:
                    files_created.add(file_path)
        
        # Extract tools
        for pattern in self.tool_patterns:
            matches = pattern.findall(text)
            for match in matches:
                tool = match.lower().strip()
                if self._is_valid_tool(tool):
                    tools_used.add(tool)
        
        # Extract MCP tools specifically
        mcp_matches = self.mcp_pattern.findall(text)
        for server, tool in mcp_matches:
            tools_used.add(f"mcp:{server}:{tool}")
        
        # Look for common CLI tools
        common_tools = [
            'git', 'npm', 'yarn', 'pip', 'python', 'node', 'docker',
            'kubectl', 'aws', 'gcloud', 'az', 'terraform', 'ansible',
            'make', 'gradle', 'maven', 'cargo', 'go', 'rustc'
        ]
        for tool in common_tools:
            if re.search(rf'\b{tool}\b', text, re.IGNORECASE):
                tools_used.add(tool)
        
        # Combine all files for backward compatibility
        all_files = files_analyzed | files_edited | files_created
        
        return {
            "files_analyzed": list(files_analyzed)[:50],
            "files_edited": list(files_edited)[:50],
            "files_created": list(files_created)[:50],
            "files": list(all_files)[:50],  # Legacy field
            "tools_used": list(tools_used)[:30],
            "file_count": len(all_files),
            "tool_count": len(tools_used)
        }
    
    def _normalize_file_path(self, path: str) -> str:
        """Normalize and validate file path."""
        # Remove quotes and whitespace
        path = path.strip('\'"` \n')
        
        # Skip if too short or too long
        if len(path) < 3 or len(path) > 200:
            return ""
        
        # Must have an extension
        if '.' not in path:
            return ""
        
        # Extract just the filename if it's a full path
        if '/' in path:
            # Get the last component
            path = path.split('/')[-1]
        
        # Validate extension
        valid_extensions = {
            'py', 'js', 'ts', 'jsx', 'tsx', 'java', 'go', 'rs', 'cpp', 'c', 'h',
            'md', 'txt', 'json', 'yaml', 'yml', 'xml', 'html', 'css', 'scss',
            'sql', 'sh', 'bash', 'dockerfile', 'makefile', 'toml', 'ini', 'cfg'
        }
        
        ext = path.split('.')[-1].lower()
        if ext not in valid_extensions:
            return ""
        
        return path
    
    def _is_valid_tool(self, tool: str) -> bool:
        """Check if a string is a valid tool name."""
        # Skip common words
        skip_words = {
            'the', 'a', 'an', 'and', 'or', 'but', 'in', 'on', 'at', 'to', 'for',
            'this', 'that', 'with', 'from', 'as', 'is', 'was', 'are', 'were'
        }
        
        if tool in skip_words:
            return False
        
        # Must be alphanumeric with possible hyphens/underscores
        if not re.match(r'^[a-z0-9\-_]+$', tool):
            return False
        
        # Reasonable length
        if len(tool) < 2 or len(tool) > 30:
            return False
        
        return True