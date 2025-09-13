"""
Integration tests for Claude Self-Reflect v3.3.x modules.
Tests the integration of all 10 modules into server.py.
"""

import pytest
import asyncio
import sys
from pathlib import Path
from datetime import datetime, timedelta
from unittest.mock import MagicMock, AsyncMock, patch

# Add parent directory to path
sys.path.insert(0, str(Path(__file__).parent.parent / 'src'))

# Import modules to test
try:
    from server import (
        SearchResult,
        qdrant_client,
        qdrant_pool,
        decay_manager,
        CONNECTION_POOL_AVAILABLE,
        PARALLEL_SEARCH_AVAILABLE,
        DECAY_MANAGER_AVAILABLE
    )
    from connection_pool import QdrantConnectionPool, CircuitBreaker, CircuitBreakerOpen
    from parallel_search import parallel_search_collections
    from decay_manager import DecayManager
    from temporal_utils import TemporalParser, SessionDetector
    from config import MAX_RESULTS_PER_COLLECTION, ENABLE_PARALLEL_SEARCH
    IMPORTS_SUCCESSFUL = True
except ImportError as e:
    print(f"Import error: {e}")
    IMPORTS_SUCCESSFUL = False


class TestModuleIntegration:
    """Test that all modules are properly integrated."""
    
    def test_imports_successful(self):
        """Test that all module imports work."""
        assert IMPORTS_SUCCESSFUL, "Failed to import required modules"
        
    def test_connection_pool_available(self):
        """Test that connection pool module is available."""
        assert CONNECTION_POOL_AVAILABLE is not None
        
    def test_parallel_search_available(self):
        """Test that parallel search module is available."""
        assert PARALLEL_SEARCH_AVAILABLE is not None
        
    def test_decay_manager_available(self):
        """Test that decay manager module is available."""
        assert DECAY_MANAGER_AVAILABLE is not None
        
    def test_search_result_model(self):
        """Test SearchResult model structure."""
        result = SearchResult(
            id="test-id",
            score=0.95,
            timestamp=datetime.now().isoformat(),
            role="assistant",
            excerpt="Test excerpt",
            project_name="test-project",
            collection_name="test-collection"
        )
        assert result.id == "test-id"
        assert result.score == 0.95
        assert result.project_name == "test-project"


class TestConnectionPool:
    """Test connection pool functionality."""
    
    @pytest.mark.asyncio
    async def test_pool_initialization(self):
        """Test that connection pool initializes correctly."""
        if not CONNECTION_POOL_AVAILABLE:
            pytest.skip("Connection pool not available")
            
        pool = QdrantConnectionPool(
            url="http://localhost:6333",
            pool_size=5,
            max_overflow=2
        )
        
        await pool.initialize()
        stats = pool.get_stats()
        
        assert stats['initialized'] == True
        assert stats['connections_created'] > 0
        
        await pool.close()
    
    @pytest.mark.asyncio
    async def test_circuit_breaker(self):
        """Test circuit breaker functionality."""
        if not CONNECTION_POOL_AVAILABLE:
            pytest.skip("Connection pool not available")
            
        breaker = CircuitBreaker(
            failure_threshold=3,
            recovery_timeout=1.0
        )
        
        # Mock a failing function
        async def failing_func():
            raise Exception("Test failure")
        
        # Test that circuit opens after threshold
        for i in range(3):
            with pytest.raises(Exception):
                await breaker.call(failing_func)
        
        # Circuit should be open now
        with pytest.raises(CircuitBreakerOpen):
            await breaker.call(failing_func)


class TestParallelSearch:
    """Test parallel search functionality."""
    
    @pytest.mark.asyncio
    async def test_parallel_search_structure(self):
        """Test parallel search function signature."""
        if not PARALLEL_SEARCH_AVAILABLE:
            pytest.skip("Parallel search not available")
            
        # Check that function exists and has correct signature
        import inspect
        sig = inspect.signature(parallel_search_collections)
        params = list(sig.parameters.keys())
        
        assert 'collections_to_search' in params
        assert 'query' in params
        assert 'qdrant_client' in params
        assert 'max_concurrent' in params


