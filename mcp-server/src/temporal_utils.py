"""
Temporal utilities for Claude Self Reflect.
Handles session detection, time parsing, and temporal query helpers.
"""

import re
from datetime import datetime, timedelta, timezone
from typing import List, Dict, Any, Optional, Tuple, Union
from functools import lru_cache
import logging
from dataclasses import dataclass
from collections import defaultdict

logger = logging.getLogger(__name__)

@dataclass
class WorkSession:
    """Represents a work session - a group of related conversations."""
    session_id: str
    start_time: datetime
    end_time: datetime
    conversation_ids: List[str]
    project: str
    duration_minutes: int
    main_topics: List[str]
    files_touched: List[str]
    message_count: int
    
    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            'session_id': self.session_id,
            'start_time': self.start_time.isoformat(),
            'end_time': self.end_time.isoformat(),
            'conversation_ids': self.conversation_ids,
            'project': self.project,
            'duration_minutes': self.duration_minutes,
            'main_topics': self.main_topics,
            'files_touched': self.files_touched,
            'message_count': self.message_count
        }


class SessionDetector:
    """Detects and groups conversations into work sessions."""
    
    def __init__(self, 
                 time_gap_minutes: int = 30,
                 min_session_chunks: int = 1,
                 merge_similar_topics: bool = True):
        """
        Initialize session detector.
        
        Args:
            time_gap_minutes: Minutes of inactivity to split sessions
            min_session_chunks: Minimum chunks to constitute a session
            merge_similar_topics: Whether to merge adjacent similar topics
        """
        self.time_gap = timedelta(minutes=time_gap_minutes)
        self.min_chunks = min_session_chunks
        self.merge_similar = merge_similar_topics
    
    def detect_sessions(self, chunks: List[Dict[str, Any]]) -> List[WorkSession]:
        """
        Group conversation chunks into work sessions.
        
        Args:
            chunks: List of conversation chunks with metadata
            
        Returns:
            List of WorkSession objects
        """
        if not chunks:
            return []
        
        # Sort by timestamp
        sorted_chunks = sorted(chunks, key=lambda x: self._parse_timestamp(x.get('timestamp')))
        
        sessions = []
        current_session_chunks = []
        
        for chunk in sorted_chunks:
            chunk_time = self._parse_timestamp(chunk.get('timestamp'))
            
            if not current_session_chunks:
                current_session_chunks.append(chunk)
                continue
            
            last_time = self._parse_timestamp(current_session_chunks[-1].get('timestamp'))
            time_gap = chunk_time - last_time
            
            # Check if we should start a new session
            if time_gap > self.time_gap or chunk.get('project') != current_session_chunks[-1].get('project'):
                # Finalize current session
                if len(current_session_chunks) >= self.min_chunks:
                    session = self._create_session(current_session_chunks)
                    if session:
                        sessions.append(session)
                current_session_chunks = [chunk]
            else:
                current_session_chunks.append(chunk)
        
        # Don't forget the last session
        if len(current_session_chunks) >= self.min_chunks:
            session = self._create_session(current_session_chunks)
            if session:
                sessions.append(session)
        
        return sessions
    
    def _create_session(self, chunks: List[Dict[str, Any]]) -> Optional[WorkSession]:
        """Create a WorkSession from chunks."""
        if not chunks:
            return None
        
        start_time = self._parse_timestamp(chunks[0].get('timestamp'))
        end_time = self._parse_timestamp(chunks[-1].get('timestamp'))
        
        # Aggregate metadata
        conversation_ids = list(set(c.get('conversation_id') for c in chunks if c.get('conversation_id')))
        files = []
        topics = []
        message_count = 0
        
        for chunk in chunks:
            if chunk.get('files_analyzed'):
                files.extend(chunk['files_analyzed'])
            if chunk.get('concepts'):
                topics.extend(chunk['concepts'])
            message_count += chunk.get('message_count', 1)
        
        # Deduplicate and limit
        files = list(set(files))[:20]
        
        # Get most common topics
        topic_counts = defaultdict(int)
        for topic in topics:
            topic_counts[topic] += 1
        main_topics = sorted(topic_counts.keys(), key=lambda x: topic_counts[x], reverse=True)[:10]
        
        duration_minutes = int((end_time - start_time).total_seconds() / 60)
        
        # Generate session ID from start time and project
        project = chunks[0].get('project', 'unknown')
        session_id = f"{project}_{start_time.strftime('%Y%m%d_%H%M%S')}"
        
        return WorkSession(
            session_id=session_id,
            start_time=start_time,
            end_time=end_time,
            conversation_ids=conversation_ids,
            project=project,
            duration_minutes=duration_minutes,
            main_topics=main_topics,
            files_touched=files,
            message_count=message_count
        )
    
    def _parse_timestamp(self, timestamp_str: str) -> datetime:
        """Parse timestamp string to datetime."""
        if not timestamp_str:
            return datetime.now(timezone.utc)
        
        # Handle ISO format with Z suffix
        if timestamp_str.endswith('Z'):
            timestamp_str = timestamp_str[:-1] + '+00:00'
        
        try:
            dt = datetime.fromisoformat(timestamp_str)
            if dt.tzinfo is None:
                dt = dt.replace(tzinfo=timezone.utc)
            return dt
        except (ValueError, AttributeError):
            logger.warning(f"Failed to parse timestamp: {timestamp_str}")
            return datetime.now(timezone.utc)


