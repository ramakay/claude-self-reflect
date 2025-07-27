#!/usr/bin/env python3
"""
File system watcher for automatic conversation imports.
Monitors Claude projects directory for new/modified JSONL files.
"""

import os
import sys
import time
import json
import logging
from datetime import datetime, timedelta
from pathlib import Path
from typing import Dict, Set
import subprocess

# Configuration
WATCH_DIR = os.getenv("WATCH_DIR", "/logs")
STATE_FILE = os.getenv("STATE_FILE", "/config/imported-files.json")
WATCH_INTERVAL = int(os.getenv("WATCH_INTERVAL", "60"))  # seconds
IMPORT_DELAY = int(os.getenv("IMPORT_DELAY", "30"))  # Wait before importing new files
IMPORTER_SCRIPT = "/scripts/import-conversations-unified.py"

# Set up logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - [Watcher] %(message)s'
)
logger = logging.getLogger(__name__)

class ImportWatcher:
    def __init__(self):
        """Initialize the import watcher."""
        self.watch_dir = Path(WATCH_DIR)
        self.state_file = Path(STATE_FILE)
        self.pending_imports: Dict[str, datetime] = {}
        self.last_scan = datetime.now()
        
    def load_imported_files(self) -> Set[str]:
        """Load set of already imported files."""
        imported = set()
        
        if self.state_file.exists():
            try:
                with open(self.state_file, 'r') as f:
                    state = json.load(f)
                    for project_files in state.get("projects", {}).values():
                        imported.update(project_files)
            except Exception as e:
                logger.error(f"Failed to load state: {e}")
        
        return imported
    
    def find_new_files(self, imported_files: Set[str]) -> Dict[str, Path]:
        """Find new or modified JSONL files."""
        new_files = {}
        
        for project_dir in self.watch_dir.iterdir():
            if not project_dir.is_dir():
                continue
            
            for jsonl_file in project_dir.glob("*.jsonl"):
                # Convert to relative path for comparison
                rel_path = str(jsonl_file).replace(str(self.watch_dir), "/logs")
                
                # Check if file is new or modified
                if rel_path not in imported_files:
                    mtime = datetime.fromtimestamp(jsonl_file.stat().st_mtime)
                    
                    # Only consider files modified after last scan
                    if mtime > self.last_scan - timedelta(seconds=WATCH_INTERVAL):
                        new_files[rel_path] = jsonl_file
                        logger.info(f"Found new file: {jsonl_file.name} in {project_dir.name}")
        
        return new_files
    
    def import_project(self, project_path: Path) -> bool:
        """Trigger import for a specific project."""
        try:
            logger.info(f"Starting import for project: {project_path.name}")
            
            # Run the streaming importer
            # Pass only the project directory name, not the full path
            result = subprocess.run(
                ["python", IMPORTER_SCRIPT, "--project", project_path.name],
                capture_output=True,
                text=True,
                timeout=300  # 5 minute timeout
            )
            
            if result.returncode == 0:
                logger.info(f"Successfully imported project: {project_path.name}")
                return True
            else:
                logger.error(f"Import failed for {project_path.name}: {result.stderr}")
                return False
                
        except subprocess.TimeoutExpired:
            logger.error(f"Import timeout for project: {project_path.name}")
            return False
        except Exception as e:
            logger.error(f"Import error for {project_path.name}: {e}")
            return False
    
    def process_pending_imports(self):
        """Process files that are ready for import."""
        current_time = datetime.now()
        projects_to_import = set()
        
        # Check which files are ready for import
        for file_path, added_time in list(self.pending_imports.items()):
            if current_time - added_time >= timedelta(seconds=IMPORT_DELAY):
                project_path = Path(file_path).parent
                projects_to_import.add(project_path)
                del self.pending_imports[file_path]
        
        # Import each project
        for project_path in projects_to_import:
            self.import_project(project_path)
    
    def run(self):
        """Main watch loop."""
        logger.info(f"Starting import watcher on {self.watch_dir}")
        logger.info(f"Scan interval: {WATCH_INTERVAL}s, Import delay: {IMPORT_DELAY}s")
        
        # Initial full import
        logger.info("Running initial full import...")
        subprocess.run(["python", IMPORTER_SCRIPT], timeout=3600)
        
        while True:
            try:
                # Load current import state
                imported_files = self.load_imported_files()
                
                # Find new files
                new_files = self.find_new_files(imported_files)
                
                # Add new files to pending
                for file_path, full_path in new_files.items():
                    if file_path not in self.pending_imports:
                        self.pending_imports[file_path] = datetime.now()
                        logger.info(f"Queued for import: {full_path.name}")
                
                # Process pending imports
                self.process_pending_imports()
                
                # Update last scan time
                self.last_scan = datetime.now()
                
                # Log status
                if self.pending_imports:
                    logger.info(f"Files pending import: {len(self.pending_imports)}")
                
                # Wait for next scan
                time.sleep(WATCH_INTERVAL)
                
            except KeyboardInterrupt:
                logger.info("Watcher stopped by user")
                break
            except Exception as e:
                logger.error(f"Watcher error: {e}")
                time.sleep(WATCH_INTERVAL)

def main():
    """Main entry point."""
    watcher = ImportWatcher()
    watcher.run()

if __name__ == "__main__":
    main()