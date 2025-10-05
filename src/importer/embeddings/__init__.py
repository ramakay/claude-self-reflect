"""Embedding providers for text vectorization."""

from .base import EmbeddingProvider
from .fastembed_provider import FastEmbedProvider
from .validator import EmbeddingValidator

# Conditional import for Voyage
try:
    from .voyage_provider import VoyageEmbeddingProvider
    __all__ = [
        "EmbeddingProvider",
        "FastEmbedProvider",
        "VoyageEmbeddingProvider",
        "EmbeddingValidator"
    ]
except ImportError:
    # Voyage not available, continue without it
    __all__ = [
        "EmbeddingProvider",
        "FastEmbedProvider",
        "EmbeddingValidator"
    ]