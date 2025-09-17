"""Test suite for bug fixes in v3.3.x (native decay, XML injection, pagination)."""

import pytest
import asyncio
from datetime import datetime, timedelta, timezone
from unittest.mock import Mock, AsyncMock, patch, MagicMock
from xml.sax.saxutils import escape
import sys
import os
import json

# Add parent directory to path for imports
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from src.search_tools import reflect_on_past, get_next_results
from src.reflection_tools import store_reflection


class TestNativeDecayBugFix:
    """Test the native decay path result processing bug fix."""
    
    @pytest.mark.asyncio
    async def test_native_decay_appends_results(self):
        """Test that native decay path correctly appends results to all_results."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            # Mock collection with native vectors
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[
                Mock(name="conv_test_local")
            ]))
            
            # Mock collection info for native vectors check
            mock_client.get_collection = AsyncMock(return_value=Mock(
                config=Mock(params=Mock(vectors=Mock(size=384)))
            ))
            
            # Mock query_points for native decay
            mock_points = [
                Mock(
                    id="test-1",
                    payload={
                        "timestamp": datetime.now(timezone.utc).isoformat(),
                        "text": "Test result from native decay",
                        "conversation_id": "conv-1",
                        "message_count": 5
                    },
                    vector=None
                )
            ]
            mock_client.query_points = AsyncMock(return_value=Mock(
                points=mock_points
            ))
            
            with patch('src.server.generate_embedding') as mock_embed:
                mock_embed.return_value = [0.1] * 384
                
                with patch('src.server.USE_DECAY', True):
                    result = await reflect_on_past(
                        ctx,
                        query="test query",
                        limit=5,
                        use_decay=1  # Force decay
                    )
                    
                    # Verify results were processed and returned
                    assert "Test result from native decay" in result
                    assert "conv-1" in result
                    mock_client.query_points.assert_called_once()
    
    @pytest.mark.asyncio
    async def test_native_decay_handles_empty_results(self):
        """Test native decay handles empty results gracefully."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[
                Mock(name="conv_test_local")
            ]))
            mock_client.get_collection = AsyncMock(return_value=Mock(
                config=Mock(params=Mock(vectors=Mock(size=384)))
            ))
            
            # Return empty points
            mock_client.query_points = AsyncMock(return_value=Mock(points=[]))
            
            with patch('src.server.generate_embedding') as mock_embed:
                mock_embed.return_value = [0.1] * 384
                
                result = await reflect_on_past(
                    ctx,
                    query="test query",
                    use_decay=1
                )
                
                assert "No similar conversations found" in result or "0" in result
    
    @pytest.mark.asyncio
    async def test_native_decay_timestamp_cleaning(self):
        """Test that timestamps with 'Z' suffix are properly cleaned."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[
                Mock(name="conv_test_local")
            ]))
            mock_client.get_collection = AsyncMock(return_value=Mock(
                config=Mock(params=Mock(vectors=Mock(size=384)))
            ))
            
            # Use timestamp with 'Z' suffix that needs cleaning
            mock_points = [
                Mock(
                    id="test-1",
                    payload={
                        "timestamp": "2025-01-12T10:30:00Z",  # Z suffix
                        "text": "Test with Z timestamp",
                        "conversation_id": "conv-1"
                    },
                    vector=None
                )
            ]
            mock_client.query_points = AsyncMock(return_value=Mock(
                points=mock_points
            ))
            
            with patch('src.server.generate_embedding') as mock_embed:
                mock_embed.return_value = [0.1] * 384
                
                # Should not raise datetime parsing error
                result = await reflect_on_past(
                    ctx,
                    query="test",
                    use_decay=1
                )
                
                assert result is not None
                # Verify timestamp was processed without error
                assert "error" not in result.lower() or "Test with Z timestamp" in result


class TestXMLInjectionFix:
    """Test the XML injection vulnerability fix."""
    
    @pytest.mark.asyncio
    async def test_xml_escape_in_store_reflection(self):
        """Test that XML special characters are escaped in reflections."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[]))
            
            # Store reflection with XML special characters
            malicious_content = "Test <script>alert('XSS')</script> & entities"
            malicious_tags = ["<tag>", "&amp;", "'quotes'"]
            
            result = await store_reflection(
                ctx,
                content=malicious_content,
                tags=malicious_tags
            )
            
            # The result should have escaped XML characters
            if "<reflection>" in result:
                assert "<script>" not in result  # Should be escaped
                assert "&lt;script&gt;" in result or escape(malicious_content) in result
    
    @pytest.mark.asyncio
    async def test_xml_escape_in_search_results(self):
        """Test that search results escape XML in file/concept categories."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[
                Mock(name="conv_test_local")
            ]))
            
            # Create payload with XML-like file names
            mock_client.search = AsyncMock(return_value=[
                Mock(
                    id="test-1",
                    score=0.9,
                    payload={
                        "timestamp": datetime.now(timezone.utc).isoformat(),
                        "text": "Test result",
                        "files_analyzed": ["<script>.py", "file&name.js"],
                        "concepts": ["<injection>", "&entity"]
                    }
                )
            ])
            
            with patch('src.server.generate_embedding') as mock_embed:
                mock_embed.return_value = [0.1] * 384
                
                result = await reflect_on_past(
                    ctx,
                    query="test",
                    limit=1
                )
                
                # Verify XML characters are escaped
                if "files_analyzed" in result:
                    assert "<script>" not in result  # Should be escaped
                    assert "&lt;script&gt;" in result or escape("<script>") in result
    
    @pytest.mark.asyncio
    async def test_safe_xml_generation(self):
        """Test that normal content without XML chars works correctly."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[
                Mock(name="conv_test_local")
            ]))
            
            mock_client.search = AsyncMock(return_value=[
                Mock(
                    id="test-1",
                    score=0.9,
                    payload={
                        "timestamp": datetime.now(timezone.utc).isoformat(),
                        "text": "Normal test result",
                        "files_analyzed": ["server.py", "utils.js"],
                        "concepts": ["testing", "validation"]
                    }
                )
            ])
            
            with patch('src.server.generate_embedding') as mock_embed:
                mock_embed.return_value = [0.1] * 384
                
                result = await reflect_on_past(
                    ctx,
                    query="test",
                    limit=1
                )
                
                # Normal content should appear unchanged
                assert "server.py" in result
                assert "testing" in result


