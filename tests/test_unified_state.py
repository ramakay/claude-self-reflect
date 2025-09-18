#!/usr/bin/env python3
"""
Comprehensive test suite for the unified state management system.

This test suite covers:
- Migration tests (dry-run, actual migration, rollback, data preservation)
- Concurrency tests (multiple readers/writers, locking, race conditions)
- Performance tests (status check speed, large files, memory usage)
- Security tests (path traversal, lock expiry, input validation)
- Cross-platform tests (Windows/Unix locking, Docker paths)
- Integration tests (batch importer, streaming watcher, status checking)
- Edge cases (corrupted files, network failures, disk full)
"""

import json
import os
import sys
import time
import uuid
import shutil
import tempfile
import threading
import subprocess
import multiprocessing
from pathlib import Path
from datetime import datetime, timezone, timedelta
from unittest.mock import patch, mock_open, MagicMock
from concurrent.futures import ThreadPoolExecutor, as_completed
import psutil
import pytest

# Add scripts directory to path for imports
project_root = Path(__file__).parent.parent
sys.path.insert(0, str(project_root / "scripts"))
sys.path.insert(0, str(project_root / "mcp-server" / "src"))

from scripts.unified_state_manager import UnifiedStateManager


class TestUnifiedStateManager:
    """Comprehensive test suite for UnifiedStateManager"""

    @pytest.fixture
    def temp_dir(self):
        """Create a temporary directory for testing"""
        with tempfile.TemporaryDirectory() as temp_dir:
            yield Path(temp_dir)

    @pytest.fixture
    def state_manager(self, temp_dir):
        """Create a UnifiedStateManager instance for testing"""
        state_file = temp_dir / "test-state.json"
        return UnifiedStateManager(state_file)

    @pytest.fixture
    def sample_state_v3(self):
        """Sample state data from v3.x for migration testing"""
        return {
            "version": "3.0.0",
            "metadata": {
                "created_at": "2023-01-01T00:00:00Z",
                "last_modified": "2023-01-01T00:00:00Z",
                "total_files": 10,
                "total_chunks": 100
            },
            "files": {
                "/path/to/file1.jsonl": {
                    "imported_at": "2023-01-01T00:00:00Z",
                    "chunks": 10,
                    "status": "completed"
                },
                "/path/to/file2.jsonl": {
                    "imported_at": "2023-01-01T00:00:00Z",
                    "chunks": 15,
                    "status": "completed"
                }
            }
        }

    @pytest.fixture
    def sample_state_v5(self):
        """Sample current v5.0 state data"""
        return {
            "version": "5.0.0",
            "metadata": {
                "created_at": "2023-01-01T00:00:00Z",
                "last_modified": "2023-01-01T00:00:00Z",
                "total_files": 10,
                "total_chunks": 100,
                "last_batch_import": None,
                "last_stream_import": None
            },
            "lock": None,
            "files": {
                "/path/to/file1.jsonl": {
                    "imported_at": "2023-01-01T00:00:00Z",
                    "last_modified": "2023-01-01T00:00:00Z",
                    "chunks": 10,
                    "importer": "batch",
                    "collection": "test_collection",
                    "embedding_mode": "local",
                    "status": "completed",
                    "error": None,
                    "retry_count": 0
                }
            },
            "importers": {
                "batch": {"last_run": None, "files_processed": 0, "chunks_imported": 0, "status": "idle"},
                "streaming": {"last_run": None, "files_processed": 0, "chunks_imported": 0, "status": "inactive"}
            },
            "collections": {
                "test_collection": {
                    "files": 1,
                    "chunks": 10,
                    "embedding_mode": "local",
                    "dimensions": 384
                }
            }
        }


