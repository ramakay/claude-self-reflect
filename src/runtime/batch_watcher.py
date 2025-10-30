#!/usr/bin/env python3
"""
Batch-Aware Watcher Service - Enhanced version of streaming-watcher.py
Integrates with Anthropic Batch API for narrative generation.

This service:
1. Watches for new conversation JSONL files
2. Accumulates conversations in a batch queue
3. Triggers batch narrative generation every 10 files OR every 30 minutes
4. Registers batches with batch_monitor for automated evaluation
5. Maintains hot/warm/cold priority system for responsiveness
"""

import os
import sys
import time
import json
import logging
import fcntl
from pathlib import Path
from typing import Dict, List, Set
from datetime import datetime, timedelta
from dataclasses import dataclass, field
from collections import deque

# Add project root to path
sys.path.insert(0, str(Path(__file__).parent.parent.parent))

from dotenv import load_dotenv
load_dotenv()

# Import existing infrastructure
sys.path.insert(0, str(Path(__file__).parent))
from unified_state_manager import UnifiedStateManager
from utils import normalize_project_name

# Import batch monitor and centralized config
sys.path.insert(0, str(Path(__file__).parent))
from batch_monitor import BatchMonitor
from config import (
    CSR_CONFIG_DIR,
    CSR_BATCH_QUEUE_DIR,
    CLAUDE_PROJECTS_DIR,
    BATCH_SIZE_TRIGGER,
    BATCH_TIME_TRIGGER_MINUTES,
    HOT_WINDOW_MINUTES,
    WARM_WINDOW_HOURS,
    MAX_COLD_FILES,
    HOT_CHECK_INTERVAL_S,
    NORMAL_CHECK_INTERVAL_S,
    SUBPROCESS_TIMEOUT_SECONDS
)

# Path to batch scripts
BATCH_IMPORT_SCRIPT = Path(__file__).parent.parent.parent / "docs" / "design" / "batch_import_all_projects.py"

logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)


@dataclass
class BatchWatcherConfig:
    """Configuration for batch-aware watcher."""

    # Directories (from centralized config)
    logs_dir: Path = field(default_factory=lambda: CLAUDE_PROJECTS_DIR)
    queue_dir: Path = field(default_factory=lambda: CSR_BATCH_QUEUE_DIR)

    # Batch triggers (from centralized config)
    batch_size_trigger: int = field(default_factory=lambda: BATCH_SIZE_TRIGGER)
    batch_time_trigger_minutes: int = field(default_factory=lambda: BATCH_TIME_TRIGGER_MINUTES)

    # Priority thresholds (from centralized config)
    hot_window_minutes: int = field(default_factory=lambda: HOT_WINDOW_MINUTES)
    warm_window_hours: int = field(default_factory=lambda: WARM_WINDOW_HOURS)
    max_cold_files: int = field(default_factory=lambda: MAX_COLD_FILES)

    # Timing (from centralized config)
    hot_check_interval_s: int = field(default_factory=lambda: HOT_CHECK_INTERVAL_S)
    normal_check_interval_s: int = field(default_factory=lambda: NORMAL_CHECK_INTERVAL_S)

    # State files (from centralized config)
    state_file: Path = field(default_factory=lambda: CSR_CONFIG_DIR / "batch-watcher.json")
    queue_state_file: Path = field(default_factory=lambda: CSR_BATCH_QUEUE_DIR / "queue-state.json")