class TemporalParser:
    """Parses natural language time expressions."""
    
    def __init__(self):
        """Initialize the temporal parser."""
        self.relative_patterns = {
            'today': (0, 0),
            'yesterday': (-1, -1),
            'tomorrow': (1, 1),
            'this week': (-7, 0),
            'last week': (-14, -7),
            'this month': (-30, 0),
            'last month': (-60, -30),
            'past week': (-7, 0),
            'past month': (-30, 0),
            'past year': (-365, 0),
        }
    
    def parse_time_expression(self, 
                             expr: str, 
                             base_time: Optional[datetime] = None) -> Tuple[datetime, datetime]:
        """
        Parse natural language time expression into datetime range.
        
        Args:
            expr: Natural language time expression
            base_time: Base time for relative calculations (default: now)
            
        Returns:
            Tuple of (start_datetime, end_datetime)
        """
        if not base_time:
            base_time = datetime.now(timezone.utc)
        
        expr_lower = expr.lower().strip()
        
        # Check for ISO timestamp
        if self._looks_like_iso(expr):
            try:
                dt = datetime.fromisoformat(expr.replace('Z', '+00:00'))
                if dt.tzinfo is None:
                    dt = dt.replace(tzinfo=timezone.utc)
                return (dt, dt)
            except ValueError:
                pass
        
        # Check for relative patterns
        for pattern, (start_days, end_days) in self.relative_patterns.items():
            if pattern in expr_lower:
                start = base_time + timedelta(days=start_days)
                end = base_time + timedelta(days=end_days) if end_days != 0 else base_time
                
                # Adjust to day boundaries
                start = start.replace(hour=0, minute=0, second=0, microsecond=0)
                if end_days == 0:
                    end = base_time
                else:
                    end = end.replace(hour=23, minute=59, second=59, microsecond=999999)
                
                return (start, end)
        
        # Check for "N days/hours/minutes ago"
        ago_match = re.match(r'(\d+)\s*(day|hour|minute)s?\s*ago', expr_lower)
        if ago_match:
            amount = int(ago_match.group(1))
            unit = ago_match.group(2)
            
            if unit == 'day':
                delta = timedelta(days=amount)
            elif unit == 'hour':
                delta = timedelta(hours=amount)
            else:  # minute
                delta = timedelta(minutes=amount)
            
            target_time = base_time - delta
            return (target_time, target_time)
        
        # Check for "last N days/hours"
        last_match = re.match(r'(?:last|past)\s*(\d+)\s*(day|hour|minute)s?', expr_lower)
        if last_match:
            amount = int(last_match.group(1))
            unit = last_match.group(2)
            
            if unit == 'day':
                delta = timedelta(days=amount)
            elif unit == 'hour':
                delta = timedelta(hours=amount)
            else:  # minute
                delta = timedelta(minutes=amount)
            
            start = base_time - delta
            return (start, base_time)
        
        # Check for "since X"
        if expr_lower.startswith('since '):
            remaining = expr[6:].strip()
            start, _ = self.parse_time_expression(remaining, base_time)
            return (start, base_time)
        
        # Default: treat as "today"
        logger.warning(f"Could not parse time expression '{expr}', defaulting to today")
        start = base_time.replace(hour=0, minute=0, second=0, microsecond=0)
        return (start, base_time)
    
    @lru_cache(maxsize=128)
    def _looks_like_iso(self, expr: str) -> bool:
        """Check if string looks like ISO timestamp."""
        iso_pattern = r'^\d{4}-\d{2}-\d{2}'
        return bool(re.match(iso_pattern, expr))
    
    def format_relative_time(self, timestamp: Union[str, datetime]) -> str:
        """
        Format timestamp as relative time string.
        
        Args:
            timestamp: Timestamp to format
            
        Returns:
            Relative time string like "2 hours ago", "yesterday"
        """
        if isinstance(timestamp, str):
            if timestamp.endswith('Z'):
                timestamp = timestamp[:-1] + '+00:00'
            try:
                dt = datetime.fromisoformat(timestamp)
            except ValueError:
                return timestamp
        else:
            dt = timestamp
        
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        
        now = datetime.now(timezone.utc)
        delta = now - dt
        
        # Format based on time difference
        if delta.total_seconds() < 60:
            return "just now"
        elif delta.total_seconds() < 3600:
            minutes = int(delta.total_seconds() / 60)
            return f"{minutes} minute{'s' if minutes != 1 else ''} ago"
        elif delta.total_seconds() < 86400:
            hours = int(delta.total_seconds() / 3600)
            return f"{hours} hour{'s' if hours != 1 else ''} ago"
        elif delta.days == 1:
            return "yesterday"
        elif delta.days < 7:
            return f"{delta.days} days ago"
        elif delta.days < 30:
            weeks = delta.days // 7
            return f"{weeks} week{'s' if weeks != 1 else ''} ago"
        elif delta.days < 365:
            months = delta.days // 30
            return f"{months} month{'s' if months != 1 else ''} ago"
        else:
            years = delta.days // 365
            return f"{years} year{'s' if years != 1 else ''} ago"