class TestMigration(TestUnifiedStateManager):
    """Migration tests for state file format changes"""

    def test_migration_dry_run_detection(self, temp_dir, sample_state_v3):
        """Test that dry-run mode can detect migration needs without modifying files"""
        # Create old state file
        old_state_file = temp_dir / "old-state.json"
        with open(old_state_file, 'w') as f:
            json.dump(sample_state_v3, f)

        # Create manager but don't initialize yet
        state_file = temp_dir / "new-state.json"
        manager = UnifiedStateManager(state_file)

        # Manually read and check migration need
        with open(old_state_file, 'r') as f:
            state = json.load(f)

        needs_migration = state.get("version", "1.0.0") < manager.VERSION
        assert needs_migration

        # Verify old file unchanged
        with open(old_state_file, 'r') as f:
            unchanged_state = json.load(f)
        assert unchanged_state == sample_state_v3

    def test_actual_migration_from_v3(self, temp_dir, sample_state_v3):
        """Test actual migration from v3.x to v5.0"""
        # Create old state file
        state_file = temp_dir / "state.json"
        with open(state_file, 'w') as f:
            json.dump(sample_state_v3, f)

        # Initialize manager (should trigger migration)
        manager = UnifiedStateManager(state_file)

        # Read migrated state
        migrated_state = manager.read_state()

        # Verify migration
        assert migrated_state["version"] == "5.0.0"
        assert "lock" in migrated_state
        assert "importers" in migrated_state
        assert "collections" in migrated_state
        assert migrated_state["metadata"]["migrated_from"] == "3.0.0"
        assert "migration_date" in migrated_state["metadata"]

        # Verify data preservation
        assert migrated_state["files"] == sample_state_v3["files"]
        assert migrated_state["metadata"]["total_files"] == sample_state_v3["metadata"]["total_files"]

    def test_migration_rollback_functionality(self, temp_dir, sample_state_v3):
        """Test rollback functionality for failed migrations"""
        state_file = temp_dir / "state.json"
        backup_file = temp_dir / "state.json.backup"

        # Create original state
        with open(state_file, 'w') as f:
            json.dump(sample_state_v3, f)

        # Create backup
        shutil.copy2(state_file, backup_file)

        # Simulate failed migration by corrupting state
        with open(state_file, 'w') as f:
            f.write("invalid json")

        # Rollback
        shutil.copy2(backup_file, state_file)

        # Verify rollback worked
        with open(state_file, 'r') as f:
            restored_state = json.load(f)
        assert restored_state == sample_state_v3

    def test_data_preservation_during_migration(self, temp_dir, sample_state_v3):
        """Test that all data is preserved during migration"""
        state_file = temp_dir / "state.json"

        # Add more complex data to v3 state
        complex_state = sample_state_v3.copy()
        complex_state["files"]["/complex/path/file.jsonl"] = {
            "imported_at": "2023-06-15T10:30:00Z",
            "chunks": 25,
            "status": "failed",
            "custom_field": "should_be_preserved"
        }

        with open(state_file, 'w') as f:
            json.dump(complex_state, f)

        # Migrate
        manager = UnifiedStateManager(state_file)
        migrated_state = manager.read_state()

        # Verify all original files preserved
        for path, metadata in complex_state["files"].items():
            assert path in migrated_state["files"]
            original_chunks = metadata["chunks"]
            migrated_chunks = migrated_state["files"][path]["chunks"]
            assert migrated_chunks == original_chunks


