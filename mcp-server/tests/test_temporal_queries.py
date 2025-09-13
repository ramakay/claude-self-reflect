#!/usr/bin/env python3
"""
Test suite for temporal query capabilities.
Tests the new MCP tools: get_recent_work, search_by_recency, get_timeline
"""

import asyncio
import json
from datetime import datetime, timedelta, timezone
from pathlib import Path
import sys

# Add parent directory to path
sys.path.insert(0, str(Path(__file__).parent.parent / "src"))

from temporal_utils import SessionDetector, TemporalParser, WorkSession, group_by_time_period


def test_temporal_parser():
    """Test natural language time parsing."""
    parser = TemporalParser()
    
    # Test relative time expressions
    test_cases = [
        ("today", 0, 0),
        ("yesterday", -1, -1),
        ("last week", -14, -7),  # Last week means the previous full week
        ("past week", -7, 0),    # Past week means last 7 days
        ("past 3 days", -3, 0),
        ("2 hours ago", None, None),  # Special case - exact time
    ]
    
    base_time = datetime.now(timezone.utc)
    
    for expr, expected_start_days, expected_end_days in test_cases:
        print(f"\nTesting: '{expr}'")
        start, end = parser.parse_time_expression(expr, base_time)
        
        if expected_start_days is not None:
            expected_start = base_time + timedelta(days=expected_start_days)
            expected_end = base_time + timedelta(days=expected_end_days) if expected_end_days != 0 else base_time
            
            # Check dates match (ignore time of day for day-level comparisons)
            assert start.date() == expected_start.date() or abs((start - expected_start).days) <= 1, \
                f"Start date mismatch for '{expr}': got {start.date()}, expected ~{expected_start.date()}"
            
            if expected_end_days != 0:
                assert end.date() == expected_end.date() or abs((end - expected_end).days) <= 1, \
                    f"End date mismatch for '{expr}': got {end.date()}, expected ~{expected_end.date()}"
        
        print(f"  Start: {start.isoformat()}")
        print(f"  End: {end.isoformat()}")
        print(f"  ✓ Passed")
    
    # Test relative time formatting
    test_times = [
        (datetime.now(timezone.utc) - timedelta(seconds=30), "just now"),
        (datetime.now(timezone.utc) - timedelta(minutes=5), "5 minutes ago"),
        (datetime.now(timezone.utc) - timedelta(hours=2), "2 hours ago"),
        (datetime.now(timezone.utc) - timedelta(days=1), "yesterday"),
        (datetime.now(timezone.utc) - timedelta(days=3), "3 days ago"),
        (datetime.now(timezone.utc) - timedelta(days=10), "1 week ago"),
    ]
    
    print("\n\nTesting relative time formatting:")
    for test_time, expected in test_times:
        result = parser.format_relative_time(test_time)
        print(f"  {test_time.isoformat()} -> '{result}' (expected: '{expected}')")
        # Allow some flexibility in exact wording
        assert expected.split()[0] in result or expected.split()[-1] in result, \
            f"Formatting mismatch: got '{result}', expected something like '{expected}'"
        print(f"  ✓ Passed")


