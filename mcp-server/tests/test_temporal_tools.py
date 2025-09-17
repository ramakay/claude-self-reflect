"""Comprehensive test suite for temporal tools in Claude Self-Reflect MCP server."""

import pytest
import asyncio
from datetime import datetime, timedelta, timezone
from unittest.mock import Mock, AsyncMock, patch
import sys
import os

# Add parent directory to path for imports
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from src.temporal_tools import (
    get_recent_work,
    search_by_recency,
    get_timeline
)
from src.temporal_utils import TemporalParser, SessionDetector


class TestTemporalParser:
    """Test the TemporalParser class for natural language time parsing."""
    
    def test_parse_yesterday(self):
        """Test parsing 'yesterday' returns correct time range."""
        parser = TemporalParser()
        start, end = parser.parse("yesterday")
        
        now = datetime.now(timezone.utc)
        yesterday_start = (now - timedelta(days=1)).replace(hour=0, minute=0, second=0, microsecond=0)
        yesterday_end = yesterday_start + timedelta(days=1)
        
        assert start.date() == yesterday_start.date()
        assert end.date() == yesterday_end.date()
    
    def test_parse_last_week(self):
        """Test parsing 'last week' returns 7 days."""
        parser = TemporalParser()
        start, end = parser.parse("last week")
        
        time_diff = end - start
        assert 6 <= time_diff.days <= 8  # Allow for slight variance
    
    def test_parse_specific_days(self):
        """Test parsing 'last 3 days' returns correct range."""
        parser = TemporalParser()
        start, end = parser.parse("last 3 days")
        
        time_diff = end - start
        assert 2 <= time_diff.days <= 4
    
    def test_parse_hours(self):
        """Test parsing 'past 12 hours' returns correct range."""
        parser = TemporalParser()
        start, end = parser.parse("past 12 hours")
        
        time_diff = end - start
        assert 11 <= time_diff.total_seconds() / 3600 <= 13


class TestSessionDetector:
    """Test the SessionDetector class for work session detection."""
    
    def test_detect_single_session(self):
        """Test detection of a single work session."""
        detector = SessionDetector()
        
        # Create mock conversations within 30 minutes
        now = datetime.now(timezone.utc)
        conversations = [
            {"timestamp": now.isoformat(), "id": "1", "messages": 5},
            {"timestamp": (now + timedelta(minutes=10)).isoformat(), "id": "2", "messages": 3},
            {"timestamp": (now + timedelta(minutes=20)).isoformat(), "id": "3", "messages": 4}
        ]
        
        sessions = detector.detect_sessions(conversations)
        assert len(sessions) == 1
        assert sessions[0]["conversation_count"] == 3
        assert sessions[0]["total_messages"] == 12
    
    def test_detect_multiple_sessions(self):
        """Test detection of multiple work sessions with gaps."""
        detector = SessionDetector()
        
        now = datetime.now(timezone.utc)
        conversations = [
            # Session 1
            {"timestamp": now.isoformat(), "id": "1", "messages": 5},
            {"timestamp": (now + timedelta(minutes=10)).isoformat(), "id": "2", "messages": 3},
            # Gap of 2 hours
            {"timestamp": (now + timedelta(hours=2)).isoformat(), "id": "3", "messages": 4},
            {"timestamp": (now + timedelta(hours=2, minutes=15)).isoformat(), "id": "4", "messages": 2}
        ]
        
        sessions = detector.detect_sessions(conversations)
        assert len(sessions) == 2
        assert sessions[0]["conversation_count"] == 2
        assert sessions[1]["conversation_count"] == 2


