#!/usr/bin/env python3
"""
Session indexing helper for Claude Self Reflect.
Provides functions to check and import missing conversations for any project.
All imports go through Docker for consistency.
"""

import json
import os
import sys
import subprocess
from pathlib import Path
from typing import List, Set, Tuple, Optional, Dict, Any

# Import shared utilities
try:
    from shared_utils import (
        normalize_file_path, 
        get_csr_config_dir,
        get_claude_projects_dir
    )
except ImportError:
    # Fallback if shared_utils is not in path
    def normalize_file_path(file_path: str) -> str:
        """Fallback normalization function."""
        if file_path.startswith("/logs/"):
            projects_dir = str(Path.home() / ".claude" / "projects")
            return file_path.replace("/logs/", projects_dir + "/", 1)
        return file_path
    
    def get_csr_config_dir() -> Path:
        """Fallback config directory function."""
        if 'CSR_CONFIG_DIR' in os.environ:
            return Path(os.environ['CSR_CONFIG_DIR'])
        return Path.home() / '.claude-self-reflect' / 'config'
    
    def get_claude_projects_dir() -> Path:
        """Fallback projects directory function."""
        if 'CLAUDE_PROJECTS_DIR' in os.environ:
            return Path(os.environ['CLAUDE_PROJECTS_DIR'])
        return Path.home() / ".claude" / "projects"


def load_docker_manifest() -> Dict[str, Any]:
    """Load the Docker manifest configuration.
    
    Returns:
        Dict containing the manifest configuration with validation
    """
    # Find manifest in order of preference
    manifest_locations = [
        Path(__file__).parent.parent / 'config' / 'docker-manifest.json',
        get_csr_config_dir() / 'docker-manifest.json',
        Path.home() / 'projects' / 'claude-self-reflect' / 'config' / 'docker-manifest.json'
    ]
    
    data = None
    for location in manifest_locations:
        if location.exists():
            try:
                with open(location) as f:
                    data = json.load(f)
                    break
            except json.JSONDecodeError as e:
                print(f"Error reading manifest from {location}: {e}", file=sys.stderr)
    
    # Default manifest structure
    if data is None:
        data = {}
    
    # Validate and ensure minimal schema with defaults
    if not isinstance(data.get("services"), dict):
        data["services"] = {}
    
    # Ensure watcher service exists
    if "watcher" not in data["services"]:
        data["services"]["watcher"] = {
            "container_name": "claude-reflection-safe-watcher",
            "profile": "watch"
        }
    
    # Ensure import_strategy exists
    if "import_strategy" not in data or not isinstance(data["import_strategy"], dict):
        data["import_strategy"] = {"batch_size_threshold": 10}
    
    # Ensure paths exists with defaults
    data.setdefault("paths", {}).setdefault("docker_compose", "docker-compose.yaml")
    
    return data


def find_claude_self_reflect_config() -> Optional[Path]:
    """Find the Claude Self Reflect configuration directory."""
    config_dir = get_csr_config_dir()
    
    # Check if it has the expected state files
    if config_dir.exists() and config_dir.is_dir():
        if (config_dir / 'imported-files.json').exists() or \
           (config_dir / 'csr-watcher.json').exists():
            return config_dir
    
    return None


def get_project_directory(project_path: str) -> Optional[Path]:
    """Find the Claude project directory for a given project path."""
    # Resolve to absolute path first for consistency
    p = Path(project_path).resolve()
    
    # Derive the normalized project name from the resolved path
    # This matches how Claude stores projects
    normalized_name = str(p).replace('/', '-').lstrip('-')
    project_name = p.name
    
    # Check all possible project directories
    projects_dir = get_claude_projects_dir()
    if not projects_dir.exists():
        return None
    
    possible_dirs = [
        projects_dir / project_name,
        projects_dir / normalized_name,
        projects_dir / f'-{normalized_name}'
    ]
    
    # Find the actual project directory
    for d in possible_dirs:
        if d.exists() and d.is_dir():
            return d
    
    return None


