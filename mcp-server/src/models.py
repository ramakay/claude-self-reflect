"""Pydantic models for Claude Self-Reflect MCP server."""

from typing import Optional, List, Dict, Any, Set
from datetime import datetime
from pydantic import BaseModel, Field

class SearchResult(BaseModel):
    """Model for search results."""
    id: str
    score: float
    timestamp: str
    role: str
    excerpt: str
    project_name: str
    conversation_id: Optional[str] = None
    base_conversation_id: Optional[str] = None
    collection_name: str
    raw_payload: Optional[Dict[str, Any]] = None
    code_patterns: Optional[Dict[str, List[str]]] = None
    files_analyzed: Optional[List[str]] = None
    files_edited: Optional[List[str]] = None
    tools_used: Optional[List[str]] = None
    concepts: Optional[List[str]] = None

class ConversationGroup(BaseModel):
    """Model for grouped conversations."""
    conversation_id: str
    base_conversation_id: str
    timestamp: datetime
    message_count: int
    excerpts: List[str]
    files: Set[str] = Field(default_factory=set)
    tools: Set[str] = Field(default_factory=set)
    concepts: Set[str] = Field(default_factory=set)

class WorkSession(BaseModel):
    """Model for work sessions."""
    start_time: datetime
    end_time: datetime
    conversations: List[ConversationGroup]
    total_messages: int
    files_touched: Set[str] = Field(default_factory=set)
    tools_used: Set[str] = Field(default_factory=set)
    concepts: Set[str] = Field(default_factory=set)

class ActivityStats(BaseModel):
    """Model for activity statistics."""
    total_conversations: int
    total_messages: int
    unique_files: int
    unique_tools: int
    peak_hour: Optional[str] = None
    peak_day: Optional[str] = None

class TimelineEntry(BaseModel):
    """Model for timeline entries."""
    period: str
    start_time: datetime
    end_time: datetime
    conversation_count: int
    message_count: int
    files: Set[str] = Field(default_factory=set)
    tools: Set[str] = Field(default_factory=set)
    concepts: Set[str] = Field(default_factory=set)