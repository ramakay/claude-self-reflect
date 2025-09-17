"""Integration tests for MCP tools end-to-end functionality."""

import pytest
import asyncio
import json
from datetime import datetime, timezone
from unittest.mock import Mock, AsyncMock, patch, MagicMock
import sys
import os

# Add parent directory to path for imports
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from src.search_tools import (
    reflect_on_past,
    quick_search,
    search_summary,
    get_more_results,
    search_by_file,
    search_by_concept,
    get_next_results
)
from src.temporal_tools import (
    get_recent_work,
    search_by_recency,
    get_timeline
)
from src.reflection_tools import (
    store_reflection,
    get_full_conversation
)


class TestMCPToolInvocation:
    """Test that all MCP tools can be invoked correctly."""
    
    @pytest.mark.asyncio
    async def test_reflect_on_past_invocation(self):
        """Test reflect_on_past MCP tool invocation."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[]))
            
            # Test with all parameters
            result = await reflect_on_past(
                ctx,
                query="test query",
                limit=10,
                min_score=0.5,
                use_decay=1,
                project="test-project",
                mode="full",
                brief=False,
                include_raw=False,
                response_format="xml"
            )
            
            assert result is not None
            assert isinstance(result, str)
    
    @pytest.mark.asyncio
    async def test_store_reflection_invocation(self):
        """Test store_reflection MCP tool invocation."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[]))
            
            result = await store_reflection(
                ctx,
                content="Test reflection content",
                tags=["test", "integration"]
            )
            
            assert result is not None
            assert "stored" in result.lower() or "reflection" in result.lower()
    
    @pytest.mark.asyncio
    async def test_temporal_tools_invocation(self):
        """Test all temporal MCP tools can be invoked."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[]))
            
            # Test get_recent_work
            result1 = await get_recent_work(
                ctx,
                limit=5,
                project="test",
                group_by="conversation",
                include_reflections=True
            )
            assert result1 is not None
            
            # Test search_by_recency
            with patch('src.server.generate_embedding') as mock_embed:
                mock_embed.return_value = [0.1] * 384
                
                result2 = await search_by_recency(
                    ctx,
                    query="test",
                    time_range="last week",
                    since=None,
                    until=None,
                    limit=10,
                    min_score=0.3,
                    project="test"
                )
                assert result2 is not None
            
            # Test get_timeline
            result3 = await get_timeline(
                ctx,
                time_range="last week",
                granularity="day",
                project="test",
                include_stats=True
            )
            assert result3 is not None
    
    @pytest.mark.asyncio
    async def test_search_variant_tools(self):
        """Test search variant MCP tools."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[]))
            
            with patch('src.server.generate_embedding') as mock_embed:
                mock_embed.return_value = [0.1] * 384
                
                # Test quick_search
                result1 = await quick_search(
                    ctx,
                    query="quick test",
                    min_score=0.3,
                    project="test"
                )
                assert result1 is not None
                
                # Test search_summary
                result2 = await search_summary(
                    ctx,
                    query="summary test",
                    project="test"
                )
                assert result2 is not None
                
                # Test get_more_results
                result3 = await get_more_results(
                    ctx,
                    query="more test",
                    offset=3,
                    limit=3,
                    min_score=0.3,
                    project="test"
                )
                assert result3 is not None
    
    @pytest.mark.asyncio
    async def test_metadata_search_tools(self):
        """Test file and concept search tools."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[]))
            
            # Test search_by_file
            result1 = await search_by_file(
                ctx,
                file_path="/test/file.py",
                limit=10,
                project="test"
            )
            assert result1 is not None
            
            # Test search_by_concept
            result2 = await search_by_concept(
                ctx,
                concept="testing",
                limit=10,
                include_files=True,
                project="test"
            )
            assert result2 is not None
    
    @pytest.mark.asyncio
    async def test_get_full_conversation(self):
        """Test get_full_conversation tool."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.os.path.exists') as mock_exists:
            mock_exists.return_value = False
            
            result = await get_full_conversation(
                ctx,
                conversation_id="test-conv-id",
                project="test"
            )
            
            assert result is not None
            assert "not found" in result.lower() or "error" in result.lower()


