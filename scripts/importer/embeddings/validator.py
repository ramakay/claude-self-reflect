"""Embedding validation utilities."""

import statistics
from typing import List, Tuple, Optional
import logging

logger = logging.getLogger(__name__)


class EmbeddingValidator:
    """
    Comprehensive embedding validation.
    
    Performs multiple checks to ensure embedding quality.
    """
    
    def __init__(
        self,
        expected_dimension: int,
        min_variance: float = 1e-6,
        max_magnitude: float = 100.0
    ):
        self.expected_dimension = expected_dimension
        self.min_variance = min_variance
        self.max_magnitude = max_magnitude
    
    def validate(self, embedding: List[float]) -> Tuple[bool, Optional[str]]:
        """
        Validate an embedding vector.
        
        Returns:
            Tuple of (is_valid, error_message)
        """
        # Check empty
        if not embedding:
            return False, "Empty embedding"
        
        # Check dimension
        if len(embedding) != self.expected_dimension:
            return False, f"Dimension mismatch: expected {self.expected_dimension}, got {len(embedding)}"
        
        # Check for NaN/Inf
        for i, val in enumerate(embedding):
            if not isinstance(val, (int, float)):
                return False, f"Non-numeric value at index {i}: {type(val)}"
            if val != val:  # NaN check
                return False, f"NaN value at index {i}"
            if abs(val) == float('inf'):
                return False, f"Infinite value at index {i}"
        
        # Check for degenerate (all same)
        unique_count = len(set(embedding))
        if unique_count == 1:
            return False, f"Degenerate embedding (all values are {embedding[0]})"
        
        # Check variance
        if len(embedding) > 1:
            try:
                variance = statistics.variance(embedding)
                if variance < self.min_variance:
                    # Warning, not error
                    logger.warning(f"Low variance: {variance}")
            except Exception as e:
                logger.warning(f"Could not calculate variance: {e}")
        
        # Check magnitude
        max_val = max(abs(v) for v in embedding)
        if max_val > self.max_magnitude:
            return False, f"Value exceeds maximum magnitude: {max_val}"
        
        # Check for mostly zeros
        zero_count = sum(1 for v in embedding if abs(v) < 1e-10)
        if zero_count > len(embedding) * 0.9:
            return False, f"Embedding is mostly zeros ({zero_count}/{len(embedding)})"
        
        return True, None
    
    def validate_batch(
        self, 
        embeddings: List[List[float]]
    ) -> List[Tuple[int, str]]:
        """
        Validate a batch of embeddings.
        
        Returns:
            List of (index, error_message) for invalid embeddings
        """
        errors = []
        for i, embedding in enumerate(embeddings):
            valid, error = self.validate(embedding)
            if not valid:
                errors.append((i, error))
        return errors
    
    def check_similarity(
        self,
        embeddings: List[List[float]]
    ) -> bool:
        """
        Check if embeddings in a batch are too similar.
        
        This can indicate a problem with the embedding model.
        """
        if len(embeddings) < 2:
            return True
        
        # Calculate pairwise cosine similarities
        from math import sqrt
        
        def cosine_similarity(a: List[float], b: List[float]) -> float:
            dot_product = sum(x * y for x, y in zip(a, b))
            norm_a = sqrt(sum(x * x for x in a))
            norm_b = sqrt(sum(y * y for y in b))
            if norm_a == 0 or norm_b == 0:
                return 0
            return dot_product / (norm_a * norm_b)
        
        # Check if all embeddings are too similar
        high_similarity_count = 0
        total_pairs = 0
        
        for i in range(len(embeddings)):
            for j in range(i + 1, min(i + 5, len(embeddings))):  # Check first 5 pairs
                similarity = cosine_similarity(embeddings[i], embeddings[j])
                if similarity > 0.99:  # Nearly identical
                    high_similarity_count += 1
                total_pairs += 1
        
        if total_pairs > 0 and high_similarity_count / total_pairs > 0.8:
            logger.warning(
                f"High similarity detected: {high_similarity_count}/{total_pairs} "
                f"pairs have >0.99 similarity"
            )
            return False
        
        return True