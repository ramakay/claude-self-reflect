#!/usr/bin/env python3
"""
Analyze conversation quality from JSONL files.
Simple heuristic-based quality analysis when we can't analyze actual source code.
"""

import json
import re
from pathlib import Path
from typing import Dict, List, Any
from datetime import datetime
import logging

logger = logging.getLogger(__name__)

class ConversationQualityAnalyzer:
    """Analyze code quality patterns in conversations."""

    def analyze_conversation_file(self, jsonl_path: Path) -> Dict[str, Any]:
        """Analyze a conversation JSONL file for quality patterns."""

        quality_indicators = {
            'good_patterns': 0,
            'bad_patterns': 0,
            'tests_written': 0,
            'documentation_added': 0,
            'refactoring_done': 0,
            'errors_fixed': 0,
            'security_issues_addressed': 0,
            'performance_improvements': 0,
            'code_reviews': 0,
            'todos_completed': 0
        }

        # Pattern detection
        good_patterns = [
            (r'test[s]?\s+(pass|succeed|complete)', 'tests_passing'),
            (r'fix(ed|ing)?\s+bug', 'bug_fixes'),
            (r'add(ed|ing)?\s+(test|spec)', 'tests_written'),
            (r'refactor(ed|ing)?', 'refactoring_done'),
            (r'document(ed|ing|ation)?', 'documentation_added'),
            (r'security\s+fix', 'security_issues_addressed'),
            (r'performance\s+improv', 'performance_improvements'),
            (r'code\s+review', 'code_reviews'),
            (r'todo.*complet', 'todos_completed'),
            (r'type.*check.*pass', 'type_safety'),
            (r'lint.*pass|no.*lint.*error', 'linting_clean')
        ]

        bad_patterns = [
            (r'test[s]?\s+fail', 'failing_tests'),
            (r'error|exception|traceback', 'errors'),
            (r'todo|fixme|hack', 'technical_debt'),
            (r'console\.log|print\(', 'debug_statements'),
            (r'any\s+type|typing\.Any', 'weak_typing'),
            (r'disable.*lint|noqa|type:\s*ignore', 'ignored_warnings'),
            (r'sleep\(|time\.sleep', 'blocking_calls'),
            (r'broad\s+except|except\s*:', 'broad_exceptions')
        ]

        try:
            with open(jsonl_path, 'r') as f:
                for line in f:
                    try:
                        entry = json.loads(line.strip())

                        # Analyze messages
                        if 'messages' in entry:
                            for msg in entry['messages']:
                                content = str(msg.get('content', ''))
                                content_lower = content.lower()

                                # Check good patterns
                                for pattern, name in good_patterns:
                                    if re.search(pattern, content_lower):
                                        quality_indicators['good_patterns'] += 1

                                # Check bad patterns
                                for pattern, name in bad_patterns:
                                    if re.search(pattern, content_lower):
                                        quality_indicators['bad_patterns'] += 1

                                # Check for tool usage (indicates active development)
                                if msg.get('role') == 'assistant' and isinstance(msg.get('content'), list):
                                    for item in msg['content']:
                                        if isinstance(item, dict) and item.get('type') == 'tool_use':
                                            tool_name = item.get('name', '')
                                            if tool_name in ['Write', 'Edit', 'MultiEdit']:
                                                quality_indicators['good_patterns'] += 2
                                            elif tool_name == 'TodoWrite':
                                                quality_indicators['todos_completed'] += 1
                                            elif tool_name in ['Bash'] and 'test' in str(item.get('input', {})):
                                                quality_indicators['tests_written'] += 1

                    except json.JSONDecodeError:
                        continue
                    except Exception as e:
                        logger.debug(f"Error processing line: {e}")

        except Exception as e:
            logger.error(f"Error reading file {jsonl_path}: {e}")
            return self._default_quality()

        # Calculate score
        total_patterns = quality_indicators['good_patterns'] + quality_indicators['bad_patterns']
        if total_patterns == 0:
            # No patterns found, return neutral score
            return {
                'quality_score': 0.7,
                'quality_grade': 'B',
                'total_issues': 0,
                'good_patterns_found': 0,
                'indicators': quality_indicators
            }

        quality_score = quality_indicators['good_patterns'] / total_patterns

        # Determine grade
        if quality_score >= 0.9:
            grade = 'A+'
        elif quality_score >= 0.8:
            grade = 'A'
        elif quality_score >= 0.7:
            grade = 'B'
        elif quality_score >= 0.6:
            grade = 'C'
        elif quality_score >= 0.5:
            grade = 'D'
        else:
            grade = 'F'

        return {
            'quality_score': quality_score,
            'quality_grade': grade,
            'total_issues': quality_indicators['bad_patterns'],
            'good_patterns_found': quality_indicators['good_patterns'],
            'indicators': quality_indicators
        }

    def _default_quality(self) -> Dict[str, Any]:
        """Return default quality when analysis fails."""
        return {
            'quality_score': 0.5,
            'quality_grade': 'D',
            'total_issues': 0,
            'good_patterns_found': 0,
            'indicators': {}
        }

    def analyze_project(self, project_path: Path, limit: int = 5) -> Dict[str, Any]:
        """Analyze multiple conversations from a project."""

        # Find recent JSONL files
        jsonl_files = sorted(
            project_path.glob("*.jsonl"),
            key=lambda x: x.stat().st_mtime,
            reverse=True
        )[:limit]

        if not jsonl_files:
            return self._default_quality()

        all_scores = []
        total_issues = 0
        total_good = 0

        for jsonl_file in jsonl_files:
            result = self.analyze_conversation_file(jsonl_file)
            all_scores.append(result['quality_score'])
            total_issues += result['total_issues']
            total_good += result['good_patterns_found']

        avg_score = sum(all_scores) / len(all_scores) if all_scores else 0.5

        # Determine grade
        if avg_score >= 0.9:
            grade = 'A+'
        elif avg_score >= 0.8:
            grade = 'A'
        elif avg_score >= 0.7:
            grade = 'B'
        elif avg_score >= 0.6:
            grade = 'C'
        elif avg_score >= 0.5:
            grade = 'D'
        else:
            grade = 'F'

        return {
            'quality_score': avg_score,
            'quality_grade': grade,
            'total_issues': total_issues,
            'good_patterns_found': total_good,
            'files_analyzed': len(jsonl_files)
        }