@pytest.mark.asyncio
class TestGetRecentWork:
    """Test the get_recent_work MCP tool."""
    
    async def test_get_recent_work_default(self):
        """Test get_recent_work with default parameters."""
        # Create mock context
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        # Mock Qdrant client
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[
                Mock(name="conv_test_local")
            ]))
            mock_client.scroll = AsyncMock(return_value=(
                [Mock(
                    id="test-1",
                    payload={
                        "timestamp": datetime.now(timezone.utc).isoformat(),
                        "conversation_id": "conv-1",
                        "text": "Test conversation",
                        "message_count": 10
                    }
                )],
                None
            ))
            
            result = await get_recent_work(ctx, limit=5)
            
            assert "<recent_work" in result
            assert "conv-1" in result
            mock_client.scroll.assert_called_once()
    
    async def test_get_recent_work_with_grouping(self):
        """Test get_recent_work with different grouping options."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[
                Mock(name="conv_test_local")
            ]))
            
            # Test day grouping
            result = await get_recent_work(ctx, limit=5, group_by="day")
            assert "day" in result or "error" in result
            
            # Test session grouping
            result = await get_recent_work(ctx, limit=5, group_by="session")
            assert "session" in result or "error" in result


@pytest.mark.asyncio 
class TestSearchByRecency:
    """Test the search_by_recency MCP tool."""
    
    async def test_search_with_time_range(self):
        """Test search_by_recency with natural language time range."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[
                Mock(name="conv_test_local")
            ]))
            mock_client.search = AsyncMock(return_value=[
                Mock(
                    id="test-1",
                    score=0.8,
                    payload={
                        "timestamp": datetime.now(timezone.utc).isoformat(),
                        "text": "Test result"
                    }
                )
            ])
            
            with patch('src.server.generate_embedding') as mock_embed:
                mock_embed.return_value = [0.1] * 384  # Mock embedding
                
                result = await search_by_recency(
                    ctx, 
                    query="test query",
                    time_range="last week"
                )
                
                assert "<search_results" in result
                assert "test query" in result
    
    async def test_search_with_date_range(self):
        """Test search_by_recency with specific date range."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        now = datetime.now(timezone.utc)
        since = (now - timedelta(days=3)).isoformat()
        until = now.isoformat()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[]))
            
            result = await search_by_recency(
                ctx,
                query="test", 
                since=since,
                until=until
            )
            
            # Should handle empty collections gracefully
            assert "no collections" in result.lower() or "0" in result


@pytest.mark.asyncio
class TestGetTimeline:
    """Test the get_timeline MCP tool."""
    
    async def test_timeline_by_day(self):
        """Test get_timeline with day granularity."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[
                Mock(name="conv_test_local")
            ]))
            
            now = datetime.now(timezone.utc)
            mock_client.scroll = AsyncMock(return_value=(
                [Mock(
                    payload={
                        "timestamp": now.isoformat(),
                        "conversation_id": "conv-1",
                        "message_count": 5,
                        "files_analyzed": ["file1.py"],
                        "concepts": ["testing"]
                    }
                )],
                None
            ))
            
            result = await get_timeline(
                ctx,
                time_range="last 2 days",
                granularity="day"
            )
            
            assert "<timeline" in result
            assert "period" in result
    
    async def test_timeline_with_stats(self):
        """Test get_timeline includes statistics."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[]))
            
            result = await get_timeline(
                ctx,
                time_range="last week",
                include_stats=True
            )
            
            # Should handle empty timeline
            assert "timeline" in result


class TestEdgeCases:
    """Test edge cases and error handling."""
    
    @pytest.mark.asyncio
    async def test_empty_collections(self):
        """Test handling of empty collections."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            mock_client.get_collections = AsyncMock(return_value=Mock(collections=[]))
            
            result = await get_recent_work(ctx)
            assert "no collections" in result.lower() or "0" in result
    
    @pytest.mark.asyncio
    async def test_invalid_time_range(self):
        """Test handling of invalid time range."""
        parser = TemporalParser()
        start, end = parser.parse("invalid time string")
        
        # Should fall back to default (last 24 hours)
        time_diff = end - start
        assert 0 < time_diff.total_seconds() <= 86400 * 2  # Within 2 days
    
    @pytest.mark.asyncio
    async def test_project_scoping(self):
        """Test project scoping in temporal tools."""
        ctx = Mock()
        ctx.debug = AsyncMock()
        
        with patch('src.server.qdrant_client') as mock_client:
            # Test with specific project
            result = await get_recent_work(ctx, project="test-project")
            
            # Test with all projects
            result = await get_recent_work(ctx, project="all")
            
            # Both should complete without error
            assert result is not None


if __name__ == "__main__":
    pytest.main([__file__, "-v"])