def get_imported_files(config_dir: Path, project_dir: Path) -> Set[str]:
    """Get the set of already imported files from state files.
    
    Args:
        config_dir: Configuration directory containing state files
        project_dir: Project directory to match files against
        
    Returns:
        Set of full normalized paths that have been imported
    """
    imported_set = set()
    proj_dir = project_dir.resolve()
    
    # Check both imported-files.json and csr-watcher.json
    state_files = [
        config_dir / 'imported-files.json',
        config_dir / 'csr-watcher.json'
    ]
    
    for state_file in state_files:
        if state_file.exists():
            try:
                with open(state_file) as f:
                    data = json.load(f)
                    imported_files = data.get('imported_files', {})
                    for filepath in imported_files.keys():
                        # Normalize the path for comparison
                        normalized_path = normalize_file_path(filepath)
                        # Resolve path for robust comparison
                        np = Path(normalized_path).expanduser().resolve(strict=False)
                        # Only add if it belongs to this project using proper path comparison
                        try:
                            if np.is_relative_to(proj_dir):
                                imported_set.add(str(np))
                        except AttributeError:
                            # Python < 3.9 fallback
                            if str(np).startswith(str(proj_dir) + os.sep):
                                imported_set.add(str(np))
                        except (ValueError, TypeError):
                            # Path might not be valid for comparison, skip it
                            pass
            except (json.JSONDecodeError, KeyError) as e:
                print(f"Warning: Could not read {state_file}: {e}", file=sys.stderr)
    
    return imported_set


def find_missing_files(project_dir: Path, imported_set: Set[str]) -> List[Path]:
    """Find JSONL files that haven't been imported yet.
    
    Args:
        project_dir: Directory to search for JSONL files
        imported_set: Set of full normalized paths that have been imported
        
    Returns:
        List of Path objects for files that haven't been imported
    """
    missing_files = []
    
    # Use rglob to find JSONL files in nested directories
    for jsonl_file in project_dir.rglob('*.jsonl'):
        # Compare using resolved path for consistency
        full_path = str(jsonl_file.resolve())
        if full_path not in imported_set:
            missing_files.append(jsonl_file)
    
    return missing_files


def check_project_status(project_path: str) -> Tuple[int, int, List[Path]]:
    """
    Check the indexing status of a project.
    
    Returns:
        Tuple of (total_files, imported_count, missing_files_list)
    """
    # Find project directory
    project_dir = get_project_directory(project_path)
    if not project_dir:
        print(f"No Claude project directory found for {project_path}", file=sys.stderr)
        return (0, 0, [])
    
    # Find config directory
    config_dir = find_claude_self_reflect_config()
    if not config_dir:
        print("Warning: Could not find Claude Self Reflect config directory", file=sys.stderr)
        # Continue anyway - maybe nothing is imported yet
        imported_set = set()
    else:
        imported_set = get_imported_files(config_dir, project_dir)
    
    # Count files using rglob for nested directories
    jsonl_files = list(project_dir.rglob('*.jsonl'))
    total_files = len(jsonl_files)
    
    # Find missing files
    missing_files = find_missing_files(project_dir, imported_set)
    imported_count = total_files - len(missing_files)
    
    return (total_files, imported_count, missing_files)


def _get_compose_command() -> List[str]:
    """
    Detect and return the appropriate docker compose command.
    Supports both docker-compose (v1) and docker compose (v2).
    
    Returns:
        List of command parts for docker compose
    """
    for cmd in (["docker-compose"], ["docker", "compose"]):
        try:
            subprocess.run(cmd + ["version"], capture_output=True, timeout=2, check=False)
            return cmd
        except (FileNotFoundError, subprocess.TimeoutExpired):
            continue
    raise FileNotFoundError("No docker compose command found. Please ensure Docker is installed and either 'docker-compose' or 'docker compose' is available.")


