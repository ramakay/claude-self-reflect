"""Core domain models for the import system."""

from dataclasses import dataclass, field
from datetime import datetime
from typing import List, Dict, Any, Optional, Set
from uuid import UUID, uuid4


@dataclass
class Message:
    """A single message in a conversation."""
    
    role: str
    content: str
    timestamp: Optional[datetime] = None
    message_index: int = 0
    metadata: Dict[str, Any] = field(default_factory=dict)
    
    def __post_init__(self):
        """Validate message on creation."""
        if not self.role:
            raise ValueError("Message role cannot be empty")
        if self.role not in {"user", "assistant", "system", "human"}:
            # Allow common variations
            pass  # Log warning but don't fail
        if self.message_index < 0:
            raise ValueError(f"Message index cannot be negative: {self.message_index}")


@dataclass
class ConversationChunk:
    """A chunk of conversation ready for embedding."""
    
    text: str
    metadata: Dict[str, Any] = field(default_factory=dict)
    message_indices: List[int] = field(default_factory=list)
    chunk_index: int = 0
    total_chunks: int = 1
    conversation_id: str = field(default_factory=lambda: str(uuid4()))
    
    def __post_init__(self):
        """Validate chunk on creation."""
        if not self.text:
            raise ValueError("Chunk text cannot be empty")
        if self.chunk_index < 0:
            raise ValueError(f"Chunk index cannot be negative: {self.chunk_index}")
        if self.chunk_index >= self.total_chunks:
            raise ValueError(
                f"Chunk index ({self.chunk_index}) must be less than "
                f"total chunks ({self.total_chunks})"
            )
    
    @property
    def unique_id(self) -> str:
        """Generate unique ID for this chunk."""
        return f"{self.conversation_id}_{self.chunk_index}"
    
    def add_metadata(self, key: str, value: Any) -> None:
        """Add metadata with conflict detection."""
        if key in self.metadata:
            # Handle conflict - could merge, replace, or raise
            if isinstance(self.metadata[key], list) and isinstance(value, list):
                # Merge lists
                self.metadata[key] = list(set(self.metadata[key] + value))
            else:
                # Replace value (log warning in production)
                self.metadata[key] = value
        else:
            self.metadata[key] = value


@dataclass
class ProcessedPoint:
    """A fully processed point ready for storage."""
    
    id: str
    vector: List[float]
    payload: Dict[str, Any]
    
    def __post_init__(self):
        """Validate point on creation."""
        if not self.id:
            raise ValueError("Point ID cannot be empty")
        if not self.vector:
            raise ValueError("Point vector cannot be empty")
        if not isinstance(self.vector, list):
            raise TypeError(f"Vector must be a list, got {type(self.vector)}")
        if not all(isinstance(x, (int, float)) for x in self.vector):
            raise TypeError("Vector must contain only numeric values")
        if not self.payload:
            # Empty payload is allowed but unusual
            pass
    
    @property
    def dimension(self) -> int:
        """Get vector dimension."""
        return len(self.vector)
    
    def validate_dimension(self, expected: int) -> bool:
        """Check if vector has expected dimension."""
        return self.dimension == expected


@dataclass
class ImportResult:
    """Result of an import operation."""
    
    file_path: str
    success: bool
    points_created: int = 0
    chunks_processed: int = 0
    error: Optional[str] = None
    duration_seconds: float = 0.0
    metadata: Dict[str, Any] = field(default_factory=dict)
    
    @property
    def failed(self) -> bool:
        """Check if import failed."""
        return not self.success
    
    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for serialization."""
        return {
            "file_path": self.file_path,
            "success": self.success,
            "points_created": self.points_created,
            "chunks_processed": self.chunks_processed,
            "error": self.error,
            "duration_seconds": self.duration_seconds,
            "metadata": self.metadata
        }


@dataclass
class ImportStats:
    """Aggregate statistics for import operations."""
    
    total_files: int = 0
    successful_files: int = 0
    failed_files: int = 0
    total_points: int = 0
    total_chunks: int = 0
    total_duration_seconds: float = 0.0
    errors: List[str] = field(default_factory=list)
    
    def add_result(self, result: ImportResult) -> None:
        """Add a result to the statistics."""
        self.total_files += 1
        if result.success:
            self.successful_files += 1
            self.total_points += result.points_created
            self.total_chunks += result.chunks_processed
        else:
            self.failed_files += 1
            if result.error:
                self.errors.append(f"{result.file_path}: {result.error}")
        self.total_duration_seconds += result.duration_seconds
    
    @property
    def success_rate(self) -> float:
        """Calculate success rate."""
        if self.total_files == 0:
            return 0.0
        return self.successful_files / self.total_files * 100
    
    @property
    def average_duration(self) -> float:
        """Calculate average import duration."""
        if self.total_files == 0:
            return 0.0
        return self.total_duration_seconds / self.total_files
    
    def summary(self) -> str:
        """Generate summary string."""
        return (
            f"Import Statistics:\n"
            f"  Total Files: {self.total_files}\n"
            f"  Successful: {self.successful_files} ({self.success_rate:.1f}%)\n"
            f"  Failed: {self.failed_files}\n"
            f"  Total Points: {self.total_points}\n"
            f"  Total Chunks: {self.total_chunks}\n"
            f"  Total Duration: {self.total_duration_seconds:.2f}s\n"
            f"  Average Duration: {self.average_duration:.2f}s per file"
        )