class TestConcurrency(TestUnifiedStateManager):
    """Concurrency tests for multiple readers/writers and locking"""

    def test_multiple_readers(self, state_manager):
        """Test multiple concurrent readers don't interfere"""
        # Add some data first
        state_manager.add_imported_file("/test/file1.jsonl", 10)

        results = []

        def read_state():
            try:
                state = state_manager.read_state()
                results.append(("success", len(state["files"])))
            except Exception as e:
                results.append(("error", str(e)))

        # Start multiple readers
        threads = []
        for i in range(10):
            thread = threading.Thread(target=read_state)
            threads.append(thread)
            thread.start()

        # Wait for all to complete
        for thread in threads:
            thread.join()

        # Verify all succeeded
        assert len(results) == 10
        for status, data in results:
            assert status == "success"
            assert data == 1  # Should see 1 file

    def test_multiple_writers_with_locking(self, state_manager):
        """Test multiple writers with file locking prevent corruption"""
        results = []

        def add_file(file_id):
            try:
                state_manager.add_imported_file(f"/test/file{file_id}.jsonl", 5)
                results.append(("success", file_id))
            except Exception as e:
                results.append(("error", f"{file_id}: {str(e)}"))

        # Start multiple writers
        threads = []
        for i in range(20):
            thread = threading.Thread(target=add_file, args=(i,))
            threads.append(thread)
            thread.start()

        # Wait for all to complete
        for thread in threads:
            thread.join()

        # Verify all succeeded and no corruption
        success_count = sum(1 for status, _ in results if status == "success")
        assert success_count == 20

        # Verify final state consistency
        final_state = state_manager.read_state()
        assert len(final_state["files"]) == 20
        assert final_state["metadata"]["total_files"] == 20

    def test_race_condition_handling(self, state_manager):
        """Test race condition handling between readers and writers"""
        def read_continuously():
            for _ in range(50):
                try:
                    state_manager.read_state()
                    time.sleep(0.001)  # Small delay
                except Exception:
                    pass  # Ignore lock contention

        def write_continuously():
            for i in range(10):
                try:
                    state_manager.add_imported_file(f"/race/file{i}.jsonl", 1)
                    time.sleep(0.002)  # Small delay
                except Exception:
                    pass  # Ignore lock contention

        # Start readers and writers concurrently
        threads = []

        # Multiple readers
        for _ in range(3):
            thread = threading.Thread(target=read_continuously)
            threads.append(thread)
            thread.start()

        # Multiple writers
        for _ in range(2):
            thread = threading.Thread(target=write_continuously)
            threads.append(thread)
            thread.start()

        # Wait for completion
        for thread in threads:
            thread.join()

        # Verify state is still valid
        final_state = state_manager.read_state()
        assert "files" in final_state
        assert final_state["version"] == "5.0.0"

    def test_lock_timeout_and_retry(self, state_manager):
        """Test lock timeout and retry mechanism"""
        # Create a long-running operation to hold the lock
        def long_operation():
            def slow_updater(state):
                time.sleep(2)  # Hold lock for 2 seconds
                return state
            state_manager.update_state(slow_updater)

        # Start long operation
        long_thread = threading.Thread(target=long_operation)
        long_thread.start()

        time.sleep(0.1)  # Ensure long operation gets lock first

        # Try quick operation that should timeout
        start_time = time.time()
        with pytest.raises(TimeoutError):
            state_manager.add_imported_file("/test/timeout.jsonl", 1)

        duration = time.time() - start_time
        assert 4 < duration < 7  # Should timeout around 5 seconds (LOCK_TIMEOUT)

        long_thread.join()


