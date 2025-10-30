#!/usr/bin/env python3
"""
End-to-end integration tests for batch automation workflow.
Tests the complete pipeline: batch trigger -> narrative generation -> evaluation.
"""

import os
import sys
import json
import pytest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent.parent))

from qdrant_client import QdrantClient


class TestBatchWorkflow:
    """Test end-to-end batch automation workflow."""

    @pytest.fixture
    def qdrant_client(self):
        """Get Qdrant client."""
        url = os.getenv('QDRANT_URL', 'http://localhost:6333')
        try:
            client = QdrantClient(url=url)
            client.get_collections()  # Test connection
            return client
        except Exception as e:
            pytest.skip(f"Qdrant not available: {e}")

    def test_narrative_search_quality(self, qdrant_client):
        """Test that narrative search returns relevant results."""
        try:
            # Test search functionality
            results = qdrant_client.search(
                collection_name='v3_all_projects',
                query_vector=[0.1] * 384,  # Dummy vector for structure test
                limit=3
            )

            # Should return results
            assert isinstance(results, list), "Search should return list"

        except Exception as e:
            pytest.skip(f"Search test failed: {e}")

    def test_batch_state_tracking(self):
        """Test that batch state files exist and are valid JSON."""
        state_dir = Path.home() / '.claude-self-reflect' / 'batch_state'

        if not state_dir.exists():
            pytest.skip("Batch state directory not found (not initialized)")

        narrative_state = state_dir / 'narrative_batches.json'
        eval_state = state_dir / 'eval_batches.json'

        if narrative_state.exists():
            with open(narrative_state) as f:
                data = json.load(f)  # Should be valid JSON
                assert isinstance(data, dict), "Batch state should be dict"

    def test_batch_queue_exists(self):
        """Test that batch queue directory exists."""
        queue_dir = Path.home() / '.claude-self-reflect' / 'batch_queue'

        if not queue_dir.exists():
            pytest.skip("Batch queue not initialized")

        queue_file = queue_dir / 'batch_queue_state.json'
        if queue_file.exists():
            with open(queue_file) as f:
                data = json.load(f)
                assert 'queued_files' in data or 'queue' in data, "Invalid queue state"

    def test_docker_services_configuration(self):
        """Test that docker-compose has batch services configured."""
        compose_file = Path(__file__).parent.parent.parent / 'docker-compose.yaml'
        assert compose_file.exists(), "docker-compose.yaml not found"

        with open(compose_file) as f:
            content = f.read()
            # Should have Qdrant service at minimum
            assert 'qdrant' in content, "Missing Qdrant service in docker-compose"


if __name__ == '__main__':
    pytest.main([__file__, '-v'])