class BatchQueue:
    """Manages the queue of conversations waiting for batch processing."""

    def __init__(self, config: BatchWatcherConfig):
        self.config = config
        self.config.queue_dir.mkdir(parents=True, exist_ok=True)
        self.queue_state_file = config.queue_state_file

        # Load existing queue and last batch time
        queue_state = self._load_queue_state()
        self.queue = queue_state.get('queued_files', [])

        # Restore last_batch_time from state, or use current time if not available
        last_batch_str = queue_state.get('last_batch_time')
        if last_batch_str:
            try:
                self.last_batch_time = datetime.fromisoformat(last_batch_str)
            except (ValueError, TypeError):
                self.last_batch_time = datetime.now()
        else:
            self.last_batch_time = datetime.now()

    def _load_queue_state(self) -> Dict:
        """Load queue state from file with file locking."""
        if not self.queue_state_file.exists():
            return {"queued_files": [], "last_batch_time": None}

        try:
            with open(self.queue_state_file, 'r', encoding='utf-8') as f:
                # Acquire shared lock for reading
                fcntl.flock(f.fileno(), fcntl.LOCK_SH)
                try:
                    data = json.load(f)
                    return data
                finally:
                    fcntl.flock(f.fileno(), fcntl.LOCK_UN)
        except FileNotFoundError:
            return {"queued_files": [], "last_batch_time": None}
        except Exception as e:
            logger.error(f"Error loading queue: {e}")
            return {"queued_files": [], "last_batch_time": None}

    def _save_queue(self):
        """Save queue state to file with exclusive file locking."""
        self.queue_state_file.parent.mkdir(parents=True, exist_ok=True)

        try:
            # Use atomic write: write to temp file, then rename
            temp_file = self.queue_state_file.with_suffix('.tmp')

            with open(temp_file, 'w', encoding='utf-8') as f:
                # Acquire exclusive lock for writing
                fcntl.flock(f.fileno(), fcntl.LOCK_EX)
                try:
                    json.dump({
                        "queued_files": self.queue,
                        "last_batch_time": self.last_batch_time.isoformat(),
                        "queue_size": len(self.queue)
                    }, f, indent=2)
                    f.flush()
                    os.fsync(f.fileno())
                finally:
                    fcntl.flock(f.fileno(), fcntl.LOCK_UN)

            # Atomic rename (POSIX guarantees atomicity)
            temp_file.replace(self.queue_state_file)

        except Exception as e:
            logger.error(f"Error saving queue: {e}")
            # Clean up temp file if it exists
            if temp_file.exists():
                temp_file.unlink()

    def add(self, file_path: str, project: str):
        """Add a conversation to the queue."""
        entry = {
            "file_path": file_path,
            "project": project,
            "queued_at": datetime.now().isoformat()
        }

        # Avoid duplicates
        if not any(q["file_path"] == file_path for q in self.queue):
            self.queue.append(entry)
            self._save_queue()
            logger.info(f"📝 Queued for batch: {file_path} (queue size: {len(self.queue)})")

    def should_trigger_batch(self) -> bool:
        """Check if batch should be triggered."""
        # Size trigger
        if len(self.queue) >= self.config.batch_size_trigger:
            logger.info(f"🎯 Batch size trigger: {len(self.queue)} >= {self.config.batch_size_trigger}")
            return True

        # Time trigger (and queue not empty)
        if len(self.queue) > 0:
            time_since_last = datetime.now() - self.last_batch_time
            if time_since_last > timedelta(minutes=self.config.batch_time_trigger_minutes):
                logger.info(f"⏰ Batch time trigger: {time_since_last.total_seconds()/60:.1f} min >= {self.config.batch_time_trigger_minutes} min")
                return True

        return False

    def get_batch(self) -> List[Dict]:
        """Get all queued files and clear the queue."""
        batch = self.queue.copy()
        self.queue = []
        self.last_batch_time = datetime.now()
        self._save_queue()
        return batch

    def size(self) -> int:
        """Get current queue size."""
        return len(self.queue)