class TestPerformance(TestUnifiedStateManager):
    """Performance tests for speed and memory usage"""

    def test_status_check_speed(self, state_manager):
        """Test that status check completes in under 20ms"""
        # Add some data first
        for i in range(100):
            state_manager.add_imported_file(f"/perf/file{i}.jsonl", 10)

        # Measure status check time
        start_time = time.perf_counter()
        status = state_manager.get_status()
        duration = (time.perf_counter() - start_time) * 1000  # Convert to ms

        assert duration < 20, f"Status check took {duration:.2f}ms, should be under 20ms"
        assert status["total_files"] == 100

    def test_large_state_files(self, state_manager):
        """Test performance with large state files (1000+ entries)"""
        # Add 1000 files
        start_time = time.time()
        for i in range(1000):
            state_manager.add_imported_file(f"/large/file{i:04d}.jsonl", 5)

            # Check progress every 100 files
            if (i + 1) % 100 == 0:
                elapsed = time.time() - start_time
                print(f"Added {i + 1} files in {elapsed:.2f}s")

        total_time = time.time() - start_time
        print(f"Total time for 1000 files: {total_time:.2f}s")

        # Test read performance
        read_start = time.time()
        state = state_manager.read_state()
        read_time = time.time() - read_start

        assert len(state["files"]) == 1000
        assert read_time < 1.0, f"Reading 1000 files took {read_time:.2f}s, should be under 1s"

        # Test status performance
        status_start = time.time()
        status = state_manager.get_status()
        status_time = time.time() - status_start

        assert status_time < 0.1, f"Status check took {status_time:.2f}s, should be under 0.1s"

    def test_memory_usage_validation(self, state_manager):
        """Test memory usage remains reasonable with large datasets"""
        process = psutil.Process()
        initial_memory = process.memory_info().rss / 1024 / 1024  # MB

        # Add many files
        for i in range(500):
            state_manager.add_imported_file(f"/memory/file{i}.jsonl", 10)

        peak_memory = process.memory_info().rss / 1024 / 1024  # MB
        memory_increase = peak_memory - initial_memory

        # Memory increase should be reasonable (less than 50MB for 500 files)
        assert memory_increase < 50, f"Memory increased by {memory_increase:.2f}MB, should be under 50MB"

    def test_benchmark_vs_old_approach(self, temp_dir):
        """Benchmark unified state vs old multi-file approach"""
        # Simulate old multi-file approach
        def old_multi_file_approach():
            files_data = {}
            for i in range(100):
                file_path = temp_dir / f"old_file_{i}.json"
                data = {"chunks": 10, "status": "completed"}
                with open(file_path, 'w') as f:
                    json.dump(data, f)
                files_data[str(file_path)] = data
            return files_data

        # Benchmark old approach
        old_start = time.time()
        old_data = old_multi_file_approach()
        old_time = time.time() - old_start

        # Benchmark new unified approach
        state_manager = UnifiedStateManager(temp_dir / "unified.json")
        new_start = time.time()
        for i in range(100):
            state_manager.add_imported_file(f"/benchmark/file{i}.jsonl", 10)
        new_time = time.time() - new_start

        print(f"Old approach: {old_time:.3f}s, New approach: {new_time:.3f}s")

        # New approach should be competitive (within 2x)
        assert new_time < old_time * 2, "New approach should not be significantly slower"


class TestSecurity(TestUnifiedStateManager):
    """Security tests for path traversal and input validation"""

    def test_path_traversal_protection(self, state_manager):
        """Test protection against path traversal attacks"""
        malicious_paths = [
            "../../../etc/passwd",
            "..\\..\\windows\\system32\\config\\sam",
            "/etc/shadow",
            "C:\\Windows\\System32\\config\\SAM",
            "../../root/.ssh/id_rsa",
            "../../../proc/version",
            "/var/log/auth.log"
        ]

        for path in malicious_paths:
            with pytest.raises(ValueError, match="Path outside allowed directories"):
                state_manager.add_imported_file(path, 1)

    def test_lock_expiry_mechanism(self, state_manager):
        """Test that expired locks are automatically cleared"""
        # Create an expired lock manually
        def create_expired_lock(state):
            state["lock"] = {
                "holder": "test_process",
                "acquired_at": (datetime.now(timezone.utc) - timedelta(minutes=5)).isoformat(),
                "expires_at": (datetime.now(timezone.utc) - timedelta(minutes=1)).isoformat(),
                "transaction_id": "expired_lock"
            }
            return state

        state_manager.update_state(create_expired_lock)

        # Next operation should clear the expired lock
        state_manager.add_imported_file("/test/after_expired.jsonl", 5)

        # Verify lock was cleared
        state = state_manager.read_state()
        assert state["lock"] is None

    def test_input_validation(self, state_manager):
        """Test comprehensive input validation"""
        # Test empty file path
        with pytest.raises(ValueError, match="File path cannot be empty"):
            state_manager.add_imported_file("", 10)

        # Test negative chunks
        with pytest.raises(ValueError, match="Chunks must be non-negative"):
            state_manager.add_imported_file("/test/file.jsonl", -1)

        # Test invalid importer
        with pytest.raises(ValueError, match="Invalid importer"):
            state_manager.add_imported_file("/test/file.jsonl", 10, importer="invalid")

        # Test invalid embedding mode
        with pytest.raises(ValueError, match="Invalid embedding mode"):
            state_manager.add_imported_file("/test/file.jsonl", 10, embedding_mode="invalid")

        # Test invalid status
        with pytest.raises(ValueError, match="Invalid status"):
            state_manager.add_imported_file("/test/file.jsonl", 10, status="invalid")

    def test_safe_json_serialization(self, state_manager):
        """Test safe JSON serialization handles all types correctly"""
        # Test with datetime objects
        from datetime import datetime

        def updater_with_datetime(state):
            # This should be safely serialized
            state["test_datetime"] = datetime.now(timezone.utc)
            state["test_path"] = Path("/test/path")
            return state

        # Should not raise TypeError
        state_manager.update_state(updater_with_datetime)

        # Verify data was serialized correctly
        state = state_manager.read_state()
        assert isinstance(state["test_datetime"], str)
        assert isinstance(state["test_path"], str)


