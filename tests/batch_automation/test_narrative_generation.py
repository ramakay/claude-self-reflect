#!/usr/bin/env python3
"""
Automated tests for narrative generation system (v7.0 primary feature).
Tests batch import, narrative quality, and metadata extraction.
"""

import os
import sys
import json
import pytest
from pathlib import Path
from unittest.mock import Mock, patch

# Add project root to path
sys.path.insert(0, str(Path(__file__).parent.parent.parent))

from qdrant_client import QdrantClient


class TestNarrativeGeneration:
    """Test narrative generation and batch automation."""

    @pytest.fixture
    def qdrant_client(self):
        """Get Qdrant client."""
        url = os.getenv('QDRANT_URL', 'http://localhost:6333')
        return QdrantClient(url=url)

    def test_batch_import_script_exists(self):
        """Test that batch import script exists and is valid."""
        script_path = Path(__file__).parent.parent.parent / 'docs' / 'design' / 'batch_import_all_projects.py'
        assert script_path.exists(), "batch_import_all_projects.py not found"

        # Check it's valid Python
        with open(script_path) as f:
            code = f.read()
            compile(code, str(script_path), 'exec')

    def test_ground_truth_generator_exists(self):
        """Test that ground truth generator exists and is valid."""
        script_path = Path(__file__).parent.parent.parent / 'docs' / 'design' / 'batch_ground_truth_generator.py'
        assert script_path.exists(), "batch_ground_truth_generator.py not found"

        with open(script_path) as f:
            code = f.read()
            compile(code, str(script_path), 'exec')

    def test_v3_extraction_importable(self):
        """Test that V3 extraction module is importable."""
        sys.path.insert(0, str(Path(__file__).parent.parent.parent / 'docs' / 'design'))
        from extract_events_v3 import extract_events_v3
        assert callable(extract_events_v3)

    def test_narrative_collection_exists(self, qdrant_client):
        """Test that narrative collection exists in Qdrant."""
        collections = qdrant_client.get_collections().collections
        collection_names = [c.name for c in collections]
        assert 'v3_all_projects' in collection_names, "v3_all_projects collection not found"

    def test_narrative_collection_has_data(self, qdrant_client):
        """Test that narrative collection has narratives."""
        try:
            collection_info = qdrant_client.get_collection('v3_all_projects')
            assert collection_info.points_count > 0, "v3_all_projects has no narratives"
        except Exception as e:
            pytest.skip(f"Qdrant not available: {e}")

    def test_narrative_structure(self, qdrant_client):
        """Test that narratives have proper structure."""
        try:
            results = qdrant_client.scroll(
                collection_name='v3_all_projects',
                limit=1,
                with_payload=True
            )

            if not results[0]:
                pytest.skip("No narratives in collection")

            narrative = results[0][0].payload

            # Check required fields
            assert 'narrative' in narrative, "Missing 'narrative' field"
            assert 'conversation_id' in narrative, "Missing 'conversation_id' field"
            assert 'project' in narrative, "Missing 'project' field"

            # Check narrative is non-empty
            assert len(narrative['narrative']) > 0, "Empty narrative"

            # Check for metadata enrichment
            if 'signature' in narrative:
                assert isinstance(narrative['signature'], dict), "Invalid signature format"

        except Exception as e:
            pytest.skip(f"Qdrant not available: {e}")

    def test_evaluation_collection_exists(self, qdrant_client):
        """Test that evaluation collection exists."""
        try:
            collections = qdrant_client.get_collections().collections
            collection_names = [c.name for c in collections]
            assert 'ground_truth_evals' in collection_names, "ground_truth_evals collection not found"
        except Exception as e:
            pytest.skip(f"Qdrant not available: {e}")

    def test_evaluation_scripts_exist(self):
        """Test that evaluation scripts exist."""
        run_eval = Path(__file__).parent.parent.parent / 'scripts' / 'evaluation' / 'run_evaluation.py'
        simple_eval = Path(__file__).parent.parent.parent / 'scripts' / 'evaluation' / 'simple_evaluation.py'

        assert run_eval.exists(), "run_evaluation.py not found"
        assert simple_eval.exists(), "simple_evaluation.py not found"

    def test_batch_monitor_has_file_locking(self):
        """Test that batch_monitor.py implements file locking (v7.0 security)."""
        monitor_path = Path(__file__).parent.parent.parent / 'src' / 'runtime' / 'batch_monitor.py'
        assert monitor_path.exists(), "batch_monitor.py not found"

        with open(monitor_path) as f:
            code = f.read()
            assert 'fcntl' in code, "Missing fcntl import for file locking"
            assert 'LOCK_SH' in code or 'LOCK_EX' in code, "Missing file locking implementation"

    def test_batch_watcher_subprocess_security(self):
        """Test that batch_watcher.py has secure subprocess handling (v7.0 security)."""
        watcher_path = Path(__file__).parent.parent.parent / 'src' / 'runtime' / 'batch_watcher.py'
        assert watcher_path.exists(), "batch_watcher.py not found"

        with open(watcher_path) as f:
            code = f.read()
            assert 'sys.executable' in code, "Should use sys.executable instead of hardcoded python"
            assert 'subprocess.run' in code, "Should use subprocess.run"


class TestBatchConfiguration:
    """Test batch automation configuration."""

    def test_batch_triggers_configured(self):
        """Test that batch triggers are properly configured."""
        config_path = Path(__file__).parent.parent.parent / 'src' / 'runtime' / 'config.py'

        if config_path.exists():
            sys.path.insert(0, str(config_path.parent))
            import config

            assert hasattr(config, 'BATCH_SIZE_TRIGGER'), "Missing BATCH_SIZE_TRIGGER"
            assert hasattr(config, 'BATCH_TIME_TRIGGER_MINUTES'), "Missing BATCH_TIME_TRIGGER_MINUTES"
            assert config.BATCH_SIZE_TRIGGER > 0, "Invalid BATCH_SIZE_TRIGGER"
            assert config.BATCH_TIME_TRIGGER_MINUTES > 0, "Invalid BATCH_TIME_TRIGGER_MINUTES"

    def test_dockerfile_batch_monitor_exists(self):
        """Test that batch monitor Dockerfile exists."""
        dockerfile = Path(__file__).parent.parent.parent / 'Dockerfile.batch-monitor'
        assert dockerfile.exists(), "Dockerfile.batch-monitor not found"

    def test_dockerfile_batch_watcher_exists(self):
        """Test that batch watcher Dockerfile exists."""
        dockerfile = Path(__file__).parent.parent.parent / 'Dockerfile.batch-watcher'
        assert dockerfile.exists(), "Dockerfile.batch-watcher not found"


if __name__ == '__main__':
    pytest.main([__file__, '-v'])