class TestPaginationWithPayloadFix:
    """Test the missing with_payload parameter fix in pagination."""
    
    @pytest.mark.asyncio
    async def test_get_next_results_includes_payload(self):
        """Test that get_next_results includes payload in search results."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[
                Mock(name="conv_test_local")
            ]))
            
            # Mock search with full payload
            mock_results = [
                Mock(
                    id="test-2",
                    score=0.7,
                    payload={
                        "timestamp": datetime.now(timezone.utc).isoformat(),
                        "text": "Second page result",
                        "conversation_id": "conv-2",
                        "message_count": 10,
                        "files_analyzed": ["file2.py"],
                        "concepts": ["pagination"]
                    }
                )
            ]
            mock_client.search = AsyncMock(return_value=mock_results)
            
            with patch('src.server.generate_embedding') as mock_embed:
                mock_embed.return_value = [0.1] * 384
                
                result = await get_next_results(
                    ctx,
                    query="test query",
                    offset=3,
                    limit=3
                )
                
                # Verify search was called with with_payload=True
                mock_client.search.assert_called_once()
                call_kwargs = mock_client.search.call_args.kwargs
                assert call_kwargs.get('with_payload') is True
                
                # Verify payload data is in results
                assert "Second page result" in result
                assert "conv-2" in result
                assert "pagination" in result
    
    @pytest.mark.asyncio
    async def test_pagination_preserves_all_metadata(self):
        """Test that pagination preserves all metadata fields."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[
                Mock(name="conv_test_local")
            ]))
            
            # Create result with all metadata fields
            full_payload = {
                "timestamp": datetime.now(timezone.utc).isoformat(),
                "text": "Full metadata test",
                "conversation_id": "conv-3",
                "message_count": 15,
                "files_analyzed": ["test.py", "main.js"],
                "files_edited": ["config.json"],
                "tools_used": ["Read", "Edit"],
                "concepts": ["testing", "metadata"],
                "project": "test-project",
                "chunk_index": 0,
                "total_chunks": 1
            }
            
            mock_client.search = AsyncMock(return_value=[
                Mock(id="test-3", score=0.8, payload=full_payload)
            ])
            
            with patch('src.server.generate_embedding') as mock_embed:
                mock_embed.return_value = [0.1] * 384
                
                result = await get_next_results(
                    ctx,
                    query="test",
                    offset=5
                )
                
                # Verify all metadata appears in result
                assert "Full metadata test" in result
                assert "test.py" in result
                assert "config.json" in result
                assert "testing" in result
    
    @pytest.mark.asyncio
    async def test_pagination_handles_empty_results(self):
        """Test pagination handles empty results gracefully."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[
                Mock(name="conv_test_local")
            ]))
            
            # Return empty results for high offset
            mock_client.search = AsyncMock(return_value=[])
            
            with patch('src.server.generate_embedding') as mock_embed:
                mock_embed.return_value = [0.1] * 384
                
                result = await get_next_results(
                    ctx,
                    query="test",
                    offset=100  # High offset likely to have no results
                )
                
                assert "No more results" in result or "0" in result


class TestIntegratedBugFixes:
    """Test that all bug fixes work together correctly."""
    
    @pytest.mark.asyncio
    async def test_all_fixes_integrated(self):
        """Test native decay + XML escape + pagination all work together."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[
                Mock(name="conv_test_local")
            ]))
            mock_client.get_collection = AsyncMock(return_value=Mock(
                config=Mock(params=Mock(vectors=Mock(size=384)))
            ))
            
            # Test with native decay returning XML-unsafe content
            mock_points = [
                Mock(
                    id="test-1",
                    payload={
                        "timestamp": "2025-01-12T10:00:00Z",  # Z suffix
                        "text": "Result with <tag> and & entity",
                        "conversation_id": "conv-1",
                        "files_analyzed": ["<script>.py"],
                        "concepts": ["&testing"]
                    },
                    vector=None
                )
            ]
            mock_client.query_points = AsyncMock(return_value=Mock(
                points=mock_points
            ))
            
            with patch('src.server.generate_embedding') as mock_embed:
                mock_embed.return_value = [0.1] * 384
                
                with patch('src.server.USE_DECAY', True):
                    # First search with decay
                    result1 = await reflect_on_past(
                        ctx,
                        query="test",
                        use_decay=1
                    )
                    
                    # Should have results and escaped XML
                    assert result1 is not None
                    assert "<script>" not in result1  # Should be escaped
                    
                    # Now test pagination
                    mock_client.search = AsyncMock(return_value=[
                        Mock(
                            id="test-2",
                            score=0.6,
                            payload={
                                "timestamp": datetime.now(timezone.utc).isoformat(),
                                "text": "Page 2 with <xml>",
                                "conversation_id": "conv-2"
                            }
                        )
                    ])
                    
                    result2 = await get_next_results(
                        ctx,
                        query="test",
                        offset=1
                    )
                    
                    # Pagination should work with payload
                    call_kwargs = mock_client.search.call_args.kwargs
                    assert call_kwargs.get('with_payload') is True
                    assert "<xml>" not in result2  # Should be escaped


if __name__ == "__main__":
    pytest.main([__file__, "-v"])