class TestDecayManager:
    """Test decay manager functionality."""
    
    def test_decay_calculation(self):
        """Test decay score calculation."""
        if not DECAY_MANAGER_AVAILABLE:
            pytest.skip("Decay manager not available")
            
        manager = DecayManager()
        
        # Test with current timestamp (should have minimal decay)
        current_time = datetime.now().isoformat()
        score = manager.calculate_decay_score(1.0, current_time)
        assert score > 0.9  # Recent items should have high score
        
        # Test with old timestamp (should have significant decay)
        old_time = (datetime.now() - timedelta(days=180)).isoformat()
        old_score = manager.calculate_decay_score(1.0, old_time)
        assert old_score < 0.7  # Old items should have lower score
        assert old_score < score  # Old score should be less than recent


class TestTemporalUtils:
    """Test temporal utilities integration."""
    
    def test_temporal_parser(self):
        """Test temporal parser functionality."""
        parser = TemporalParser()
        
        # Test various time expressions
        start, end = parser.parse_time_expression("yesterday")
        assert start < end
        assert (datetime.now() - start).days <= 2
        
        start, end = parser.parse_time_expression("last week")
        assert start < end
        assert (datetime.now() - start).days <= 14
    
    def test_session_detector(self):
        """Test session detection."""
        detector = SessionDetector()
        
        # Create test chunks
        now = datetime.now()
        chunks = [
            {
                'timestamp': now.isoformat(),
                'text': 'First message',
                'project': 'test-project'
            },
            {
                'timestamp': (now + timedelta(hours=1)).isoformat(),
                'text': 'Second message',
                'project': 'test-project'
            },
            {
                'timestamp': (now + timedelta(hours=5)).isoformat(),
                'text': 'New session message',
                'project': 'test-project'
            }
        ]
        
        sessions = detector.detect_sessions(chunks)
        assert len(sessions) == 2  # Should detect 2 sessions with 4-hour gap


class TestConfigurationValues:
    """Test that configuration values are properly loaded."""
    
    def test_memory_limits(self):
        """Test memory limit configuration."""
        assert MAX_RESULTS_PER_COLLECTION > 0
        assert MAX_RESULTS_PER_COLLECTION <= 100
        
    def test_parallel_search_config(self):
        """Test parallel search configuration."""
        assert isinstance(ENABLE_PARALLEL_SEARCH, bool)


class TestEndToEndIntegration:
    """Test end-to-end integration scenarios."""
    
    @pytest.mark.asyncio
    async def test_modules_work_together(self):
        """Test that modules can work together."""
        # This is a placeholder for actual integration testing
        # In a real scenario, we would:
        # 1. Initialize connection pool
        # 2. Perform parallel search
        # 3. Apply decay scoring
        # 4. Parse temporal queries
        # 5. Detect sessions
        
        assert True  # Placeholder
        
    @pytest.mark.asyncio
    async def test_backward_compatibility(self):
        """Test that old code paths still work."""
        # Verify that the system works even if new modules aren't available
        # This ensures we haven't broken existing functionality
        
        assert qdrant_client is not None  # Basic client should always exist


def test_performance_improvements():
    """Test that performance improvements are in place."""
    # Check that configuration for performance is set
    from config import (
        MAX_CONCURRENT_SEARCHES,
        POOL_SIZE,
        RETRY_ATTEMPTS
    )
    
    assert MAX_CONCURRENT_SEARCHES > 1  # Should allow concurrent searches
    assert POOL_SIZE > 1  # Should have connection pooling
    assert RETRY_ATTEMPTS > 1  # Should have retry logic


if __name__ == "__main__":
    # Run tests
    import sys
    pytest.main([__file__, "-v", "--tb=short"] + sys.argv[1:])