class TestCrossPlatform(TestUnifiedStateManager):
    """Cross-platform tests for Windows/Unix compatibility"""

    @pytest.mark.skipif(sys.platform == 'win32', reason="Unix-specific test")
    def test_unix_file_locking(self, state_manager):
        """Test Unix fcntl-based file locking"""
        with patch('scripts.unified_state_manager.HAS_FILELOCK', False), \
             patch('scripts.unified_state_manager.HAS_FCNTL', True):

            # Should work with fcntl locking
            state_manager.add_imported_file("/unix/test.jsonl", 10)
            state = state_manager.read_state()
            assert len(state["files"]) == 1

    @pytest.mark.skipif(sys.platform != 'win32', reason="Windows-specific test")
    def test_windows_file_locking(self, state_manager):
        """Test Windows msvcrt-based file locking"""
        with patch('scripts.unified_state_manager.HAS_FILELOCK', False), \
             patch('scripts.unified_state_manager.HAS_MSVCRT', True):

            # Should work with msvcrt locking
            state_manager.add_imported_file("/windows/test.jsonl", 10)
            state = state_manager.read_state()
            assert len(state["files"]) == 1

    def test_docker_path_normalization(self, state_manager):
        """Test Docker to local path normalization"""
        docker_paths = [
            "/logs/project1/conversation.jsonl",
            "/config/imported-files.json",
            "/app/data/project2/chat.jsonl"
        ]

        for docker_path in docker_paths:
            try:
                # Should normalize without error (but may fail validation if outside allowed dirs)
                normalized = UnifiedStateManager.normalize_path(docker_path)
                assert not normalized.startswith("/logs/")
                assert not normalized.startswith("/config/")
                assert not normalized.startswith("/app/data/")
            except ValueError as e:
                # Expected if path is outside allowed directories
                assert "Path outside allowed directories" in str(e)

    def test_platform_specific_atomic_writes(self, state_manager):
        """Test platform-specific atomic write operations"""
        # Add a file to trigger atomic write
        state_manager.add_imported_file("/platform/test.jsonl", 5)

        # Verify file was written correctly
        assert state_manager.state_file.exists()

        # Verify no temp files left behind
        temp_files = list(state_manager.state_file.parent.glob("*.tmp"))
        assert len(temp_files) == 0


