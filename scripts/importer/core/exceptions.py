"""Custom exception hierarchy for import system."""

from typing import Optional, Any


class ImportError(Exception):
    """Base exception for all import-related errors."""
    
    def __init__(self, message: str, details: Optional[dict] = None):
        super().__init__(message)
        self.details = details or {}


class ValidationError(ImportError):
    """Raised when input validation fails."""
    
    def __init__(self, field: str, value: Any, reason: str):
        super().__init__(f"Validation failed for {field}: {reason}")
        self.field = field
        self.value = value
        self.reason = reason


class EmbeddingError(ImportError):
    """Raised when embedding generation or validation fails."""
    
    def __init__(self, message: str, provider: Optional[str] = None):
        super().__init__(message)
        self.provider = provider


class StorageError(ImportError):
    """Raised when storage operations fail."""
    
    def __init__(self, operation: str, collection: str, reason: str):
        super().__init__(f"Storage {operation} failed for {collection}: {reason}")
        self.operation = operation
        self.collection = collection


class ParseError(ImportError):
    """Raised when parsing conversation files fails."""
    
    def __init__(self, file_path: str, line_number: Optional[int] = None, reason: str = ""):
        message = f"Failed to parse {file_path}"
        if line_number:
            message += f" at line {line_number}"
        if reason:
            message += f": {reason}"
        super().__init__(message)
        self.file_path = file_path
        self.line_number = line_number