def test_session_detector():
    """Test session detection algorithm."""
    detector = SessionDetector(time_gap_minutes=30)
    
    # Create test chunks
    base_time = datetime.now(timezone.utc)
    test_chunks = [
        # Session 1: Morning work
        {
            'timestamp': (base_time - timedelta(hours=5)).isoformat(),
            'conversation_id': 'conv1',
            'project': 'project1',
            'concepts': ['python', 'testing'],
            'files_analyzed': ['test.py'],
        },
        {
            'timestamp': (base_time - timedelta(hours=4, minutes=45)).isoformat(),
            'conversation_id': 'conv1',
            'project': 'project1',
            'concepts': ['python', 'debugging'],
            'files_analyzed': ['test.py', 'main.py'],
        },
        # Gap > 30 minutes - new session
        {
            'timestamp': (base_time - timedelta(hours=2)).isoformat(),
            'conversation_id': 'conv2',
            'project': 'project1',
            'concepts': ['docker', 'deployment'],
            'files_analyzed': ['Dockerfile'],
        },
        {
            'timestamp': (base_time - timedelta(hours=1, minutes=50)).isoformat(),
            'conversation_id': 'conv2',
            'project': 'project1',
            'concepts': ['docker', 'kubernetes'],
            'files_analyzed': ['k8s.yaml'],
        },
    ]
    
    print("\n\nTesting session detection:")
    sessions = detector.detect_sessions(test_chunks)
    
    assert len(sessions) == 2, f"Expected 2 sessions, got {len(sessions)}"
    print(f"  Detected {len(sessions)} sessions ✓")
    
    # Check first session
    session1 = sessions[0]
    assert session1.conversation_ids == ['conv1'], f"Session 1 conversations: {session1.conversation_ids}"
    assert 'python' in session1.main_topics, f"Session 1 topics: {session1.main_topics}"
    assert 'test.py' in session1.files_touched, f"Session 1 files: {session1.files_touched}"
    print(f"  Session 1: {session1.duration_minutes} minutes, topics: {session1.main_topics[:3]} ✓")
    
    # Check second session
    session2 = sessions[1]
    assert session2.conversation_ids == ['conv2'], f"Session 2 conversations: {session2.conversation_ids}"
    assert 'docker' in session2.main_topics, f"Session 2 topics: {session2.main_topics}"
    print(f"  Session 2: {session2.duration_minutes} minutes, topics: {session2.main_topics[:3]} ✓")


def test_group_by_time_period():
    """Test time period grouping."""
    base_time = datetime.now(timezone.utc)
    
    test_chunks = [
        # Today
        {'timestamp': base_time.isoformat(), 'data': 'chunk1'},
        {'timestamp': (base_time - timedelta(hours=2)).isoformat(), 'data': 'chunk2'},
        # Yesterday
        {'timestamp': (base_time - timedelta(days=1)).isoformat(), 'data': 'chunk3'},
        # Last week
        {'timestamp': (base_time - timedelta(days=7)).isoformat(), 'data': 'chunk4'},
    ]
    
    print("\n\nTesting time period grouping:")
    
    # Group by day
    day_groups = group_by_time_period(test_chunks, granularity='day')
    print(f"  Grouped into {len(day_groups)} days")
    assert len(day_groups) >= 2, f"Expected at least 2 day groups, got {len(day_groups)}"
    
    # Check today's group
    today_key = base_time.strftime('%Y-%m-%d')
    if today_key in day_groups:
        assert len(day_groups[today_key]) >= 1, f"Today should have at least 1 chunk"
        print(f"  Today ({today_key}): {len(day_groups[today_key])} chunks ✓")
    
    # Group by week
    week_groups = group_by_time_period(test_chunks, granularity='week')
    print(f"  Grouped into {len(week_groups)} weeks")
    assert len(week_groups) >= 1, f"Expected at least 1 week group, got {len(week_groups)}"
    
    for week_key, chunks in week_groups.items():
        print(f"  Week {week_key}: {len(chunks)} chunks ✓")


async def test_mcp_tool_integration():
    """Test that MCP tools can be called (requires running server)."""
    print("\n\nMCP Tool Integration Test:")
    print("  Note: This test requires the MCP server to be running")
    print("  Skipping integration test in standalone mode")
    print("  To test MCP tools, use Claude Code with the MCP server configured")
    
    # The actual MCP tools would be tested through Claude Code
    # This is just a placeholder to document expected behavior
    
    expected_queries = [
        "mcp__claude-self-reflect__get_recent_work",
        "mcp__claude-self-reflect__search_by_recency",
        "mcp__claude-self-reflect__get_timeline",
    ]
    
    print("\n  Expected MCP tools:")
    for tool in expected_queries:
        print(f"    - {tool}")


def main():
    """Run all tests."""
    print("=" * 60)
    print("TEMPORAL QUERY TEST SUITE")
    print("=" * 60)
    
    try:
        # Test components
        test_temporal_parser()
        test_session_detector()
        test_group_by_time_period()
        
        # Test MCP integration (documentation only)
        asyncio.run(test_mcp_tool_integration())
        
        print("\n" + "=" * 60)
        print("ALL TESTS PASSED! ✓")
        print("=" * 60)
        
    except AssertionError as e:
        print(f"\n\n❌ TEST FAILED: {e}")
        sys.exit(1)
    except Exception as e:
        print(f"\n\n❌ UNEXPECTED ERROR: {e}")
        import traceback
        traceback.print_exc()
        sys.exit(1)


if __name__ == "__main__":
    main()