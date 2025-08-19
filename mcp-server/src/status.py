"""Ultra-fast status checker for Claude Self Reflect indexing progress.

This module provides lightweight indexing status without loading heavy MCP dependencies.
Designed for <20ms execution time to support status bars and shell scripts.
"""

import json
from pathlib import Path
from collections import defaultdict


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


def normalize_file_path(file_path: str) -> str:
    """Normalize file paths to handle Docker vs local path differences.
    
    Converts:
    - /logs/PROJECT_DIR/file.jsonl -> ~/.claude/projects/PROJECT_DIR/file.jsonl
    - Already normalized paths remain unchanged
    """
    if file_path.startswith("/logs/"):
        # Convert Docker path to local path
        projects_dir = str(Path.home() / ".claude" / "projects")
        return file_path.replace("/logs/", projects_dir + "/", 1)
    return file_path


def get_status() -> dict:
    """Get indexing status with overall stats and per-project breakdown.
    
    Returns:
        dict: JSON structure with overall and per-project indexing status
    """
    projects_dir = Path.home() / ".claude" / "projects"
    project_stats = defaultdict(lambda: {"indexed": 0, "total": 0})
    
    # Build a mapping of normalized file paths to project names
    file_to_project = {}
    
    # Count total JSONL files per project
    if projects_dir.exists():
        for jsonl_file in projects_dir.glob("**/*.jsonl"):
            file_str = str(jsonl_file)
            project_name = extract_project_name_from_path(file_str)
            project_stats[project_name]["total"] += 1
            file_to_project[file_str] = project_name
    
    # Read imported-files.json to count indexed files per project
    config_paths = [
        Path.home() / ".claude-self-reflect" / "config" / "imported-files.json",
        Path(__file__).parent.parent.parent / "config" / "imported-files.json",
        Path("/config/imported-files.json")  # Docker path
    ]
    
    imported_files_path = None
    for path in config_paths:
        if path.exists():
            imported_files_path = path
            break
    
    if imported_files_path:
        try:
            with open(imported_files_path, 'r') as f:
                data = json.load(f)
                
                # The actual structure has imported_files at the top level
                imported_files = data.get("imported_files", {})
                
                # Count all files in imported_files object (they are all fully imported)
                for file_path in imported_files.keys():
                    normalized_path = normalize_file_path(file_path)
                    if normalized_path in file_to_project:
                        project_name = file_to_project[normalized_path]
                        project_stats[project_name]["indexed"] += 1
                
                # Also check file_metadata for partially imported files
                file_metadata = data.get("file_metadata", {})
                for file_path, metadata in file_metadata.items():
                    if isinstance(metadata, dict) and metadata.get("position", 0) > 0:
                        # Only count if not already in imported_files
                        if file_path not in imported_files:
                            normalized_path = normalize_file_path(file_path)
                            if normalized_path in file_to_project:
                                project_name = file_to_project[normalized_path]
                                project_stats[project_name]["indexed"] += 1
                
                # Also check stream_position if it contains file paths
                stream_position = data.get("stream_position", {})
                if isinstance(stream_position, dict):
                    for file_path in stream_position.keys():
                        # Skip non-file entries
                        if file_path in ["imported_files", "file_metadata"]:
                            continue
                        # Only count if not already counted
                        if file_path not in imported_files:
                            normalized_path = normalize_file_path(file_path)
                            if normalized_path in file_to_project:
                                project_name = file_to_project[normalized_path]
                                # Only increment if not already counted
                                if project_stats[project_name]["indexed"] < project_stats[project_name]["total"]:
                                    project_stats[project_name]["indexed"] += 1
        except (json.JSONDecodeError, KeyError, OSError):
            # If config file is corrupted or unreadable, continue with zero indexed counts
            pass
    
    # Calculate overall stats
    total_all = sum(p["total"] for p in project_stats.values())
    indexed_all = sum(p["indexed"] for p in project_stats.values())
    
    # Build response structure
    result = {
        "overall": {
            "percentage": round((indexed_all / total_all * 100) if total_all > 0 else 100.0, 1),
            "indexed": indexed_all,
            "total": total_all,
            "backlog": total_all - indexed_all
        },
        "projects": {}
    }
    
    # Add per-project stats with percentages
    for project, stats in project_stats.items():
        result["projects"][project] = {
            "percentage": round((stats["indexed"] / stats["total"] * 100) if stats["total"] > 0 else 100.0, 1),
            "indexed": stats["indexed"],
            "total": stats["total"]
        }
    
    return result


if __name__ == "__main__":
    # Allow running as standalone script for testing
    import sys
    print(json.dumps(get_status(), indent=2))