"""Immutable configuration with validation."""

from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional
import os


@dataclass(frozen=True)
class ImportConfig:
    """
    Immutable configuration for the import system.
    
    All validation happens in __post_init__ to ensure configuration
    is always in a valid state.
    """
    
    # Qdrant settings
    qdrant_url: str = field(default="http://localhost:6333")
    qdrant_api_key: Optional[str] = field(default=None)
    
    # Embedding settings
    embedding_model: str = field(default="sentence-transformers/all-MiniLM-L6-v2")
    embedding_dimension: int = field(default=384)
    use_voyage: bool = field(default=False)
    voyage_api_key: Optional[str] = field(default=None)
    
    # Chunking settings
    chunk_size: int = field(default=3000)
    chunk_overlap: int = field(default=200)
    
    # Processing settings
    batch_size: int = field(default=10)
    max_ast_elements: int = field(default=100)
    max_workers: int = field(default=4)
    
    # State management
    state_file: str = field(default="~/.claude-self-reflect/config/imported-files.json")
    
    # Operational settings
    log_level: str = field(default="INFO")
    dry_run: bool = field(default=False)
    force_reimport: bool = field(default=False)
    
    # Limits
    file_limit: Optional[int] = field(default=None)
    
    def __post_init__(self):
        """Validate configuration on initialization."""
        # Validate chunk settings
        if self.chunk_size <= 0:
            raise ValueError(f"chunk_size must be positive, got {self.chunk_size}")
        
        if self.chunk_overlap < 0:
            raise ValueError(f"chunk_overlap cannot be negative, got {self.chunk_overlap}")
        
        if self.chunk_overlap >= self.chunk_size:
            raise ValueError(
                f"chunk_overlap ({self.chunk_overlap}) must be less than "
                f"chunk_size ({self.chunk_size})"
            )
        
        # Validate batch settings
        if self.batch_size < 1:
            raise ValueError(f"batch_size must be at least 1, got {self.batch_size}")
        
        if self.max_workers < 1:
            raise ValueError(f"max_workers must be at least 1, got {self.max_workers}")
        
        # Validate embedding settings
        if self.embedding_dimension <= 0:
            raise ValueError(f"embedding_dimension must be positive, got {self.embedding_dimension}")
        
        if self.use_voyage and not self.voyage_api_key:
            # Document the limitation of frozen dataclass
            voyage_key = os.getenv("VOYAGE_KEY")
            if not voyage_key:
                raise ValueError(
                    "voyage_api_key must be provided at initialization when use_voyage=True. "
                    "Set VOYAGE_KEY environment variable before creating config."
                )
        
        # Validate log level
        valid_levels = {"DEBUG", "INFO", "WARNING", "ERROR", "CRITICAL"}
        if self.log_level.upper() not in valid_levels:
            raise ValueError(f"log_level must be one of {valid_levels}, got {self.log_level}")
    
    @property
    def state_file_path(self) -> Path:
        """Get expanded state file path with fallback."""
        try:
            return Path(self.state_file).expanduser()
        except (RuntimeError, OSError):
            # Fallback to current directory if expansion fails
            return Path.cwd() / ".import-state.json"
    
    @classmethod
    def from_env(cls) -> "ImportConfig":
        """Create configuration from environment variables."""
        return cls(
            qdrant_url=os.getenv("QDRANT_URL", "http://localhost:6333"),
            qdrant_api_key=os.getenv("QDRANT_API_KEY"),
            use_voyage=os.getenv("USE_VOYAGE", "false").lower() == "true",
            voyage_api_key=os.getenv("VOYAGE_KEY"),
            chunk_size=int(os.getenv("CHUNK_SIZE", "3000")),
            chunk_overlap=int(os.getenv("CHUNK_OVERLAP", "200")),
            batch_size=int(os.getenv("BATCH_SIZE", "10")),
            max_workers=int(os.getenv("MAX_WORKERS", "4")),
            log_level=os.getenv("LOG_LEVEL", "INFO"),
            dry_run=os.getenv("DRY_RUN", "false").lower() == "true",
            force_reimport=os.getenv("FORCE_REIMPORT", "false").lower() == "true"
        )
    
    @classmethod
    def from_dict(cls, config_dict: dict) -> "ImportConfig":
        """Create configuration from dictionary."""
        # Filter out any unknown keys
        known_fields = {f.name for f in cls.__dataclass_fields__.values()}
        filtered_dict = {k: v for k, v in config_dict.items() if k in known_fields}
        return cls(**filtered_dict)