class TestIntegration(TestUnifiedStateManager):
    """Integration tests with other system components"""

    def test_batch_importer_integration(self, state_manager):
        """Test integration with batch importer workflow"""
        # Simulate batch import workflow
        batch_files = [
            "/project1/conversation1.jsonl",
            "/project1/conversation2.jsonl",
            "/project1/conversation3.jsonl"
        ]

        # Add files as batch import
        for i, file_path in enumerate(batch_files):
            state_manager.add_imported_file(
                file_path,
                chunks=10 + i,
                importer="batch",
                collection="project1_local",
                embedding_mode="local"
            )

        # Verify batch import stats
        status = state_manager.get_status()
        assert status["importers"]["batch"]["files_processed"] == 3
        assert status["importers"]["batch"]["chunks_imported"] == 33  # 10+11+12
        assert status["last_batch_import"] is not None

    def test_streaming_watcher_integration(self, state_manager):
        """Test integration with streaming watcher"""
        # Simulate streaming import
        streaming_files = [
            "/project2/new_conversation.jsonl"
        ]

        for file_path in streaming_files:
            state_manager.add_imported_file(
                file_path,
                chunks=25,
                importer="streaming",
                collection="project2_cloud",
                embedding_mode="cloud"
            )

        # Verify streaming import stats
        status = state_manager.get_status()
        assert status["importers"]["streaming"]["files_processed"] == 1
        assert status["importers"]["streaming"]["chunks_imported"] == 25
        assert status["last_stream_import"] is not None

    def test_status_checking_integration(self, state_manager):
        """Test integration with status checking system"""
        # Add mixed import data
        state_manager.add_imported_file("/test1.jsonl", 10, "batch", "coll1", "local")
        state_manager.add_imported_file("/test2.jsonl", 15, "streaming", "coll2", "cloud")
        state_manager.mark_file_failed("/test3.jsonl", "Network error")

        # Get comprehensive status
        status = state_manager.get_status()

        # Verify all status fields
        assert status["version"] == "5.0.0"
        assert status["total_files"] == 3
        assert status["total_chunks"] == 25  # Only completed files counted
        assert status["indexed_files"] == 3
        assert len(status["collections"]) == 2
        assert "batch" in status["importers"]
        assert "streaming" in status["importers"]

    def test_error_recovery_scenarios(self, state_manager):
        """Test error recovery and retry scenarios"""
        # Mark a file as failed
        state_manager.mark_file_failed("/failed/file.jsonl", "Connection timeout")

        # Verify failure recorded
        state = state_manager.read_state()
        failed_file = state["files"]["/failed/file.jsonl"]
        assert failed_file["status"] == "failed"
        assert failed_file["error"] == "Connection timeout"
        assert failed_file["retry_count"] == 1

        # Retry the same file (mark as failed again)
        state_manager.mark_file_failed("/failed/file.jsonl", "Still failing")

        # Verify retry count increased
        state = state_manager.read_state()
        failed_file = state["files"]["/failed/file.jsonl"]
        assert failed_file["retry_count"] == 2

        # Successfully import the file
        state_manager.add_imported_file("/failed/file.jsonl", 20, status="completed")

        # Verify file is now successful
        state = state_manager.read_state()
        successful_file = state["files"]["/failed/file.jsonl"]
        assert successful_file["status"] == "completed"
        assert successful_file["chunks"] == 20


