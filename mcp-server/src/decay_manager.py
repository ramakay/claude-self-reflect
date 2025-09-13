"""Decay calculation manager for Claude Self-Reflect MCP server."""

import math
from datetime import datetime, timezone
from typing import List, Tuple, Optional
try:
    from .config import (
        USE_DECAY,
        DECAY_SCALE_DAYS,
        DECAY_WEIGHT,
        USE_NATIVE_DECAY,
        logger
    )
except ImportError:
    # Fallback for direct execution
    import os
    import logging
    USE_DECAY = os.getenv('USE_DECAY', 'false').lower() == 'true'
    DECAY_SCALE_DAYS = float(os.getenv('DECAY_SCALE_DAYS', '90'))
    DECAY_WEIGHT = float(os.getenv('DECAY_WEIGHT', '0.3'))
    USE_NATIVE_DECAY = os.getenv('USE_NATIVE_DECAY', 'false').lower() == 'true'
    logger = logging.getLogger(__name__)

class DecayManager:
    """Manages memory decay calculations for search results."""
    
    def __init__(self):
        self.scale_ms = DECAY_SCALE_DAYS * 24 * 60 * 60 * 1000
        self.weight = DECAY_WEIGHT
        self.use_decay = USE_DECAY
        self.use_native = USE_NATIVE_DECAY
    
    def calculate_decay_score(
        self, 
        base_score: float, 
        timestamp: str
    ) -> float:
        """Calculate decayed score for a single result."""
        if not self.use_decay:
            return base_score
        
        try:
            # Parse timestamp
            if timestamp.endswith('Z'):
                timestamp = timestamp.replace('Z', '+00:00')
            
            result_time = datetime.fromisoformat(timestamp)
            if result_time.tzinfo is None:
                result_time = result_time.replace(tzinfo=timezone.utc)
            
            # Calculate age
            now = datetime.now(timezone.utc)
            age_ms = (now - result_time).total_seconds() * 1000
            
            # Calculate decay factor using half-life formula
            # decay = exp(-ln(2) * age / half_life)
            decay_factor = math.exp(-0.693147 * age_ms / self.scale_ms)
            
            # Apply decay with weight
            final_score = base_score * (1 - self.weight) + base_score * self.weight * decay_factor
            
            return final_score
            
        except Exception as e:
            logger.error(f"Failed to calculate decay: {e}")
            return base_score
    
    def apply_decay_to_results(
        self, 
        results: List[Tuple[float, str, dict]]
    ) -> List[Tuple[float, str, dict]]:
        """Apply decay to a list of results and re-sort."""
        if not self.use_decay:
            return results
        
        decayed_results = []
        for score, id_str, payload in results:
            timestamp = payload.get('timestamp', datetime.now().isoformat())
            decayed_score = self.calculate_decay_score(score, timestamp)
            decayed_results.append((decayed_score, id_str, payload))
        
        # Re-sort by decayed score
        decayed_results.sort(key=lambda x: x[0], reverse=True)
        
        return decayed_results
    
    def get_native_decay_config(self) -> Optional[dict]:
        """Get configuration for native Qdrant decay."""
        if not self.use_native:
            return None
        
        return {
            'scale_seconds': self.scale_ms / 1000,
            'weight': self.weight,
            'midpoint': 0.5  # Half-life semantics
        }
    
    def should_use_decay(self, explicit_setting: Optional[int] = None) -> bool:
        """Determine if decay should be used for a query."""
        if explicit_setting is not None:
            if explicit_setting == 1:
                return True
            elif explicit_setting == 0:
                return False
        
        return self.use_decay