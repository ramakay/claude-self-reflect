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
        """Initialize empty metadata structure."""
        return {
            "files_analyzed": [],
            "files_edited": [],
            "tools_used": [],
            "concepts": [],
            "ast_elements": [],
            "has_code_blocks": False,
            "total_messages": 0,
            "project_path": None,
            "pattern_analysis": {},
            "avg_quality_score": 0.0
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