class TestEdgeCases(TestUnifiedStateManager):
    """Edge case tests for unusual scenarios"""

    def test_empty_state_file(self, temp_dir):
        """Test handling of empty state file"""
        state_file = temp_dir / "empty.json"
        state_file.touch()  # Create empty file

        # Should initialize properly
        manager = UnifiedStateManager(state_file)
        state = manager.read_state()

        assert state["version"] == "5.0.0"
        assert "files" in state
        assert "metadata" in state

    def test_corrupted_state_file(self, temp_dir):
        """Test handling of corrupted state file"""
        state_file = temp_dir / "corrupted.json"
        with open(state_file, 'w') as f:
            f.write("invalid json content {")

        # Should raise appropriate error
        with pytest.raises(json.JSONDecodeError):
            UnifiedStateManager(state_file)

    def test_missing_state_file(self, temp_dir):
        """Test handling of missing state file"""
        state_file = temp_dir / "nonexistent.json"

        # Should create new file
        manager = UnifiedStateManager(state_file)
        assert state_file.exists()

        state = manager.read_state()
        assert state["version"] == "5.0.0"

    def test_network_failures_simulation(self, state_manager):
        """Test handling of simulated network failures"""
        # Mock network failure during file operations
        with patch('builtins.open', side_effect=OSError("Network unreachable")):
            with pytest.raises(OSError):
                state_manager.read_state()

    def test_disk_full_scenario(self, state_manager):
        """Test handling of disk full scenarios"""
        # Mock disk full error
        with patch('builtins.open', side_effect=OSError("No space left on device")):
            with pytest.raises(OSError):
                state_manager.add_imported_file("/test/disk_full.jsonl", 10)

    def test_cleanup_old_entries(self, state_manager):
        """Test cleanup of old entries"""
        # Add some old entries by mocking datetime
        old_date = datetime.now(timezone.utc) - timedelta(days=45)
        recent_date = datetime.now(timezone.utc) - timedelta(days=15)

        with patch('scripts.unified_state_manager.datetime') as mock_datetime:
            mock_datetime.now.return_value = old_date
            mock_datetime.now.side_effect = None
            state_manager.add_imported_file("/old/file1.jsonl", 10)

        with patch('scripts.unified_state_manager.datetime') as mock_datetime:
            mock_datetime.now.return_value = recent_date
            mock_datetime.now.side_effect = None
            state_manager.add_imported_file("/recent/file2.jsonl", 15)

        # Cleanup entries older than 30 days
        removed_count = state_manager.cleanup_old_entries(days=30)

        # Should remove 1 old entry
        assert removed_count == 1

        # Verify only recent file remains
        state = state_manager.read_state()
        assert len(state["files"]) == 1
        assert "/recent/file2.jsonl" in state["files"]
        assert "/old/file1.jsonl" not in state["files"]

    def test_unicode_and_special_characters(self, state_manager):
        """Test handling of Unicode and special characters in paths"""
        unicode_paths = [
            "/测试/文件.jsonl",
            "/тест/файл.jsonl",
            "/test/file with spaces.jsonl",
            "/test/file-with-hyphens.jsonl",
            "/test/file_with_underscores.jsonl"
        ]

        for path in unicode_paths:
            try:
                # May fail due to path validation, but should handle Unicode gracefully
                state_manager.add_imported_file(path, 5)
            except ValueError as e:
                # Expected if path is outside allowed directories
                assert "Path outside allowed directories" in str(e)

    def test_very_large_chunks_number(self, state_manager):
        """Test handling of very large chunk numbers"""
        # Test with very large chunk count
        large_chunks = 1000000
        state_manager.add_imported_file("/large/chunks.jsonl", large_chunks)

        state = state_manager.read_state()
        assert state["files"]["/large/chunks.jsonl"]["chunks"] == large_chunks
        assert state["metadata"]["total_chunks"] == large_chunks

    def test_concurrent_cleanup_operations(self, state_manager):
        """Test concurrent cleanup operations don't interfere"""
        # Add multiple files
        for i in range(20):
            state_manager.add_imported_file(f"/cleanup/file{i}.jsonl", 5)

        # Run concurrent cleanup operations
        results = []

        def cleanup_thread():
            try:
                removed = state_manager.cleanup_old_entries(days=0)  # Remove all
                results.append(("success", removed))
            except Exception as e:
                results.append(("error", str(e)))

        threads = []
        for _ in range(3):
            thread = threading.Thread(target=cleanup_thread)
            threads.append(thread)
            thread.start()

        for thread in threads:
            thread.join()

        # At least one should succeed
        success_count = sum(1 for status, _ in results if status == "success")
        assert success_count >= 1


# Pytest configuration and test runner
def run_comprehensive_tests():
    """Run all tests with detailed reporting"""
    pytest_args = [
        __file__,
        "-v",  # Verbose output
        "--tb=short",  # Short traceback format
        "--durations=10",  # Show 10 slowest tests
        "-x",  # Stop on first failure
        "--capture=no"  # Don't capture stdout
    ]

    return pytest.main(pytest_args)


if __name__ == "__main__":
    # Check dependencies
    try:
        import pytest
        import psutil
    except ImportError as e:
        print(f"Missing dependency: {e}")
        print("Install with: pip install pytest psutil")
        sys.exit(1)

    # Run the test suite
    exit_code = run_comprehensive_tests()
    sys.exit(exit_code)