"""State management with atomic writes."""

import json
import logging
import os
from pathlib import Path
from typing import Dict, Any, Optional, Set
from datetime import datetime
import fcntl
import tempfile

logger = logging.getLogger(__name__)


class StateManager:
    """
    Manage import state with atomic writes and crash recovery.
    
    Implements the critical STATE_FILE fixes:
    1. Handle empty dirname case
    2. Atomic writes to prevent corruption
    3. File locking for concurrent access
    """
    
    def __init__(self, state_file: Path):
        self.state_file = Path(state_file)
        self.state: Dict[str, Any] = self._load_state()
        self._lock_file = None
    
    def _load_state(self) -> Dict[str, Any]:
        """Load state from file or create new."""
        if self.state_file.exists():
            try:
                with open(self.state_file, 'r') as f:
                    state = json.load(f)
                    logger.debug(f"Loaded state with {len(state.get('processed', []))} processed files")
                    return state
            except Exception as e:
                logger.error(f"Failed to load state: {e}")
                # Create backup of corrupted state
                backup = self.state_file.with_suffix('.corrupt')
                try:
                    self.state_file.rename(backup)
                    logger.info(f"Backed up corrupted state to {backup}")
                except Exception:
                    pass
        
        # Return default state
        return {
            "processed": {},
            "failed": {},
            "last_updated": None,
            "version": "3.0.0"
        }
    
    def save_state(self) -> None:
        """
        Save state with atomic write.
        
        Implements the critical fix for STATE_FILE directory handling.
        """
        try:
            # FIX: Handle case where STATE_FILE has no directory component
            state_dir = self.state_file.parent
            if state_dir and state_dir != Path('.'):
                state_dir.mkdir(parents=True, exist_ok=True)
            
            # Update timestamp
            self.state["last_updated"] = datetime.now().isoformat()
            
            # Use atomic write to prevent corruption during crashes
            # Create temp file in same directory for atomic rename
            temp_fd, temp_path = tempfile.mkstemp(
                dir=str(state_dir) if state_dir else '.',
                prefix='.tmp_state_',
                suffix='.json'
            )
            
            try:
                # Write to temp file
                with os.fdopen(temp_fd, 'w') as f:
                    json.dump(self.state, f, indent=2)
                
                # Atomic rename (on POSIX systems)
                Path(temp_path).replace(self.state_file)
                
                logger.debug("State saved successfully")
                
            except Exception as e:
                # Clean up temp file on error
                try:
                    os.unlink(temp_path)
                except:
                    pass
                raise e
                
        except Exception as e:
            logger.error(f"Failed to save state: {e}")
            raise
    
    def is_processed(self, file_path: Path) -> bool:
        """Check if file has been processed."""
        return str(file_path) in self.state.get("processed", {})
    
    def mark_processed(self, file_path: Path, points_created: int) -> None:
        """Mark file as processed."""
        if "processed" not in self.state:
            self.state["processed"] = {}
        
        self.state["processed"][str(file_path)] = {
            "timestamp": datetime.now().isoformat(),
            "points_created": points_created
        }
        
        # Remove from failed if present
        if str(file_path) in self.state.get("failed", {}):
            del self.state["failed"][str(file_path)]
        
        self.save_state()
    
    def mark_failed(self, file_path: Path, error: str) -> None:
        """Mark file as failed."""
        if "failed" not in self.state:
            self.state["failed"] = {}
        
        self.state["failed"][str(file_path)] = {
            "timestamp": datetime.now().isoformat(),
            "error": error
        }
        
        self.save_state()
    
    def get_processed_files(self) -> Set[str]:
        """Get set of processed file paths."""
        return set(self.state.get("processed", {}).keys())
    
    def get_failed_files(self) -> Set[str]:
        """Get set of failed file paths."""
        return set(self.state.get("failed", {}).keys())
    
    def reset(self) -> None:
        """Reset state to empty."""
        self.state = {
            "processed": {},
            "failed": {},
            "last_updated": datetime.now().isoformat(),
            "version": "3.0.0"
        }
        self.save_state()
    
    def get_statistics(self) -> Dict[str, Any]:
        """Get import statistics."""
        processed = self.state.get("processed", {})
        failed = self.state.get("failed", {})
        
        total_points = sum(
            info.get("points_created", 0) 
            for info in processed.values()
        )
        
        return {
            "processed_count": len(processed),
            "failed_count": len(failed),
            "total_points": total_points,
            "last_updated": self.state.get("last_updated"),
            "version": self.state.get("version")
        }
    
    def acquire_lock(self) -> bool:
        """Acquire exclusive lock for state file (Unix/Linux only)."""
        if os.name == 'nt':  # Windows
            logger.warning("File locking not implemented for Windows")
            return True  # Allow operation to proceed without locking on Windows
        try:
            lock_path = self.state_file.with_suffix('.lock')
            self._lock_file = open(lock_path, 'w')
            fcntl.flock(self._lock_file.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            return True
        except (IOError, OSError):
            return False
    
    def release_lock(self) -> None:
        """Release state file lock."""
        if self._lock_file:
            try:
                fcntl.flock(self._lock_file.fileno(), fcntl.LOCK_UN)
                self._lock_file.close()
            except:
                pass
            self._lock_file = None