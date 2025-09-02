"""Ultra-fast status checker for Claude Self Reflect indexing progress.

This module provides lightweight indexing status without loading heavy MCP dependencies.
Designed for <20ms execution time to support status bars and shell scripts.
"""

import json
import time
import sys
from pathlib import Path
from collections import defaultdict

# Try to import shared utilities
try:
    # Add scripts directory to path
    scripts_dir = Path(__file__).parent.parent.parent / "scripts"
    if scripts_dir.exists():
        sys.path.insert(0, str(scripts_dir))
    from shared_utils import (
        normalize_file_path, 
        extract_project_name_from_path,
        get_claude_projects_dir,
        get_csr_config_dir
    )
except ImportError:
    # Fallback implementations if shared_utils is not available
    def extract_project_name_from_path(file_path: str) -> str:
        """Extract project name from JSONL file path."""
        path_obj = Path(file_path)
        dir_name = path_obj.parent.name
        
        if dir_name.startswith('-') and 'projects' in dir_name:
            parts = dir_name.split('-')
            try:
                projects_idx = parts.index('projects')
                if projects_idx + 1 < len(parts):
                    project_parts = parts[projects_idx + 1:]
                    return '-'.join(project_parts)
            except ValueError:
                pass
        
        return dir_name.lstrip('-')
    
    def normalize_file_path(file_path: str) -> str:
        """Normalize file paths to handle Docker vs local path differences."""
        if file_path.startswith("/logs/"):
            projects_dir = str(Path.home() / ".claude" / "projects")
            return file_path.replace("/logs/", projects_dir + "/", 1)
        return file_path
    
    def get_claude_projects_dir() -> Path:
        """Get Claude projects directory."""
        import os
        if 'CLAUDE_PROJECTS_DIR' in os.environ:
            return Path(os.environ['CLAUDE_PROJECTS_DIR'])
        return Path.home() / ".claude" / "projects"
    
    def get_csr_config_dir() -> Path:
        """Get CSR config directory."""
        import os
        if 'CSR_CONFIG_DIR' in os.environ:
            return Path(os.environ['CSR_CONFIG_DIR'])
        return Path.home() / '.claude-self-reflect' / 'config'


def get_watcher_status() -> dict:
    """Get streaming watcher status if available."""
    watcher_state_file = get_csr_config_dir() / "csr-watcher.json"
    
    if not watcher_state_file.exists():
        return {"running": False, "status": "not configured"}
    
    try:
        with open(watcher_state_file) as f:
            state = json.load(f)
            
        # Check if watcher is active (modified recently)
        file_age = time.time() - watcher_state_file.stat().st_mtime
        is_active = file_age < 120  # Active if updated in last 2 minutes
        
        return {
            "running": is_active,
            "files_processed": len(state.get("imported_files", {})),
            "last_update_seconds": int(file_age),
            "status": "🟢 active" if is_active else "🔴 inactive"
        }
    except:
        return {"running": False, "status": "error reading state"}


def get_status() -> dict:
    """Get indexing status with overall stats and per-project breakdown.
    
    Returns:
        dict: JSON structure with overall and per-project indexing status, plus watcher status
    """
    projects_dir = get_claude_projects_dir()
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
    
    # Track which files have been counted to avoid duplicates
    counted_files = set()
    
    # Read imported-files.json to count indexed files per project
    config_dir = get_csr_config_dir()
    imported_files_path = config_dir / "imported-files.json"
    
    # Fallback to other locations if not found
    if not imported_files_path.exists():
        config_paths = [
            Path(__file__).parent.parent.parent / "config" / "imported-files.json",
            Path("/config/imported-files.json")  # Docker path
        ]
        
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
                    if normalized_path in file_to_project and normalized_path not in counted_files:
                        project_name = file_to_project[normalized_path]
                        project_stats[project_name]["indexed"] += 1
                        counted_files.add(normalized_path)
                
                # Also check file_metadata for partially imported files
                file_metadata = data.get("file_metadata", {})
                for file_path, metadata in file_metadata.items():
                    if isinstance(metadata, dict) and metadata.get("position", 0) > 0:
                        # Only count if not already in imported_files
                        if file_path not in imported_files:
                            normalized_path = normalize_file_path(file_path)
                            if normalized_path in file_to_project and normalized_path not in counted_files:
                                project_name = file_to_project[normalized_path]
                                project_stats[project_name]["indexed"] += 1
                                counted_files.add(normalized_path)
                
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
                            if normalized_path in file_to_project and normalized_path not in counted_files:
                                project_name = file_to_project[normalized_path]
                                project_stats[project_name]["indexed"] += 1
                                counted_files.add(normalized_path)
        except (json.JSONDecodeError, KeyError, OSError):
            # If config file is corrupted or unreadable, continue with zero indexed counts
            pass
    
    # Also read csr-watcher.json to count files imported by the watcher
    watcher_state_file = config_dir / "csr-watcher.json"
    if watcher_state_file.exists():
        try:
            with open(watcher_state_file, 'r') as f:
                watcher_data = json.load(f)
                
                # Count files imported by the watcher
                watcher_imports = watcher_data.get("imported_files", {})
                for file_path in watcher_imports.keys():
                    normalized_path = normalize_file_path(file_path)
                    if normalized_path in file_to_project and normalized_path not in counted_files:
                        project_name = file_to_project[normalized_path]
                        project_stats[project_name]["indexed"] += 1
                        counted_files.add(normalized_path)
        except (json.JSONDecodeError, KeyError, OSError):
            # If watcher file is corrupted or unreadable, continue
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
    
    # Add watcher status
    result["watcher"] = get_watcher_status()
    
    return result


if __name__ == "__main__":
    # Allow running as standalone script for testing
    import sys
    print(json.dumps(get_status(), indent=2))