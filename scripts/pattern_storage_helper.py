"""
High-Quality Pattern Storage Helper
Extracted from metadata_extractor.py to reduce complexity.
Complexity target: <5 per function
"""

import os
import asyncio
import logging
from pathlib import Path
from typing import Dict, List, Any, Tuple
from datetime import datetime

logger = logging.getLogger(__name__)


class HighQualityPatternStore:
    """
    Helper class for storing high-quality patterns to Memory Tool.
    Complexity: 4 (setup, analysis, formatting, storage)
    """

    def __init__(self):
        """Initialize pattern store with dependencies."""
        self.memory_handler = None
        self.analyzer = None

    def store_patterns(self, high_quality_files: List[Tuple[str, Dict[str, Any]]]) -> List[Dict[str, Any]]:
        """
        Store high-quality patterns to Memory Tool.
        Complexity: 3 (setup, iterate, delegate)
        """
        # Late imports to avoid circular dependencies
        self._setup_dependencies()

        memory_references = []

        for file_path, quality_info in high_quality_files:
            try:
                ref = self._store_single_file(file_path, quality_info)
                if ref:
                    memory_references.append(ref)
            except Exception as e:
                logger.debug(f"Could not auto-store patterns for {file_path}: {e}")
                continue

        return memory_references

    def _setup_dependencies(self):
        """
        Setup Memory Tool handler and analyzer.
        Complexity: 2 (two imports)
        """
        if self.memory_handler is None:
            import sys
            sys.path.insert(0, str(Path(__file__).parent.parent / 'mcp-server' / 'src'))
            from memory_tools import get_memory_handler
            self.memory_handler = get_memory_handler()

        if self.analyzer is None:
            from ast_grep_final_analyzer import FinalASTGrepAnalyzer
            self.analyzer = FinalASTGrepAnalyzer()

    def _store_single_file(
        self,
        file_path: str,
        quality_info: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        Store patterns from a single file.
        Complexity: 4 (analyze, extract, format, store)
        """
        # Expand and validate path
        expanded_path = os.path.expanduser(file_path) if file_path.startswith('~') else file_path
        if not os.path.exists(expanded_path):
            return None

        # Analyze file
        result = self.analyzer.analyze_file(expanded_path)

        # Extract good patterns
        good_matches = [
            m for m in result.get('all_matches', [])
            if m.get('quality') == 'good'
        ]

        if not good_matches:
            return None

        # Format content
        content = self._format_pattern_content(file_path, quality_info, good_matches, result)

        # Store to Memory Tool
        return self._save_to_memory_tool(file_path, quality_info, content, len(good_matches))

    def _format_pattern_content(
        self,
        file_path: str,
        quality_info: Dict[str, Any],
        good_matches: List[Dict[str, Any]],
        result: Dict[str, Any]
    ) -> str:
        """
        Format patterns into markdown content.
        Complexity: 3 (build header, format patterns, add recommendations)
        """
        file_name = Path(file_path).name
        timestamp = datetime.now().strftime('%Y-%m-%d')

        # Build header
        content_lines = [
            f"# High-Quality Patterns: {file_name}",
            f"**Date**: {timestamp}",
            f"**Quality Score**: {quality_info['score']:.2f}",
            f"**File**: `{file_path}`\n",
            "## Patterns Found\n"
        ]

        # Format patterns by category
        by_category = self._group_by_category(good_matches[:10])

        for category, matches in by_category.items():
            content_lines.append(f"### {category.title()}")
            for match in matches:
                content_lines.append(f"- **{match.get('description', 'Pattern')}**")
                if match.get('locations'):
                    first_loc = match['locations'][0]
                    content_lines.append(f"  - Line {first_loc['line']}: `{first_loc['text']}`")
            content_lines.append("")

        # Add recommendations
        if result.get('recommendations'):
            content_lines.append("## Recommendations")
            for rec in result['recommendations'][:3]:
                content_lines.append(f"- {rec}")

        return "\n".join(content_lines)

    @staticmethod
    def _group_by_category(matches: List[Dict[str, Any]]) -> Dict[str, List[Dict[str, Any]]]:
        """
        Group patterns by category.
        Complexity: 2 (iterate and group)
        """
        by_category = {}
        for match in matches:
            category = match.get('category', 'general')
            if category not in by_category:
                by_category[category] = []
            by_category[category].append(match)
        return by_category

    def _save_to_memory_tool(
        self,
        file_path: str,
        quality_info: Dict[str, Any],
        content: str,
        pattern_count: int
    ) -> Dict[str, Any]:
        """
        Save content to Memory Tool using asyncio with proper cleanup.
        Complexity: 3 (create loop, store, cleanup)
        """
        file_name = Path(file_path).name
        timestamp = datetime.now().strftime('%Y-%m-%d')
        memory_path = f"quality/{file_name.replace('.', '_')}_{timestamp}.md"

        # Create dedicated event loop with proper cleanup (CodeRabbit fix)
        loop = asyncio.new_event_loop()
        asyncio.set_event_loop(loop)

        try:
            result_msg = loop.run_until_complete(
                self.memory_handler.create(memory_path, content)
            )

            if "✅" in result_msg:
                logger.info(f"Auto-stored high-quality patterns from {file_name} to {memory_path}")
                return {
                    "file": file_path,
                    "memory_path": memory_path,
                    "quality_score": quality_info['score'],
                    "pattern_count": pattern_count,
                    "timestamp": timestamp
                }

            return None
        finally:
            # Always close loop to prevent resource leaks
            loop.close()
