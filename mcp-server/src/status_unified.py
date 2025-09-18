"""Ultra-fast status checker using unified state management.

This module reads from the unified state file for indexing status.
Designed for <20ms execution time to support status bars and shell scripts.
"""

import json
import time
import sys
from pathlib import Path
from collections import defaultdict

# Add scripts directory to path for unified state manager
scripts_dir = Path(__file__).parent.parent.parent / "scripts"
if scripts_dir.exists():
    sys.path.insert(0, str(scripts_dir))

try:
    from unified_state_manager import UnifiedStateManager
except ImportError:
    # Fallback to reading JSON directly if manager not available
    UnifiedStateManager = None

# Try to import shared utilities
try:
    from shared_utils import (
        extract_project_name_from_path,
        get_claude_projects_dir,
        get_csr_config_dir
    )
except ImportError:
    # Fallback implementations
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
    """Get streaming watcher status from unified state."""
    try:
        if UnifiedStateManager:
            manager = UnifiedStateManager()
            state = manager.read_state()

            # Get watcher status from importers section
            watcher_info = state.get("importers", {}).get("streaming", {})
            last_run = watcher_info.get("last_run")

            if last_run:
                from datetime import datetime, timezone
                last_run_dt = datetime.fromisoformat(last_run)
                now = datetime.now(timezone.utc)
                age_seconds = (now - last_run_dt).total_seconds()
                is_active = age_seconds < 120  # Active if updated in last 2 minutes
            else:
                is_active = False
                age_seconds = float('inf')

            return {
                "running": is_active,
                "files_processed": watcher_info.get("files_processed", 0),
                "last_update_seconds": int(age_seconds) if age_seconds != float('inf') else None,
                "status": "🟢 active" if is_active else "🔴 inactive"
            }
        else:
            # Fallback to old method if UnifiedStateManager not available
            watcher_state_file = get_csr_config_dir() / "csr-watcher.json"

            if not watcher_state_file.exists():
                return {"running": False, "status": "not configured"}

            with open(watcher_state_file) as f:
                state = json.load(f)

            file_age = time.time() - watcher_state_file.stat().st_mtime
            is_active = file_age < 120

            return {
                "running": is_active,
                "files_processed": len(state.get("imported_files", {})),
                "last_update_seconds": int(file_age),
                "status": "🟢 active" if is_active else "🔴 inactive"
            }
    except Exception as e:
        return {"running": False, "status": f"error: {str(e)[:50]}"}


def get_status() -> dict:
    """Get indexing status from unified state with per-project breakdown.

    Returns:
        dict: JSON structure with overall and per-project indexing status
    """
    start_time = time.time()

    try:
        if UnifiedStateManager:
            # Use unified state manager for fast access
            manager = UnifiedStateManager()
            status = manager.get_status()

            # Get per-project breakdown
            project_stats = defaultdict(lambda: {"indexed": 0, "total": 0})

            # Count total JSONL files per project
            projects_dir = get_claude_projects_dir()
            if projects_dir.exists():
                for jsonl_file in projects_dir.glob("**/*.jsonl"):
                    project_name = extract_project_name_from_path(str(jsonl_file))
                    project_stats[project_name]["total"] += 1

            # Count indexed files per project from unified state
            state = manager.read_state()
            for file_path, metadata in state.get("files", {}).items():
                if metadata.get("status") == "completed":
                    project_name = extract_project_name_from_path(file_path)
                    if project_name in project_stats:
                        project_stats[project_name]["indexed"] += 1

            # Format response
            result = {
                "overall": {
                    "percentage": status["percentage"],
                    "indexed_files": status["indexed_files"],
                    "total_files": status["total_files"],
                    "total_chunks": status["total_chunks"],
                },
                "watcher": get_watcher_status(),
                "projects": dict(project_stats),
                "execution_time_ms": round((time.time() - start_time) * 1000, 2)
            }

            return result

        else:
            # Fallback to old multi-file method
            return get_status_legacy()

    except Exception as e:
        return {
            "error": str(e),
            "execution_time_ms": round((time.time() - start_time) * 1000, 2)
        }


