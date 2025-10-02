"""
Metadata extractor using message processors to reduce complexity.
Refactored from extract_metadata_single_pass function.
"""

import json
import os
import logging
from pathlib import Path
from typing import Dict, Any, Tuple, Optional
from datetime import datetime

from message_processors import (
    MessageProcessorFactory,
    extract_concepts,
    MAX_CONCEPT_MESSAGES,
    MAX_FILES_ANALYZED,
    MAX_FILES_EDITED,
    MAX_TOOLS_USED,
    MAX_AST_ELEMENTS
)

logger = logging.getLogger(__name__)


class MetadataExtractor:
    """Extract metadata from JSONL conversation files."""

    def __init__(self):
        self.processor_factory = MessageProcessorFactory()

    def extract_metadata_from_file(self, file_path: str) -> Tuple[Dict[str, Any], str, int]:
        """
        Extract metadata from a JSONL file in a single pass.
        Returns: (metadata, first_timestamp, message_count)
        """
        metadata = self._initialize_metadata()
        first_timestamp = None
        message_count = 0
        all_text = []

        try:
            with open(file_path, 'r', encoding='utf-8') as f:
                for line in f:
                    if not line.strip():
                        continue

                    result = self._process_line(line, metadata)
                    if result:
                        text_content, is_message = result

                        # Update timestamp and counts
                        if first_timestamp is None:
                            first_timestamp = self._extract_timestamp(line)

                        if is_message:
                            message_count += 1

                        if text_content:
                            # Limit text accumulation to prevent memory issues
                            if len(all_text) < MAX_CONCEPT_MESSAGES:
                                all_text.append(text_content[:1000])

        except (IOError, OSError) as e:
            logger.warning(f"Error reading file {file_path}: {e}")
        except (json.JSONDecodeError, ValueError) as e:
            logger.warning(f"Error parsing JSON in {file_path}: {e}")
        except Exception as e:
            logger.error(f"Unexpected error extracting metadata from {file_path}: {e}")

        # Post-process collected data
        self._post_process_metadata(metadata, all_text, file_path)

        # Apply limits to arrays
        self._apply_metadata_limits(metadata)

        return metadata, first_timestamp or datetime.now().isoformat(), message_count

    def _initialize_metadata(self) -> Dict[str, Any]:
        """Initialize empty metadata structure with Memory Tool integration."""
        return {
            # Core file tracking
            "files_analyzed": [],
            "files_edited": [],
            "tools_used": [],
            "concepts": [],
            "ast_elements": [],
            "has_code_blocks": False,
            "total_messages": 0,
            "project_path": None,

            # AST-GREP quality analysis
            "pattern_analysis": {},
            "avg_quality_score": 0.0,

            # Memory Tool integration (v6.0)
            "memory_references": [],  # Paths to /memories/ files with stored patterns
            "quality_evolution": [],  # Track quality changes over time
            "pattern_frequency": {},  # Track recurring patterns across conversations
            "context_importance": 0.0  # Calculated importance score for context clearing
        }

    def _process_line(self, line: str, metadata: Dict[str, Any]) -> Optional[Tuple[str, bool]]:
        """
        Process a single line from the JSONL file.
        Returns: (text_content, is_message) or None
        """
        try:
            data = json.loads(line)

            # Extract project path from cwd
            if metadata["project_path"] is None and 'cwd' in data:
                metadata["project_path"] = data.get('cwd')

            # Handle message entries
            if 'message' in data and data['message']:
                return self._process_message_entry(data['message'], metadata)

            # Handle top-level tool entries
            entry_type = data.get('type')
            if entry_type in ('tool_result', 'tool_use'):
                return self._process_tool_entry(data, metadata)

        except json.JSONDecodeError:
            # Expected for non-JSON lines, skip silently
            pass
        except (KeyError, TypeError, ValueError) as e:
            # Log specific parsing errors for debugging
            logger.debug(f"Error parsing line: {e}")

        return None

    def _process_message_entry(self, message: Dict[str, Any], metadata: Dict[str, Any]) -> Optional[Tuple[str, bool]]:
        """Process a message entry."""
        role = message.get('role')
        content = message.get('content')

        if not role or not content:
            return None

        # Check if it's a countable message
        is_user_or_assistant = role in ['user', 'assistant']

        # Process content
        text_content = self.processor_factory.process_content(content, metadata)

        return text_content, is_user_or_assistant

    def _process_tool_entry(self, data: Dict[str, Any], metadata: Dict[str, Any]) -> Optional[Tuple[str, bool]]:
        """Process a top-level tool entry."""
        entry_type = data.get('type')
        text_parts = []

        if entry_type == 'tool_use':
            tool_name = data.get('name', 'unknown')
            tool_input = str(data.get('input', ''))[:500]
            text_parts.append(f"[Tool: {tool_name}] {tool_input}")

            # Track tool usage
            if tool_name and tool_name not in metadata['tools_used']:
                metadata['tools_used'].append(tool_name)

        elif entry_type == 'tool_result':
            result_content = self._extract_tool_result_content(data)
            text_parts.append(f"[Result] {result_content[:1000]}")

        content = "\n".join(text_parts)
        # Tool entries should not count as messages (only user/assistant messages count)
        return (content, False) if content else None

    def _extract_tool_result_content(self, data: Dict[str, Any]) -> str:
        """Extract content from tool result data."""
        result_content = data.get('content')

        if isinstance(result_content, list):
            flat = []
            for item in result_content:
                if isinstance(item, dict) and item.get('type') == 'text':
                    flat.append(item.get('text', ''))
                elif isinstance(item, str):
                    flat.append(item)
            result_content = "\n".join(flat)

        if not result_content:
            result_content = data.get('result', '')

        return str(result_content)

    def _extract_timestamp(self, line: str) -> Optional[str]:
        """Extract timestamp from a line if present."""
        try:
            data = json.loads(line)
            return data.get('timestamp')
        except (json.JSONDecodeError, TypeError) as e:
            logger.debug(f"Failed to extract timestamp: {e}")
            return None

    def _post_process_metadata(self, metadata: Dict[str, Any], all_text: list, file_path: str):
        """Post-process collected metadata."""
        # Extract concepts from collected text
        if all_text:
            combined_text = ' '.join(all_text[:MAX_CONCEPT_MESSAGES])
            metadata['concepts'] = extract_concepts(combined_text)

        # Run AST-GREP pattern analysis if available
        self._run_pattern_analysis(metadata)

    def _run_pattern_analysis(self, metadata: Dict[str, Any]):
        """Run AST-GREP pattern analysis on mentioned files."""
        pattern_quality = {}
        avg_quality_score = 0.0

        try:
            # Update patterns first
            from update_patterns import check_and_update_patterns
            check_and_update_patterns()

            # Import analyzer
            from ast_grep_final_analyzer import FinalASTGrepAnalyzer
            analyzer = FinalASTGrepAnalyzer()

            # Analyze files
            files_to_analyze = list(set(
                metadata['files_edited'] + metadata['files_analyzed'][:10]
            ))
            quality_scores = []

            for file_path in files_to_analyze:
                # Expand file path for proper checking
                expanded_path = os.path.expanduser(file_path) if file_path.startswith('~') else file_path
                if self._is_code_file(expanded_path) and os.path.exists(expanded_path):
                    try:
                        result = analyzer.analyze_file(expanded_path)
                        metrics = result['quality_metrics']
                        pattern_quality[file_path] = {
                            'score': metrics['quality_score'],
                            'good_patterns': metrics['good_patterns_found'],
                            'bad_patterns': metrics['bad_patterns_found'],
                            'issues': metrics['total_issues']
                        }
                        quality_scores.append(metrics['quality_score'])
                    except (IOError, OSError) as e:
                        logger.debug(f"Could not read file {file_path}: {e}")
                    except (KeyError, ValueError) as e:
                        logger.debug(f"Error parsing AST results for {file_path}: {e}")
                    except Exception as e:
                        logger.warning(f"Unexpected error analyzing {file_path}: {e}")

            # Calculate average quality
            if quality_scores:
                avg_quality_score = sum(quality_scores) / len(quality_scores)

        except Exception as e:
            logger.debug(f"AST analysis not available: {e}")

        metadata['pattern_analysis'] = pattern_quality
        metadata['avg_quality_score'] = round(avg_quality_score, 3)

        # Auto-store high-quality patterns to Memory Tool
        memory_refs = self._auto_store_high_quality_patterns(pattern_quality, files_to_analyze)
        if memory_refs:
            metadata['memory_references'] = memory_refs

        # Track quality evolution
        if quality_scores:
            metadata['quality_evolution'] = [{
                'timestamp': datetime.now().isoformat(),
                'avg_score': avg_quality_score,
                'file_count': len(quality_scores),
                'scores': quality_scores[:10]  # Track top 10 scores
            }]

        # Track pattern frequency (CodeRabbit fix - implement pattern counting)
        metadata['pattern_frequency'] = self._count_pattern_frequency(pattern_quality)

        # Calculate context importance for Context Editing API
        metadata['context_importance'] = self._calculate_context_importance(
            avg_quality_score, len(metadata.get('memory_references', [])), len(metadata.get('concepts', []))
        )

    def _auto_store_high_quality_patterns(
        self,
        pattern_quality: Dict[str, Dict[str, Any]],
        files_analyzed: list
    ) -> list:
        """
        Auto-store high-quality patterns (score >90) to Memory Tool.
        Complexity: 3 (filter, delegate to helper, error handling)
        """
        memory_references = []

        try:
            # Filter high-quality files
            high_quality_files = [
                (file, info) for file, info in pattern_quality.items()
                if info.get('score', 0) > 0.90
            ]

            if not high_quality_files:
                return memory_references

            # Delegate to helper class
            from .pattern_storage_helper import HighQualityPatternStore
            store = HighQualityPatternStore()

            memory_references = store.store_patterns(high_quality_files)

        except Exception as e:
            logger.debug(f"Memory Tool auto-storage not available: {e}")

        return memory_references

    def _count_pattern_frequency(self, pattern_quality: Dict[str, Dict[str, Any]]) -> Dict[str, int]:
        """
        Count frequency of recurring patterns across analyzed files.
        Complexity: 3 (iterate files, extract patterns, count)
        """
        pattern_freq = {}

        for file_path, quality_info in pattern_quality.items():
            # Extract pattern types from the analysis
            good_count = quality_info.get('good_patterns', 0)
            bad_count = quality_info.get('bad_patterns', 0)

            if good_count > 0:
                pattern_freq['good_practices'] = pattern_freq.get('good_practices', 0) + good_count
            if bad_count > 0:
                pattern_freq['anti_patterns'] = pattern_freq.get('anti_patterns', 0) + bad_count

        return pattern_freq

    def _calculate_context_importance(
        self,
        avg_quality: float,
        memory_refs_count: int,
        concepts_count: int
    ) -> float:
        """
        Calculate context importance score for Context Editing API.
        Higher scores = more important to keep in context.
        Complexity: 2 (simple weighted calculation)
        """
        # Weight factors
        quality_weight = 0.5  # Quality is most important
        memory_weight = 0.3   # Memory storage indicates value
        concept_weight = 0.2  # Concept diversity matters

        # Normalize memory and concept counts (cap at reasonable max)
        memory_score = min(memory_refs_count / 5.0, 1.0)  # Max 5 refs = 1.0
        concept_score = min(concepts_count / 10.0, 1.0)    # Max 10 concepts = 1.0

        # Calculate weighted score
        importance = (
            (avg_quality * quality_weight) +
            (memory_score * memory_weight) +
            (concept_score * concept_weight)
        )

        return round(importance, 3)

    def _is_code_file(self, file_path: str) -> bool:
        """Check if file is a code file."""
        if not file_path:
            return False
        extensions = ['.py', '.ts', '.js', '.tsx', '.jsx']
        return any(file_path.endswith(ext) for ext in extensions)

    def _apply_metadata_limits(self, metadata: Dict[str, Any]):
        """Apply size limits to metadata arrays."""
        metadata['files_analyzed'] = metadata['files_analyzed'][:MAX_FILES_ANALYZED]
        metadata['files_edited'] = metadata['files_edited'][:MAX_FILES_EDITED]
        metadata['tools_used'] = metadata['tools_used'][:MAX_TOOLS_USED]
        metadata['ast_elements'] = metadata['ast_elements'][:MAX_AST_ELEMENTS]