class TestMCPErrorHandling:
    """Test error handling in MCP tools."""
    
    @pytest.mark.asyncio
    async def test_handles_qdrant_connection_error(self):
        """Test handling of Qdrant connection errors."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            # Simulate connection error
            mock_client.get_collections = AsyncMock(
                side_effect=Exception("Connection refused")
            )
            
            result = await reflect_on_past(ctx, query="test")
            
            # Should handle error gracefully
            assert "error" in result.lower() or "failed" in result.lower()
            assert "Connection refused" in result
    
    @pytest.mark.asyncio
    async def test_handles_embedding_generation_error(self):
        """Test handling of embedding generation errors."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[
                Mock(name="conv_test_local")
            ]))
            
            with patch('src.server.generate_embedding') as mock_embed:
                mock_embed.side_effect = Exception("Embedding model error")
                
                result = await reflect_on_past(ctx, query="test")
                
                # Should handle error gracefully
                assert "error" in result.lower()
                assert "Embedding" in result
    
    @pytest.mark.asyncio
    async def test_handles_invalid_parameters(self):
        """Test handling of invalid parameters."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[]))
            
            # Test with invalid limit
            result = await reflect_on_past(
                ctx,
                query="test",
                limit=-1  # Invalid
            )
            # Should either handle gracefully or use default
            assert result is not None
            
            # Test with invalid min_score
            result = await reflect_on_past(
                ctx,
                query="test",
                min_score=2.0  # Out of range
            )
            assert result is not None


class TestMCPPerformance:
    """Test performance characteristics of MCP tools."""
    
    @pytest.mark.asyncio
    async def test_search_with_large_collections(self):
        """Test search performance with many collections."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            # Simulate many collections
            large_collections = [
                Mock(name=f"conv_test_{i}_local") for i in range(100)
            ]
            mock_client.get_collections = AsyncMock(return_value=Mock(
                collections=large_collections
            ))
            
            # Each collection returns empty results quickly
            mock_client.search = AsyncMock(return_value=[])
            
            with patch('src.server.generate_embedding') as mock_embed:
                mock_embed.return_value = [0.1] * 384
                
                import time
                start = time.time()
                
                result = await reflect_on_past(
                    ctx,
                    query="test",
                    limit=5
                )
                
                elapsed = time.time() - start
                
                # Should complete in reasonable time even with 100 collections
                assert elapsed < 10  # 10 seconds max
                assert result is not None
    
    @pytest.mark.asyncio
    async def test_pagination_efficiency(self):
        """Test that pagination doesn't re-search earlier results."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        search_count = 0
        
        async def mock_search(*args, **kwargs):
            nonlocal search_count
            search_count += 1
            offset = kwargs.get('offset', 0)
            # Return different results based on offset
            return [
                Mock(
                    id=f"test-{offset}",
                    score=0.9 - (offset * 0.1),
                    payload={
                        "timestamp": datetime.now(timezone.utc).isoformat(),
                        "text": f"Result at offset {offset}"
                    }
                )
            ]
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[
                Mock(name="conv_test_local")
            ]))
            mock_client.search = AsyncMock(side_effect=mock_search)
            
            with patch('src.server.generate_embedding') as mock_embed:
                mock_embed.return_value = [0.1] * 384
                
                # Get first page
                result1 = await reflect_on_past(ctx, query="test", limit=1)
                count1 = search_count
                
                # Get next page
                result2 = await get_next_results(ctx, query="test", offset=1)
                count2 = search_count
                
                # Should have made separate searches
                assert count2 > count1
                assert "Result at offset 0" in result1 or "test-0" in result1
                assert "Result at offset 1" in result2 or "test-1" in result2


class TestMCPEndToEnd:
    """End-to-end tests simulating real usage scenarios."""
    
    @pytest.mark.asyncio
    async def test_complete_search_workflow(self):
        """Test complete search workflow: search -> get more -> full conversation."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[
                Mock(name="conv_test_local")
            ]))
            
            # Initial search returns results
            mock_client.search = AsyncMock(return_value=[
                Mock(
                    id="test-1",
                    score=0.9,
                    payload={
                        "timestamp": datetime.now(timezone.utc).isoformat(),
                        "text": "First result",
                        "conversation_id": "conv-123"
                    }
                )
            ])
            
            with patch('src.server.generate_embedding') as mock_embed:
                mock_embed.return_value = [0.1] * 384
                
                # Step 1: Initial search
                result1 = await reflect_on_past(ctx, query="test bug", limit=1)
                assert "First result" in result1
                assert "conv-123" in result1
                
                # Step 2: Get more results
                mock_client.search = AsyncMock(return_value=[
                    Mock(
                        id="test-2",
                        score=0.7,
                        payload={
                            "timestamp": datetime.now(timezone.utc).isoformat(),
                            "text": "Second result",
                            "conversation_id": "conv-456"
                        }
                    )
                ])
                
                result2 = await get_next_results(ctx, query="test bug", offset=1)
                assert "Second result" in result2
                
                # Step 3: Get full conversation
                with patch('src.server.os.path.exists') as mock_exists:
                    mock_exists.return_value = True
                    
                    with patch('builtins.open', mock=True) as mock_open:
                        mock_open.return_value.__enter__.return_value.read.return_value = (
                            '{"messages": [{"content": "Full conversation"}]}'
                        )
                        
                        result3 = await get_full_conversation(
                            ctx,
                            conversation_id="conv-123"
                        )
                        assert "Full conversation" in result3 or "conv-123" in result3
    
    @pytest.mark.asyncio
    async def test_reflection_storage_and_retrieval(self):
        """Test storing a reflection and retrieving it."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            # Setup for storage
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[]))
            mock_client.recreate_collection = AsyncMock()
            mock_client.upsert = AsyncMock()
            
            with patch('src.server.generate_embedding') as mock_embed:
                mock_embed.return_value = [0.1] * 384
                
                # Store reflection
                store_result = await store_reflection(
                    ctx,
                    content="Important insight about testing",
                    tags=["testing", "insight"]
                )
                assert "stored" in store_result.lower()
                
                # Setup for retrieval
                mock_client.get_collections = AsyncMock(return_value=Mock(collections=[
                    Mock(name="reflections")
                ]))
                mock_client.search = AsyncMock(return_value=[
                    Mock(
                        id="reflection-1",
                        score=0.95,
                        payload={
                            "timestamp": datetime.now(timezone.utc).isoformat(),
                            "text": "Important insight about testing",
                            "tags": ["testing", "insight"],
                            "type": "reflection"
                        }
                    )
                ])
                
                # Search for reflection
                search_result = await reflect_on_past(
                    ctx,
                    query="testing insights",
                    include_reflections=True
                )
                assert "Important insight about testing" in search_result
    
    @pytest.mark.asyncio
    async def test_temporal_workflow(self):
        """Test temporal query workflow: recent work -> timeline -> recency search."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            # Mock collection for all temporal queries
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[
                Mock(name="conv_test_local")
            ]))
            
            now = datetime.now(timezone.utc)
            
            # Step 1: Get recent work
            mock_client.scroll = AsyncMock(return_value=(
                [
                    Mock(payload={
                        "timestamp": now.isoformat(),
                        "conversation_id": "recent-1",
                        "text": "Recent work item",
                        "message_count": 5
                    })
                ],
                None
            ))
            
            recent_result = await get_recent_work(ctx, limit=5)
            assert "Recent work item" in recent_result or "recent-1" in recent_result
            
            # Step 2: Get timeline
            timeline_result = await get_timeline(
                ctx,
                time_range="last 24 hours",
                granularity="hour"
            )
            assert "timeline" in timeline_result.lower()
            
            # Step 3: Search with recency constraint
            mock_client.search = AsyncMock(return_value=[
                Mock(
                    id="recent-search-1",
                    score=0.8,
                    payload={
                        "timestamp": now.isoformat(),
                        "text": "Recent search result"
                    }
                )
            ])
            
            with patch('src.server.generate_embedding') as mock_embed:
                mock_embed.return_value = [0.1] * 384
                
                recency_result = await search_by_recency(
                    ctx,
                    query="recent work",
                    time_range="last hour"
                )
                assert "Recent search result" in recency_result or "recent-search-1" in recency_result


if __name__ == "__main__":
    pytest.main([__file__, "-v"])