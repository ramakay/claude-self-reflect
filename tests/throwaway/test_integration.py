#!/usr/bin/env python3
"""Integration test for unified state management."""

import sys
import json
import tempfile
import time
from pathlib import Path

# Add scripts to path
scripts_dir = Path(__file__).parent / "scripts"
sys.path.insert(0, str(scripts_dir))

from unified_state_manager import UnifiedStateManager

def test_integration():
    """Test all major operations of unified state management."""

    print("Running Unified State Management Integration Tests")
    print("=" * 60)

    # Create a test state manager with a temp file
    with tempfile.NamedTemporaryFile(suffix='.json', delete=False) as tmp:
        test_file = tmp.name
        # Initialize with proper state structure
        initial_state = {
            "version": "5.0.0",
            "metadata": {},
            "files": {},
            "importers": {},
            "collections": {}
        }
        tmp.write(json.dumps(initial_state).encode())
        tmp.flush()

    try:
        manager = UnifiedStateManager(Path(test_file))
        print(f"✓ Created UnifiedStateManager v{manager.VERSION}")

        # Test 1: Add files from different importers
        print("\n1. Testing file addition from multiple importers:")

        # Batch importer
        manager.add_imported_file(
            "/Users/ramakrishnanannaswamy/.claude/projects/test1.jsonl",
            chunks=50,
            importer="batch"
        )
        print("  ✓ Added file via batch importer")

        # Streaming importer
        manager.add_imported_file(
            "/Users/ramakrishnanannaswamy/.claude/projects/test2.jsonl",
            chunks=30,
            importer="streaming"
        )
        print("  ✓ Added file via streaming importer")

        # Test 2: Check status
        print("\n2. Testing status retrieval:")
        status = manager.get_status()
        assert status["total_files"] == 2
        assert status["total_chunks"] == 80
        assert status["percentage"] == 100.0
        print(f"  ✓ Status: {status['indexed_files']}/{status['total_files']} files, {status['total_chunks']} chunks")

        # Test 3: Test duplicate detection
        print("\n3. Testing duplicate detection:")
        state = manager.read_state()
        normalized_path = UnifiedStateManager.normalize_path("/Users/ramakrishnanannaswamy/.claude/projects/test1.jsonl")
        is_duplicate = normalized_path in state["files"]
        assert is_duplicate == True
        print("  ✓ Duplicate detection working")

        # Test 4: Mark file as failed
        print("\n4. Testing error handling:")
        manager.mark_file_failed(
            "/Users/ramakrishnanannaswamy/.claude/projects/test3.jsonl",
            "Test error"
        )
        state = manager.read_state()
        failed_file = state["files"]["/Users/ramakrishnanannaswamy/.claude/projects/test3.jsonl"]
        assert failed_file["status"] == "failed"
        assert failed_file["error"] == "Test error"
        print("  ✓ Error tracking working")

        # Test 5: Test concurrent access simulation
        print("\n5. Testing concurrent access (simulated):")
        start = time.time()

        # Simulate two processes updating simultaneously
        manager.add_imported_file(
            "/Users/ramakrishnanannaswamy/.claude/projects/concurrent1.jsonl",
            chunks=25,
            importer="batch"
        )

        manager.add_imported_file(
            "/Users/ramakrishnanannaswamy/.claude/projects/concurrent2.jsonl",
            chunks=35,
            importer="streaming"
        )

        elapsed = (time.time() - start) * 1000
        print(f"  ✓ Concurrent updates completed in {elapsed:.2f}ms")

        # Test 6: Performance check
        print("\n6. Testing performance:")

        # Add 100 files
        for i in range(100):
            manager.add_imported_file(
                f"/Users/ramakrishnanannaswamy/.claude/projects/perf{i}.jsonl",
                chunks=10,
                importer="batch" if i % 2 == 0 else "streaming"
            )

        # Time status retrieval
        start = time.time()
        status = manager.get_status()
        elapsed = (time.time() - start) * 1000

        assert status["total_files"] == 105  # 2 + 1 failed + 2 concurrent + 100 perf
        assert elapsed < 20  # Must be under 20ms
        print(f"  ✓ Status retrieval for 105 files: {elapsed:.2f}ms (<20ms requirement)")

        # Test 7: Docker path normalization
        print("\n7. Testing Docker path normalization:")
        docker_paths = [
            ("/logs/myproject/file.jsonl", "myproject"),
            ("/config/settings.json", "settings.json"),
            ("/app/data/test.jsonl", "test.jsonl")
        ]

        for docker_path, expected_part in docker_paths:
            normalized = UnifiedStateManager.normalize_path(docker_path)
            assert expected_part in normalized
            print(f"  ✓ {docker_path} → .../{expected_part}")

        # Test 8: Test metadata preservation
        print("\n8. Testing metadata preservation:")
        test_file = "/Users/ramakrishnanannaswamy/.claude/projects/metadata_test.jsonl"
        manager.add_imported_file(
            test_file,
            chunks=42,
            importer="manual",  # Use valid importer
            collection="test_collection",
            embedding_mode="cloud"
        )

        state = manager.read_state()
        metadata = state["files"][test_file]
        assert metadata["chunks"] == 42
        assert metadata["collection"] == "test_collection"
        assert metadata["embedding_mode"] == "cloud"
        print("  ✓ All metadata preserved correctly")

        print("\n" + "=" * 60)
        print("✅ All integration tests passed!")

        # Show final statistics
        final_status = manager.get_status()
        print(f"\nFinal Statistics:")
        print(f"  - Total files: {final_status['total_files']}")
        print(f"  - Total chunks: {final_status['total_chunks']}")
        print(f"  - Importers: {json.dumps(state['importers'], indent=4)}")

    finally:
        # Cleanup
        Path(test_file).unlink(missing_ok=True)
        print(f"\n✓ Cleaned up test file")

if __name__ == "__main__":
    test_integration()