class BatchWatcher:
    """Enhanced watcher that integrates with Batch API."""

    def __init__(self, config: BatchWatcherConfig):
        self.config = config
        self.state_manager = UnifiedStateManager(str(config.state_file))
        self.batch_queue = BatchQueue(config)
        self.batch_monitor = BatchMonitor()

        logger.info(f"🚀 Batch Watcher initialized")
        logger.info(f"   Watching: {config.logs_dir}")
        logger.info(f"   Batch triggers: {config.batch_size_trigger} files OR {config.batch_time_trigger_minutes} min")
        logger.info(f"   Queue state: {config.queue_state_file}")

    def _discover_files(self) -> List[tuple]:
        """Discover all JSONL conversation files with priority."""
        files = []
        now = datetime.now()

        for project_dir in self.config.logs_dir.iterdir():
            if not project_dir.is_dir():
                continue

            project_name = project_dir.name

            # Find JSONL files in project
            jsonl_files = list(project_dir.glob("*.jsonl"))

            for file_path in jsonl_files:
                try:
                    stat = file_path.stat()
                    mtime = datetime.fromtimestamp(stat.st_mtime)
                    age = now - mtime

                    # Calculate priority
                    if age < timedelta(minutes=self.config.hot_window_minutes):
                        priority = "HOT"
                    elif age < timedelta(hours=self.config.warm_window_hours):
                        priority = "WARM"
                    else:
                        priority = "COLD"

                    files.append((str(file_path), project_name, priority, age))

                except Exception as e:
                    logger.warning(f"Error checking {file_path}: {e}")

        # Sort by priority (HOT first, then WARM, then COLD by age)
        priority_order = {"HOT": 0, "WARM": 1, "COLD": 2}
        files.sort(key=lambda x: (priority_order[x[2]], x[3]))

        return files

    def _process_file(self, file_path: str, project: str, priority: str) -> bool:
        """Process a single file - add to queue or trigger batch."""
        # Check if already processed
        imported_files = self.state_manager.get_imported_files()
        normalized_path = self.state_manager.normalize_path(file_path)

        if normalized_path in imported_files:
            return False

        # HOT files: Add to queue immediately
        if priority == "HOT":
            logger.info(f"🔥 HOT file detected: {Path(file_path).name}")
            self.batch_queue.add(file_path, project)

            # Mark as queued (not processed yet, but in queue)
            # Don't mark as processed until batch completes
            return True

        # WARM/COLD files: Add to queue
        else:
            self.batch_queue.add(file_path, project)
            return True

    def _trigger_batch(self):
        """Trigger batch narrative generation."""
        batch_files = self.batch_queue.get_batch()

        if not batch_files:
            return

        logger.info(f"\n{'='*60}")
        logger.info(f"🚀 TRIGGERING BATCH NARRATIVE GENERATION")
        logger.info(f"   Files: {len(batch_files)}")
        logger.info(f"{'='*60}\n")

        try:
            import subprocess

            # Run batch import script with configurable timeout
            result = subprocess.run(
                [sys.executable, str(BATCH_IMPORT_SCRIPT)],
                capture_output=True,
                text=True,
                timeout=SUBPROCESS_TIMEOUT_SECONDS,  # Default: 1800s (30 min)
                check=True
            )

            logger.info("\n✅ Batch triggered successfully")
            logger.info("   Output:\n%s", result.stdout)

            # Mark files as processed
            for entry in batch_files:
                self.state_manager.add_imported_file(
                    file_path=entry["file_path"],
                    chunks=0,  # Will be updated by batch import
                    metadata={"batch_queued": True}
                )

        except subprocess.CalledProcessError as cpe:
            logger.error("❌ Batch import failed (rc=%s)", cpe.returncode)
            logger.error("   Stdout: %s", cpe.stdout)
            logger.error("   Stderr: %s", cpe.stderr)

            # Re-queue failed files
            for entry in batch_files:
                self.batch_queue.add(entry["file_path"], entry["project"])

        except Exception as e:
            logger.error("❌ Error triggering batch: %s", e, exc_info=True)

            # Re-queue failed files
            for entry in batch_files:
                self.batch_queue.add(entry["file_path"], entry["project"])

    def _hot_cycle(self):
        """Fast cycle to check for HOT files only."""
        files = self._discover_files()

        hot_files = [f for f in files if f[2] == "HOT"]

        if hot_files:
            logger.info(f"🔥 {len(hot_files)} HOT files detected")

            for file_path, project, priority, age in hot_files:
                self._process_file(file_path, project, priority)

            # Check if batch should trigger
            if self.batch_queue.should_trigger_batch():
                self._trigger_batch()

    def _normal_cycle(self):
        """Normal cycle to process all files."""
        files = self._discover_files()

        logger.info(f"\n📊 Scan results:")
        hot = sum(1 for f in files if f[2] == "HOT")
        warm = sum(1 for f in files if f[2] == "WARM")
        cold = sum(1 for f in files if f[2] == "COLD")
        logger.info(f"   🔥 HOT: {hot}, 🌤️  WARM: {warm}, ❄️  COLD: {cold}")
        logger.info(f"   📝 Queue size: {self.batch_queue.size()}")

        # Process files by priority
        processed = 0

        # HOT files (all of them)
        for file_path, project, priority, age in files:
            if priority == "HOT":
                if self._process_file(file_path, project, priority):
                    processed += 1

        # WARM files (all of them)
        for file_path, project, priority, age in files:
            if priority == "WARM":
                if self._process_file(file_path, project, priority):
                    processed += 1

        # COLD files (limited)
        cold_processed = 0
        for file_path, project, priority, age in files:
            if priority == "COLD" and cold_processed < self.config.max_cold_files:
                if self._process_file(file_path, project, priority):
                    processed += 1
                    cold_processed += 1

        if processed > 0:
            logger.info(f"   ✅ Queued {processed} new files")

        # Check if batch should trigger
        if self.batch_queue.should_trigger_batch():
            self._trigger_batch()

    def run_once(self):
        """Run one monitoring cycle."""
        self._normal_cycle()

        # Also check batch monitor
        self.batch_monitor.run_once()

    def run_forever(self):
        """Run watcher forever with hot/normal cycles."""
        logger.info(f"\n{'='*60}")
        logger.info(f"🚀 BATCH WATCHER STARTED")
        logger.info(f"{'='*60}")
        logger.info(f"HOT check: every {self.config.hot_check_interval_s}s")
        logger.info(f"Normal cycle: every {self.config.normal_check_interval_s}s")
        logger.info(f"{'='*60}\n")

        last_normal_cycle = time.time()

        while True:
            try:
                # HOT cycle (every 2 seconds)
                self._hot_cycle()

                # Normal cycle (every 60 seconds)
                now = time.time()
                if now - last_normal_cycle >= self.config.normal_check_interval_s:
                    self._normal_cycle()
                    last_normal_cycle = now

                # Sleep until next HOT check
                time.sleep(self.config.hot_check_interval_s)

            except KeyboardInterrupt:
                logger.info("\n👋 Batch watcher stopped")
                break
            except Exception as e:
                logger.error(f"Error in watcher loop: {e}", exc_info=True)
                time.sleep(self.config.hot_check_interval_s)


def main():
    """Main entry point."""
    import argparse

    parser = argparse.ArgumentParser(description="Batch-Aware Watcher Service")
    parser.add_argument("--once", action="store_true", help="Run once and exit")
    parser.add_argument("--batch-size", type=int, default=10, help="Batch size trigger")
    parser.add_argument("--batch-time", type=int, default=30, help="Batch time trigger (minutes)")

    args = parser.parse_args()

    config = BatchWatcherConfig(
        batch_size_trigger=args.batch_size,
        batch_time_trigger_minutes=args.batch_time
    )

    watcher = BatchWatcher(config)

    if args.once:
        watcher.run_once()
    else:
        watcher.run_forever()


if __name__ == "__main__":
    main()
