#!/usr/bin/env python3
"""
Quick import script for current project's latest conversations.
Designed for PreCompact hook integration - targets <10 second imports.
"""

import os
import sys
import json
import subprocess
from datetime import datetime, timedelta
from pathlib import Path
import logging

# Configuration
LOGS_DIR = os.getenv("LOGS_DIR", os.path.expanduser("~/.claude/projects"))
STATE_FILE = os.getenv("STATE_FILE", os.path.expanduser("~/.claude-self-reflect-state.json"))
HOURS_BACK = int(os.getenv("IMPORT_HOURS_BACK", "2"))  # Only import last 2 hours by default

# Set up logging
logging.basicConfig(
    level=logging.INFO, 
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

def load_state():
    """Load import state from file."""
    if os.path.exists(STATE_FILE):
        try:
            with open(STATE_FILE, 'r') as f:
                return json.load(f)
        except:
            return {}
    return {}

def save_state(state):
    """Save import state to file."""
    os.makedirs(os.path.dirname(STATE_FILE), exist_ok=True)
    with open(STATE_FILE, 'w') as f:
        json.dump(state, f, indent=2)

def get_project_from_cwd():
    """Detect project from current working directory."""
    cwd = os.getcwd()
    # Convert path to project name format used in logs
    # Claude logs use format: -Users-username-path-to-project
    project_name = cwd.replace('/', '-')
    # Keep the leading dash as that's how Claude stores it
    if not project_name.startswith('-'):
        project_name = '-' + project_name
    return project_name

def get_recent_files(project_path: Path, hours_back: int):
    """Get JSONL files modified in the last N hours."""
    cutoff_time = datetime.now() - timedelta(hours=hours_back)
    recent_files = []
    
    for jsonl_file in project_path.glob("*.jsonl"):
        mtime = datetime.fromtimestamp(jsonl_file.stat().st_mtime)
        if mtime > cutoff_time:
            recent_files.append(jsonl_file)
    
    return sorted(recent_files, key=lambda f: f.stat().st_mtime, reverse=True)

def main():
    """Main quick import function."""
    start_time = datetime.now()
    
    # Detect current project
    project_name = get_project_from_cwd()
    project_path = Path(LOGS_DIR) / project_name
    
    if not project_path.exists():
        logger.warning(f"Project logs not found: {project_path}")
        logger.info("Make sure you're in a project directory with Claude conversations.")
        return
    
    logger.info(f"Quick importing latest conversations for: {project_name}")
    
    # Get recent files
    recent_files = get_recent_files(project_path, HOURS_BACK)
    logger.info(f"Found {len(recent_files)} files modified in last {HOURS_BACK} hours")
    
    if not recent_files:
        logger.info("No recent conversations to import")
        return
    
    # For now, just call the unified importer with the specific project
    # This is a temporary solution until we implement incremental imports
    script_dir = os.path.dirname(os.path.abspath(__file__))
    unified_script = os.path.join(script_dir, "import-conversations-unified.py")
    
    # Set environment to only process this project
    env = os.environ.copy()
    env['LOGS_DIR'] = str(project_path.parent)
    env['IMPORT_PROJECT'] = project_name
    
    try:
        # Run the unified importer for just this project
        result = subprocess.run(
            [sys.executable, unified_script],
            env=env,
            capture_output=True,
            text=True,
            timeout=60  # 60 second timeout
        )
        
        if result.returncode == 0:
            logger.info("Quick import completed successfully")
        else:
            logger.error(f"Import failed: {result.stderr}")
            
    except subprocess.TimeoutExpired:
        logger.warning("Import timed out after 60 seconds")
    except Exception as e:
        logger.error(f"Error during import: {e}")
    
    # Report timing
    elapsed = (datetime.now() - start_time).total_seconds()
    logger.info(f"Quick import completed in {elapsed:.1f} seconds")

if __name__ == "__main__":
    main()