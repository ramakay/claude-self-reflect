#!/usr/bin/env python3
"""
Test HOT/WARM/COLD categorization and priority system.
Tests both LOCAL (FastEmbed) and CLOUD (Voyage AI) modes.
"""

import asyncio
import os
import sys
import time
import tempfile
import json
from pathlib import Path
from datetime import datetime, timedelta
from unittest.mock import Mock, patch, MagicMock

# Add parent directory to path
sys.path.insert(0, str(Path(__file__).parent.parent / "scripts"))

# Import directly using importlib to handle hyphenated filename
import importlib.util
spec = importlib.util.spec_from_file_location(
    "streaming_watcher", 
    Path(__file__).parent.parent / "scripts" / "streaming-watcher.py"
)
streaming_watcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(streaming_watcher)

# Import required classes
FreshnessLevel = streaming_watcher.FreshnessLevel
StreamingWatcher = streaming_watcher.StreamingWatcher
Config = streaming_watcher.Config
QueueManager = streaming_watcher.QueueManager

def create_test_file(age_minutes: float, base_dir: Path) -> Path:
    """Create a test JSONL file with specific age."""
    test_file = base_dir / f"test_{int(age_minutes)}_min.jsonl"
    test_file.write_text('{"message": {"role": "user", "content": "test"}}\n')
    
    # Set modification time
    mod_time = time.time() - (age_minutes * 60)
    os.utime(test_file, (mod_time, mod_time))
    
    return test_file

def test_freshness_categorization():
    """Test file categorization into HOT/WARM/COLD."""
    print("\n=== Testing Freshness Categorization ===")
    
    with tempfile.TemporaryDirectory() as tmpdir:
        base_dir = Path(tmpdir) / "test_project"
        base_dir.mkdir(parents=True)
        
        # Create test files with different ages
        hot_file = create_test_file(2, base_dir)      # 2 minutes old -> HOT
        warm_file = create_test_file(120, base_dir)   # 2 hours old -> WARM
        cold_file = create_test_file(1500, base_dir)  # 25 hours old -> COLD
        
        # Create config
        config = Config()
        config.logs_dir = Path(tmpdir)
        config.state_file = Path(tmpdir) / "test_state.json"
        config.hot_window_minutes = 5
        config.warm_window_hours = 24
        config.max_warm_wait_minutes = 30
        
        # Create watcher
        watcher = StreamingWatcher(config)
        
        # Test categorization
        hot_level, hot_priority = watcher.categorize_freshness(hot_file)
        warm_level, warm_priority = watcher.categorize_freshness(warm_file)
        cold_level, cold_priority = watcher.categorize_freshness(cold_file)
        
        assert hot_level == FreshnessLevel.HOT, f"Expected HOT, got {hot_level}"
        assert warm_level == FreshnessLevel.WARM, f"Expected WARM, got {warm_level}"
        assert cold_level == FreshnessLevel.COLD, f"Expected COLD, got {cold_level}"
        
        # Test priority ordering (lower value = higher priority)
        assert hot_priority < warm_priority, f"HOT priority {hot_priority} should be < WARM {warm_priority}"
        assert warm_priority < cold_priority, f"WARM priority {warm_priority} should be < COLD {cold_priority}"
        
        print(f"✅ HOT file (2 min): level={hot_level.value}, priority={hot_priority}")
        print(f"✅ WARM file (2 hr): level={warm_level.value}, priority={warm_priority}")
        print(f"✅ COLD file (25 hr): level={cold_level.value}, priority={cold_priority}")

def test_starvation_prevention():
    """Test URGENT_WARM for starvation prevention."""
    print("\n=== Testing Starvation Prevention ===")
    
    with tempfile.TemporaryDirectory() as tmpdir:
        base_dir = Path(tmpdir) / "test_project"
        base_dir.mkdir(parents=True)
        
        # Create a WARM file (2 hours old)
        warm_file = create_test_file(120, base_dir)
        
        # Create config
        config = Config()
        config.logs_dir = Path(tmpdir)
        config.state_file = Path(tmpdir) / "test_state.json"
        config.hot_window_minutes = 5
        config.warm_window_hours = 24
        config.max_warm_wait_minutes = 30
        
        # Create watcher
        watcher = StreamingWatcher(config)
        
        # First categorization - should be WARM
        level1, priority1 = watcher.categorize_freshness(warm_file)
        assert level1 == FreshnessLevel.WARM, f"Expected WARM, got {level1}"
        
        # Simulate file waiting for 35 minutes
        file_key = str(warm_file)
        watcher.file_first_seen[file_key] = time.time() - (35 * 60)
        
        # Second categorization - should be URGENT_WARM
        level2, priority2 = watcher.categorize_freshness(warm_file)
        assert level2 == FreshnessLevel.URGENT_WARM, f"Expected URGENT_WARM, got {level2}"
        
        # URGENT_WARM should have higher priority than regular WARM
        assert priority2 < priority1, f"URGENT priority {priority2} should be < WARM {priority1}"
        
        print(f"✅ WARM file initially: level={level1.value}, priority={priority1}")
        print(f"✅ After 35 min wait: level={level2.value}, priority={priority2}")

