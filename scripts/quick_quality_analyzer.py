#!/usr/bin/env python3
"""
Lightweight quality analyzer for integration into importers and watchers.
Uses AST-GREP patterns to detect code quality issues in real-time.
"""

import json
import subprocess
import time
from pathlib import Path
from typing import Dict, List, Optional, Tuple
import logging

logger = logging.getLogger(__name__)

class QuickQualityAnalyzer:
    """Lightweight quality analyzer for real-time analysis."""

    def __init__(self):
        self.registry_path = Path(__file__).parent / "unified_registry.json"
        self.patterns = self._load_patterns()

    def _load_patterns(self) -> Dict:
        """Load AST-GREP patterns from registry."""
        try:
            with open(self.registry_path, 'r') as f:
                data = json.load(f)
                return data.get('patterns', {})
        except Exception as e:
            logger.error(f"Failed to load patterns: {e}")
            return {}

    def analyze_file(self, file_path: Path) -> Dict[str, any]:
        """
        Analyze a single file for quality issues.
        Returns lightweight metadata for Qdrant storage.
        """
        if not file_path.exists():
            return {}

        # Determine language from extension
        ext_to_lang = {
            '.py': 'python',
            '.ts': 'typescript',
            '.tsx': 'typescript',
            '.js': 'javascript',
            '.jsx': 'javascript'
        }

        ext = file_path.suffix.lower()
        language = ext_to_lang.get(ext)

        if not language:
            return {}

        # Run quick analysis with key patterns only
        critical_patterns = self._get_critical_patterns(language)
        issues = self._run_ast_grep(file_path, critical_patterns, language)

        # Calculate simple quality score
        score = self._calculate_simple_score(issues)

        return {
            'quality_score': score,
            'critical_issues': len([i for i in issues if i.get('severity') == 'high']),
            'total_issues': len(issues),
            'language': language,
            'analyzed_at': Path(file_path).stat().st_mtime
        }

    def _get_critical_patterns(self, language: str) -> List[Dict]:
        """Get only critical patterns for quick analysis."""
        critical_categories = [
            f"{language}_complexity",  # Our new complexity patterns
            f"{language}_antipatterns",
            f"{language}_error_handling"
        ]

        patterns = []
        for category in critical_categories:
            if category in self.patterns:
                # Only take high-severity patterns for speed
                for pattern in self.patterns[category]:
                    if pattern.get('weight', 0) <= -3:  # High severity
                        patterns.append(pattern)

        return patterns[:10]  # Limit to 10 patterns for speed

    def _run_ast_grep(self, file_path: Path, patterns: List[Dict], language: str) -> List[Dict]:
        """Run AST-GREP on file with patterns."""
        issues = []

        for pattern in patterns:
            try:
                # Use ast-grep-py if available, otherwise fall back to CLI
                pattern_str = pattern.get('pattern', '')
                if not pattern_str:
                    continue

                # Simple CLI approach for speed
                cmd = [
                    'ast-grep',
                    'scan',
                    '--pattern', pattern_str,
                    '--lang', language,
                    str(file_path)
                ]

                result = subprocess.run(
                    cmd,
                    capture_output=True,
                    text=True,
                    timeout=1  # 1 second timeout per pattern
                )

                if result.returncode == 0 and result.stdout:
                    # Found matches
                    match_count = result.stdout.count('\n')
                    if match_count > 0:
                        issues.append({
                            'pattern_id': pattern.get('id'),
                            'severity': pattern.get('severity', 'medium'),
                            'count': match_count,
                            'weight': pattern.get('weight', -1)
                        })

            except subprocess.TimeoutExpired:
                continue  # Skip slow patterns
            except Exception as e:
                logger.debug(f"Pattern failed: {e}")
                continue

        return issues

    def _calculate_simple_score(self, issues: List[Dict]) -> float:
        """Calculate simple quality score (0-100)."""
        if not issues:
            return 100.0

        # Simple penalty-based scoring
        penalty = 0
        for issue in issues:
            weight = abs(issue.get('weight', 1))
            count = issue.get('count', 1)
            penalty += weight * count

        # Convert to 0-100 scale
        score = max(0, 100 - (penalty * 2))
        return round(score, 1)

    def get_project_quality(self, project_path: Path) -> Dict[str, any]:
        """
        Get overall project quality metadata.
        Used for adding to conversation chunks.
        """
        try:
            # Check if we have cached quality data
            cache_file = Path.home() / ".claude-self-reflect" / "quality_cache.json"
            if cache_file.exists():
                with open(cache_file, 'r') as f:
                    cache = json.load(f)
                    project_key = str(project_path.resolve())
                    if project_key in cache:
                        cached = cache[project_key]
                        # Use cache if less than 1 hour old
                        if cached.get('timestamp', 0) > time.time() - 3600:
                            return cached.get('metadata', {})

            # Otherwise return minimal metadata
            return {
                'project_quality': 'not_analyzed',
                'quality_score': None
            }

        except Exception as e:
            logger.debug(f"Failed to get project quality: {e}")
            return {}


# Singleton instance for importers
_analyzer = None

def get_analyzer() -> QuickQualityAnalyzer:
    """Get or create singleton analyzer instance."""
    global _analyzer
    if _analyzer is None:
        _analyzer = QuickQualityAnalyzer()
    return _analyzer


def analyze_for_metadata(file_path: Path) -> Dict[str, any]:
    """
    Quick function for importers to get quality metadata.
    Returns dict suitable for Qdrant point metadata.
    """
    analyzer = get_analyzer()
    return analyzer.analyze_file(file_path)


if __name__ == "__main__":
    # Test on a file
    import sys
    if len(sys.argv) > 1:
        file_path = Path(sys.argv[1])
        analyzer = QuickQualityAnalyzer()
        result = analyzer.analyze_file(file_path)
        print(json.dumps(result, indent=2))
    else:
        print("Usage: python quick_quality_analyzer.py <file_path>")