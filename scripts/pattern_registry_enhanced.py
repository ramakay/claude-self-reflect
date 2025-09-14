#!/usr/bin/env python3
"""
Enhanced Pattern Registry that combines AST-GREP patterns with value-based metadata.
This provides rich metadata for tracking code quality evolution.
"""

from typing import Dict, List, Any, Set
import re
import logging
from pathlib import Path
import sys

# Add parent for existing imports
sys.path.append(str(Path(__file__).parent))
from pattern_registry import PatternRegistry

logger = logging.getLogger(__name__)

class EnhancedPatternRegistry(PatternRegistry):
    """Enhanced pattern registry with quality scoring and evolution tracking."""

    def __init__(self):
        super().__init__()
        # Add quality scoring patterns
        self.quality_patterns = self._load_quality_patterns()

    def _load_quality_patterns(self) -> Dict[str, Dict]:
        """Load patterns that indicate code quality."""
        return {
            "high_quality": {
                "patterns": [
                    "async-context-manager", "parallel-execution",
                    "specific-exception", "type-hints", "logging-usage",
                    "transaction-context", "caching-decorator"
                ],
                "weight": 1.5  # High quality patterns worth more
            },
            "standard": {
                "patterns": [
                    "useState-hook", "useEffect-hook", "rest-endpoint",
                    "query-builder", "mock-usage"
                ],
                "weight": 1.0
            },
            "anti_patterns": {
                "patterns": [
                    "broad-except", "print-statements", "hardcoded-values",
                    "sync-in-async", "no-error-handling"
                ],
                "weight": -2.0  # Anti-patterns hurt score more
            },
            "critical_issues": {
                "patterns": [
                    "sync-in-async", "no-error-handling"
                ],
                "weight": -5.0  # Critical issues severely impact score
            }
        }

    def calculate_quality_score(self, patterns: Dict[str, List[str]]) -> float:
        """
        Calculate a quality score based on patterns found.
        Score range: 0.0 (worst) to 1.0 (best)
        """
        score = 50.0  # Start at neutral
        total_patterns = 0

        # Flatten patterns
        all_patterns = []
        for category, pattern_list in patterns.items():
            for pattern in pattern_list:
                # Extract just the pattern ID
                if '.' in pattern:
                    pattern_id = pattern.split('.')[-1]
                else:
                    pattern_id = pattern
                all_patterns.append(pattern_id)
                total_patterns += 1

        # Apply quality scoring
        for quality_type, config in self.quality_patterns.items():
            for pattern_id in all_patterns:
                if pattern_id in config["patterns"]:
                    score += config["weight"]

        # Normalize to 0-1 range
        score = max(0, min(100, score)) / 100
        return score

    def extract_enhanced_metadata(self, text: str) -> Dict[str, Any]:
        """
        Extract patterns and calculate enhanced metadata including quality score.
        """
        # Get base patterns
        patterns = self.extract_patterns(text)
        categories = self.categorize_patterns(patterns)

        # Calculate quality score
        quality_score = self.calculate_quality_score(patterns)

        # Count anti-patterns
        anti_pattern_count = 0
        critical_issue_count = 0

        all_pattern_ids = []
        for pattern_list in patterns.values():
            for pattern in pattern_list:
                pattern_id = pattern.split('.')[-1] if '.' in pattern else pattern
                all_pattern_ids.append(pattern_id)

                if pattern_id in self.quality_patterns["anti_patterns"]["patterns"]:
                    anti_pattern_count += 1
                if pattern_id in self.quality_patterns["critical_issues"]["patterns"]:
                    critical_issue_count += 1

        # Determine code quality level
        if quality_score >= 0.8:
            quality_level = "excellent"
        elif quality_score >= 0.6:
            quality_level = "good"
        elif quality_score >= 0.4:
            quality_level = "fair"
        elif quality_score >= 0.2:
            quality_level = "poor"
        else:
            quality_level = "critical"

        return {
            "patterns": list(set([p for plist in patterns.values() for p in plist]))[:50],
            "pattern_categories": list(categories.keys()),
            "has_patterns": len(patterns) > 0,
            "quality_score": round(quality_score, 3),
            "quality_level": quality_level,
            "anti_pattern_count": anti_pattern_count,
            "critical_issues": critical_issue_count,
            "pattern_summary": {
                "total": len(all_pattern_ids),
                "unique": len(set(all_pattern_ids)),
                "by_category": {k: len(v) for k, v in patterns.items()}
            }
        }

    def track_evolution(self, file_path: str, patterns_history: List[Dict]) -> Dict[str, Any]:
        """
        Track pattern evolution for a file over time.
        """
        if not patterns_history:
            return {"trend": "unknown", "improvement": 0}

        # Sort by timestamp
        sorted_history = sorted(patterns_history, key=lambda x: x.get('timestamp', ''))

        if len(sorted_history) < 2:
            return {"trend": "insufficient_data", "improvement": 0}

        # Compare oldest to newest
        oldest = sorted_history[0]
        newest = sorted_history[-1]

        old_score = oldest.get('quality_score', 0.5)
        new_score = newest.get('quality_score', 0.5)

        improvement = new_score - old_score

        if improvement > 0.1:
            trend = "improving"
        elif improvement < -0.1:
            trend = "degrading"
        else:
            trend = "stable"

        # Track specific improvements
        old_anti = oldest.get('anti_pattern_count', 0)
        new_anti = newest.get('anti_pattern_count', 0)

        old_critical = oldest.get('critical_issues', 0)
        new_critical = newest.get('critical_issues', 0)

        return {
            "trend": trend,
            "improvement": round(improvement, 3),
            "quality_change": f"{old_score:.1%} → {new_score:.1%}",
            "anti_patterns_change": new_anti - old_anti,
            "critical_issues_change": new_critical - old_critical,
            "samples_analyzed": len(sorted_history)
        }


# Singleton instance
_enhanced_registry = None

def get_enhanced_registry() -> EnhancedPatternRegistry:
    """Get or create the singleton enhanced registry."""
    global _enhanced_registry
    if _enhanced_registry is None:
        _enhanced_registry = EnhancedPatternRegistry()
    return _enhanced_registry


def extract_enhanced_patterns(text: str) -> Dict[str, Any]:
    """
    Main entry point for enhanced pattern extraction with quality scoring.
    """
    registry = get_enhanced_registry()
    return registry.extract_enhanced_metadata(text)


if __name__ == "__main__":
    # Test the enhanced extraction
    test_code = """
    async def process_data():
        async with pool.get_client() as client:
            try:
                results = await asyncio.gather(
                    fetch_user(client),
                    fetch_posts(client)
                )
            except TimeoutError:
                logger.error("Timeout occurred")
                return cached_data
            except Exception:  # Anti-pattern: broad except
                logger.info("Error occurred")  # Anti-pattern: print statement

            async for item in results:
                yield process_item(item)
    """

    metadata = extract_enhanced_patterns(test_code)
    logger.info("Enhanced Metadata Extraction:")
    logger.info(f"  Quality Score: {metadata['quality_score']:.1%}")
    logger.info(f"  Quality Level: {metadata['quality_level']}")
    logger.info(f"  Anti-patterns: {metadata['anti_pattern_count']}")
    logger.info(f"  Critical Issues: {metadata['critical_issues']}")
    logger.info(f"  Categories: {metadata['pattern_categories']}")
    logger.info(f"  Pattern Summary: {metadata['pattern_summary']}")