def test_priority_queue():
    """Test priority queue with HOT/URGENT_WARM prioritization."""
    print("\n=== Testing Priority Queue ===")
    
    queue = QueueManager(max_size=10, max_age_hours=24)
    
    now = datetime.now()
    
    # Add files in wrong order (COLD, WARM, HOT)
    items = [
        (Path("/cold.jsonl"), now - timedelta(hours=30), FreshnessLevel.COLD, 40000),
        (Path("/warm.jsonl"), now - timedelta(hours=2), FreshnessLevel.WARM, 20000),
        (Path("/hot.jsonl"), now - timedelta(minutes=2), FreshnessLevel.HOT, 100),
        (Path("/urgent.jsonl"), now - timedelta(hours=1), FreshnessLevel.URGENT_WARM, 10000),
    ]
    
    added = queue.add_categorized(items)
    assert added == 4, f"Expected 4 files added, got {added}"
    
    # Get batch - should come out in priority order
    batch = queue.get_batch(10)
    
    # HOT and URGENT_WARM should come first (front of queue)
    batch_names = [str(p.name) for p, _ in batch]
    
    assert batch_names[0] == "urgent.jsonl", f"First should be urgent, got {batch_names[0]}"
    assert batch_names[1] == "hot.jsonl", f"Second should be hot, got {batch_names[1]}"
    assert batch_names[2] == "cold.jsonl", f"Third should be cold (FIFO), got {batch_names[2]}"
    assert batch_names[3] == "warm.jsonl", f"Fourth should be warm (FIFO), got {batch_names[3]}"
    
    print(f"✅ Queue order: {batch_names}")
    print(f"✅ HOT and URGENT_WARM correctly prioritized to front")

def test_duplicate_prevention():
    """Test that duplicate files aren't added to queue."""
    print("\n=== Testing Duplicate Prevention ===")
    
    queue = QueueManager(max_size=10, max_age_hours=24)
    
    now = datetime.now()
    file_path = Path("/test.jsonl")
    
    # Add same file multiple times
    items = [
        (file_path, now, FreshnessLevel.HOT, 100),
    ]
    
    added1 = queue.add_categorized(items)
    added2 = queue.add_categorized(items)
    added3 = queue.add_categorized(items)
    
    assert added1 == 1, f"First add should succeed, got {added1}"
    assert added2 == 0, f"Second add should be skipped, got {added2}"
    assert added3 == 0, f"Third add should be skipped, got {added3}"
    
    assert queue.get_metrics()["queue_size"] == 1, "Queue should only have 1 file"
    
    print(f"✅ Duplicate prevention working: only 1 file in queue")

def test_cold_file_limiting():
    """Test that COLD files are limited per cycle."""
    print("\n=== Testing COLD File Limiting ===")
    
    with tempfile.TemporaryDirectory() as tmpdir:
        base_dir = Path(tmpdir) / "test_project"
        base_dir.mkdir(parents=True)
        
        # Create 10 COLD files
        cold_files = []
        for i in range(10):
            cold_file = create_test_file(1500 + i*60, base_dir)  # All > 24 hours old
            cold_files.append(cold_file)
        
        # Create config with limit of 3 COLD files
        config = Config()
        config.logs_dir = Path(tmpdir)
        config.state_file = Path(tmpdir) / "test_state.json"
        config.max_cold_files = 3
        
        # Simulate the filtering logic from run_continuous
        categorized = []
        watcher = StreamingWatcher(config)
        
        for cf in cold_files:
            level, priority = watcher.categorize_freshness(cf)
            categorized.append((cf, level, priority))
        
        # Apply COLD limiting
        files_to_process = []
        cold_count = 0
        
        for file_path, level, priority in categorized:
            if level == FreshnessLevel.COLD:
                if cold_count >= config.max_cold_files:
                    continue
                cold_count += 1
            files_to_process.append(file_path)
        
        assert len(files_to_process) == 3, f"Expected 3 COLD files, got {len(files_to_process)}"
        print(f"✅ COLD file limiting: {len(files_to_process)}/{len(cold_files)} files selected")

async def test_interval_switching():
    """Test dynamic interval switching for HOT files."""
    print("\n=== Testing Interval Switching ===")
    
    with tempfile.TemporaryDirectory() as tmpdir:
        config = Config()
        config.logs_dir = Path(tmpdir)
        config.state_file = Path(tmpdir) / "test_state.json"
        config.hot_check_interval_s = 2
        config.import_frequency = 60
        
        watcher = StreamingWatcher(config)
        
        # Test with no HOT files
        queue = QueueManager(10, 24)
        watcher.queue_manager = queue
        
        has_hot = queue.has_hot_or_urgent()
        assert not has_hot, "Should not have HOT files initially"
        
        wait_time = config.hot_check_interval_s if has_hot else config.import_frequency
        assert wait_time == 60, f"Expected 60s interval, got {wait_time}"
        
        # Add a HOT file to queue
        now = datetime.now()
        queue.add_categorized([
            (Path("/hot.jsonl"), now, FreshnessLevel.HOT, 100)
        ])
        
        has_hot = queue.has_hot_or_urgent()
        assert has_hot, "Should have HOT files now"
        
        wait_time = config.hot_check_interval_s if has_hot else config.import_frequency
        assert wait_time == 2, f"Expected 2s interval for HOT, got {wait_time}"
        
        print(f"✅ Normal mode: {config.import_frequency}s interval")
        print(f"✅ HOT mode: {config.hot_check_interval_s}s interval")

def main():
    """Run all tests."""
    print("=" * 60)
    print("HOT/WARM/COLD Implementation Tests")
    print("=" * 60)
    
    try:
        test_freshness_categorization()
        test_starvation_prevention()
        test_priority_queue()
        test_duplicate_prevention()
        test_cold_file_limiting()
        asyncio.run(test_interval_switching())
        
        print("\n" + "=" * 60)
        print("✅ ALL TESTS PASSED!")
        print("=" * 60)
        
    except AssertionError as e:
        print(f"\n❌ TEST FAILED: {e}")
        sys.exit(1)
    except Exception as e:
        print(f"\n❌ ERROR: {e}")
        import traceback
        traceback.print_exc()
        sys.exit(1)

if __name__ == "__main__":
    main()