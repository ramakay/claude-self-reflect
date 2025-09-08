"""
Claude Self-Reflect Modular Import System
==========================================

A pristine, modular conversation import system following SOLID principles
and clean architecture patterns.

Version: 3.0.0
Author: Claude Self-Reflect Team
License: MIT
"""

from .core.config import ImportConfig
from .core.models import Message, ConversationChunk, ProcessedPoint
from .main import ConversationProcessor, ImporterContainer

__version__ = "3.0.0"
__all__ = [
    "ImportConfig",
    "Message", 
    "ConversationChunk",
    "ProcessedPoint",
    "ConversationProcessor",
    "ImporterContainer"
]