def get_status_legacy() -> dict:
    """Legacy status method reading from multiple files (fallback)."""
    projects_dir = get_claude_projects_dir()
    project_stats = defaultdict(lambda: {"indexed": 0, "total": 0})

    # Count total JSONL files per project
    if projects_dir.exists():
        for jsonl_file in projects_dir.glob("**/*.jsonl"):
            file_str = str(jsonl_file)
            project_name = extract_project_name_from_path(file_str)
            project_stats[project_name]["total"] += 1

    # Read imported-files.json to count indexed files
    config_dir = get_csr_config_dir()
    imported_files_path = config_dir / "imported-files.json"

    if imported_files_path.exists():
        try:
            with open(imported_files_path, 'r') as f:
                data = json.load(f)
                imported_files = data.get("imported_files", {})

                for file_path in imported_files.keys():
                    # Normalize path
                    if file_path.startswith("/logs/"):
                        projects_path = str(get_claude_projects_dir())
                        normalized_path = file_path.replace("/logs/", projects_path + "/", 1)
                    else:
                        normalized_path = file_path

                    # Check if file exists and count it
                    if Path(normalized_path).exists():
                        project_name = extract_project_name_from_path(normalized_path)
                        if project_name in project_stats:
                            project_stats[project_name]["indexed"] += 1
        except Exception:
            pass

    # Calculate overall stats
    total_files = sum(p["total"] for p in project_stats.values())
    indexed_files = sum(p["indexed"] for p in project_stats.values())
    percentage = (indexed_files / max(total_files, 1)) * 100

    return {
        "overall": {
            "percentage": percentage,
            "indexed_files": indexed_files,
            "total_files": total_files
        },
        "watcher": get_watcher_status(),
        "projects": dict(project_stats)
    }


def main():
    """CLI interface for status checking."""
    import argparse

    parser = argparse.ArgumentParser(description="Check Claude Self-Reflect indexing status")
    parser.add_argument("--format", choices=["json", "text"], default="json",
                       help="Output format (default: json)")
    parser.add_argument("--watch", action="store_true",
                       help="Watch mode - update every 2 seconds")

    args = parser.parse_args()

    if args.watch:
        try:
            while True:
                status = get_status()
                if args.format == "json":
                    print(json.dumps(status, indent=2))
                else:
                    overall = status.get("overall", {})
                    print(f"Indexing: {overall.get('percentage', 0):.1f}% "
                          f"({overall.get('indexed_files', 0)}/{overall.get('total_files', 0)})")

                    watcher = status.get("watcher", {})
                    print(f"Watcher: {watcher.get('status', '🔴 inactive')}")

                    if status.get("execution_time_ms"):
                        print(f"Time: {status['execution_time_ms']}ms")

                print("\n" + "-" * 40)
                time.sleep(2)

        except KeyboardInterrupt:
            print("\nStopped")
    else:
        status = get_status()
        if args.format == "json":
            print(json.dumps(status, indent=2))
        else:
            overall = status.get("overall", {})
            print(f"Indexing: {overall.get('percentage', 0):.1f}% "
                  f"({overall.get('indexed_files', 0)}/{overall.get('total_files', 0)} files)")

            watcher = status.get("watcher", {})
            print(f"Watcher: {watcher.get('status', '🔴 inactive')}")

            # Show per-project if available
            projects = status.get("projects", {})
            if projects:
                print("\nProjects:")
                for proj, stats in projects.items():
                    pct = (stats["indexed"] / max(stats["total"], 1)) * 100
                    print(f"  {proj}: {pct:.1f}% ({stats['indexed']}/{stats['total']})")

            if status.get("execution_time_ms"):
                print(f"\nExecution time: {status['execution_time_ms']}ms")


if __name__ == "__main__":
    main()