def group_by_time_period(chunks: List[Dict[str, Any]], 
                         granularity: str = 'day') -> Dict[str, List[Dict[str, Any]]]:
    """
    Group chunks by time period.
    
    Args:
        chunks: List of conversation chunks
        granularity: 'hour', 'day', 'week', or 'month'
        
    Returns:
        Dictionary mapping time period keys to chunks
    """
    grouped = defaultdict(list)
    
    for chunk in chunks:
        timestamp_str = chunk.get('timestamp')
        if not timestamp_str:
            continue
        
        if timestamp_str.endswith('Z'):
            timestamp_str = timestamp_str[:-1] + '+00:00'
        
        try:
            dt = datetime.fromisoformat(timestamp_str)
        except ValueError:
            continue
        
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        
        # Generate period key based on granularity
        if granularity == 'hour':
            key = dt.strftime('%Y-%m-%d %H:00')
        elif granularity == 'day':
            key = dt.strftime('%Y-%m-%d')
        elif granularity == 'week':
            # Get start of week
            week_start = dt - timedelta(days=dt.weekday())
            key = week_start.strftime('%Y-W%V')
        elif granularity == 'month':
            key = dt.strftime('%Y-%m')
        else:
            key = dt.strftime('%Y-%m-%d')
        
        grouped[key].append(chunk)
    
    return dict(grouped)