def import_files(files: List[Path], limit: Optional[int] = None) -> bool:
    """
    Import files using Docker watcher based on manifest configuration.
    All imports MUST go through Docker to maintain consistency.
    
    Args:
        files: List of file paths to import
        limit: Maximum number of files (uses manifest default if not specified)
        
    Returns:
        True if watcher was started or already running, False otherwise
    """
    if not files:
        return True
    
    # Load manifest for configuration
    manifest = load_docker_manifest()
    
    # Get batch size threshold from manifest if not specified
    if limit is None:
        limit = manifest.get('import_strategy', {}).get('batch_size_threshold', 10)
    
    # Find claude-self-reflect project root
    csr_locations = [
        Path.home() / 'projects' / 'claude-self-reflect',
        Path.home() / 'claude-self-reflect',
        Path('/usr/local/claude-self-reflect')
    ]
    
    csr_root = None
    for location in csr_locations:
        compose_file = location / manifest.get('paths', {}).get('docker_compose', 'docker-compose.yaml')
        if compose_file.exists():
            csr_root = location
            break
    
    if not csr_root:
        print("Could not find claude-self-reflect docker-compose.yaml", file=sys.stderr)
        return False
    
    # Get watcher configuration from manifest
    watcher_config = manifest.get('services', {}).get('watcher', {})
    container_name = watcher_config.get('container_name', 'claude-reflection-safe-watcher')
    profile = watcher_config.get('profile', 'watch')
    
    try:
        # Use advisory lock to prevent concurrent Docker operations
        import fcntl
        lock_path = get_csr_config_dir() / 'watcher.start.lock'
        lock_path.parent.mkdir(parents=True, exist_ok=True)
        
        with open(lock_path, 'w') as lock_file:
            try:
                # Try to acquire exclusive lock (non-blocking)
                fcntl.flock(lock_file, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                # Another process is starting the watcher
                return True
            
            # Check if watcher is already running
            check_cmd = ["docker", "ps", "--filter", f"name={container_name}", "--format", "{{.Names}}"]
            result = subprocess.run(check_cmd, capture_output=True, text=True, timeout=2)
            
            # Exact name matching to avoid false positives
            names = [n.strip() for n in result.stdout.splitlines() if n.strip()]
            if container_name in names:
                # Silent - watcher already running
                return True
            
            # Get the appropriate compose command (v1 or v2)
            try:
                compose_cmd = _get_compose_command()
            except FileNotFoundError as e:
                print(str(e), file=sys.stderr)
                return False
            
            # Start the watcher using docker-compose with profile
            start_cmd = compose_cmd + [
                "-f", str(csr_root / manifest.get('paths', {}).get('docker_compose', 'docker-compose.yaml')),
                "--profile", profile,
                "up", "-d", watcher_config.get('name', 'safe-watcher')
            ]
            
            result = subprocess.run(
                start_cmd,
                capture_output=True,
                text=True,
                timeout=10  # Short timeout
            )
            
            if result.returncode == 0:
                # Silent success - progress bar shows the status
                return True
            else:
                # Only show error in verbose mode
                if os.environ.get('CSR_QUIET_MODE', 'true') != 'true':
                    print(f"Failed to start Docker watcher: {result.stderr}", file=sys.stderr)
                return False
    except subprocess.TimeoutExpired:
        # Docker compose started in background
        print("Docker watcher starting in background")
        return True
    except FileNotFoundError:
        print("Docker or docker-compose not found. Please ensure Docker is installed.", file=sys.stderr)
        return False
    except Exception as e:
        print(f"Error starting Docker watcher: {e}", file=sys.stderr)
        return False


def is_watcher_running() -> bool:
    """Check if the Docker watcher is running based on manifest.
    
    Returns:
        True if the configured watcher container is running
    """
    # Load manifest to get the correct container name
    manifest = load_docker_manifest()
    watcher_config = manifest.get('services', {}).get('watcher', {})
    container_name = watcher_config.get('container_name', 'claude-reflection-safe-watcher')
    
    # Check if the Docker container is running
    try:
        result = subprocess.run(
            ["docker", "ps", "--filter", f"name={container_name}", "--format", "{{.Names}}"],
            capture_output=True,
            text=True,
            timeout=2
        )
        
        # Exact name matching to avoid false positives
        names = [n.strip() for n in result.stdout.splitlines() if n.strip()]
        if container_name in names:
            return True
                
    except (FileNotFoundError, subprocess.TimeoutExpired):
        pass
    
    # Fallback: Check watcher state file modification time
    state_file_name = manifest.get('import_strategy', {}).get('watcher_state_file', 'csr-watcher.json')
    watcher_state = get_csr_config_dir() / state_file_name
    
    if watcher_state.exists():
        try:
            import time
            file_age = time.time() - watcher_state.stat().st_mtime
            # Get check interval from manifest
            check_interval = manifest.get('import_strategy', {}).get('watcher_check_interval', 60) * 2
            # Consider active if updated recently
            if file_age < check_interval:
                return True
        except OSError:
            pass
    
    return False


if __name__ == '__main__':
    # For testing - check current directory
    if len(sys.argv) > 1:
        project_path = sys.argv[1]
    else:
        project_path = os.getcwd()
    
    total, imported, missing = check_project_status(project_path)
    
    print(f"Project: {Path(project_path).name}")
    print(f"Total files: {total}")
    print(f"Imported: {imported}")
    print(f"Missing: {len(missing)}")
    
    if total > 0:
        percentage = (imported / total * 100)
        print(f"Indexing: {percentage:.1f}%")
    
    if missing and len(missing) <= 5:
        print(f"\nMissing files:")
        for f in missing[:5]:
            print(f"  - {f.name}")