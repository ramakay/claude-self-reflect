"""Centralized logging configuration."""

import logging
import sys
from pathlib import Path
from datetime import datetime
from typing import Optional


def setup_logging(
    level: str = "INFO",
    log_file: Optional[str] = None,
    format_string: Optional[str] = None
) -> logging.Logger:
    """
    Set up logging configuration.
    
    Args:
        level: Log level (DEBUG, INFO, WARNING, ERROR, CRITICAL)
        log_file: Optional log file path
        format_string: Optional custom format string
        
    Returns:
        Configured root logger
    """
    # Default format
    if not format_string:
        format_string = "%(asctime)s - %(name)s - %(levelname)s - %(message)s"
    
    # Configure root logger
    logging.basicConfig(
        level=getattr(logging, level.upper()),
        format=format_string,
        handlers=[]  # Clear default handlers
    )
    
    logger = logging.getLogger()
    
    # Console handler
    console_handler = logging.StreamHandler(sys.stdout)
    console_handler.setFormatter(logging.Formatter(format_string))
    logger.addHandler(console_handler)
    
    # File handler if specified
    if log_file:
        log_path = Path(log_file)
        log_path.parent.mkdir(parents=True, exist_ok=True)
        
        file_handler = logging.FileHandler(log_path)
        file_handler.setFormatter(logging.Formatter(format_string))
        logger.addHandler(file_handler)
    
    return logger


class ProgressLogger:
    """Logger for tracking import progress."""
    
    def __init__(self, total: int, logger: Optional[logging.Logger] = None):
        self.total = total
        self.current = 0
        self.logger = logger or logging.getLogger(__name__)
        self.start_time = datetime.now()
    
    def update(self, increment: int = 1, message: Optional[str] = None) -> None:
        """Update progress."""
        self.current += increment
        percentage = (self.current / self.total * 100) if self.total > 0 else 0
        
        elapsed = (datetime.now() - self.start_time).total_seconds()
        rate = self.current / elapsed if elapsed > 0 else 0
        eta = (self.total - self.current) / rate if rate > 0 else 0
        
        log_message = f"Progress: {self.current}/{self.total} ({percentage:.1f}%)"
        if message:
            log_message += f" - {message}"
        log_message += f" - Rate: {rate:.1f}/s - ETA: {eta:.0f}s"
        
        self.logger.info(log_message)
    
    def complete(self) -> None:
        """Mark as complete."""
        elapsed = (datetime.now() - self.start_time).total_seconds()
        self.logger.info(
            f"Completed {self.total} items in {elapsed:.1f}s "
            f"({self.total/elapsed:.1f} items/s)"
        )