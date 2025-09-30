"""
Import strategies using Strategy pattern to reduce complexity of stream_import_file.
"""

import json
import gc
import os
import logging
from abc import ABC, abstractmethod
from pathlib import Path
from typing import Dict, Any, List, Optional, Generator
from datetime import datetime

from message_processors import MessageProcessorFactory

logger = logging.getLogger(__name__)


class ImportStrategy(ABC):
    """Abstract base class for import strategies."""

    @abstractmethod
    def import_file(self, jsonl_file: Path, collection_name: str, project_path: Path) -> int:
        """Import a JSONL file using the specific strategy."""
        pass


class ChunkBuffer:
    """Manages buffering and processing of message chunks."""

    def __init__(self, max_size: int = 50):
        self.buffer: List[Dict[str, Any]] = []
        self.max_size = max_size
        self.current_index = 0
        # Add memory limit for message content
        self.max_content_length = int(os.getenv('MAX_MESSAGE_CONTENT_LENGTH', '5000'))

    def add(self, message: Dict[str, Any]) -> bool:
        """Add a message to the buffer. Returns True if buffer is full."""
        # Truncate long content to prevent memory issues
        if 'content' in message and len(message['content']) > self.max_content_length:
            message = message.copy()
            message['content'] = message['content'][:self.max_content_length] + '...[truncated]'
        self.buffer.append(message)
        return len(self.buffer) >= self.max_size

    def get_and_clear(self) -> List[Dict[str, Any]]:
        """Get buffer contents and clear it."""
        contents = self.buffer.copy()
        self.buffer.clear()
        return contents

    def has_content(self) -> bool:
        """Check if buffer has any content."""
        return len(self.buffer) > 0


class MessageStreamReader:
    """Handles reading and parsing messages from JSONL files."""

    def __init__(self):
        self.processor_factory = MessageProcessorFactory()
        self.current_message_index = 0

    def read_messages(self, file_path: Path) -> Generator[Dict[str, Any], None, None]:
        """Generator that yields processed messages from a JSONL file."""
        self.current_message_index = 0

        with open(file_path, 'r', encoding='utf-8') as f:
            for line_num, line in enumerate(f, 1):
                line = line.strip()
                if not line:
                    continue

                message = self._parse_line(line, line_num)
                if message:
                    yield message

    def _parse_line(self, line: str, line_num: int) -> Optional[Dict[str, Any]]:
        """Parse a single line and extract message if present."""
        try:
            data = json.loads(line)

            # Skip summary lines
            if data.get('type') == 'summary':
                return None

            # Handle message entries
            if 'message' in data and data['message']:
                return self._process_message(data['message'])

            # Handle top-level tool entries
            entry_type = data.get('type')
            if entry_type in ('tool_result', 'tool_use'):
                return self._process_tool_entry(data, entry_type)

        except json.JSONDecodeError:
            logger.debug(f"Skipping invalid JSON at line {line_num}")
        except (KeyError, TypeError, ValueError) as e:
            logger.debug(f"Error processing data at line {line_num}: {e}")
        except Exception as e:
            logger.warning(f"Unexpected error at line {line_num}: {e}")

        return None

    def _process_message(self, message: Dict[str, Any]) -> Optional[Dict[str, Any]]:
        """Process a message entry."""
        role = message.get('role')
        content = message.get('content')

        if not role or not content:
            return None

        # Process content
        text_content = self._extract_text_content(content)

        if not text_content:
            return None

        # Track message index for user/assistant messages
        if role in ['user', 'assistant']:
            message_idx = self.current_message_index
            self.current_message_index += 1
        else:
            message_idx = 0

        return {
            'role': role,
            'content': text_content,
            'message_index': message_idx
        }

    def _extract_text_content(self, content: Any) -> str:
        """Extract text content from various content formats."""
        if isinstance(content, str):
            return content

        if isinstance(content, list):
            text_parts = []
            for item in content:
                if isinstance(item, dict):
                    text = self._process_content_item(item)
                    if text:
                        text_parts.append(text)
                elif isinstance(item, str):
                    text_parts.append(item)
            return '\n'.join(text_parts)

        return ''

    def _process_content_item(self, item: Dict[str, Any]) -> Optional[str]:
        """Process a single content item."""
        item_type = item.get('type', '')

        if item_type == 'text':
            return item.get('text', '')
        elif item_type == 'thinking':
            thinking_content = item.get('thinking', '')
            return f"[Thinking] {thinking_content[:1000]}" if thinking_content else None
        elif item_type == 'tool_use':
            tool_name = item.get('name', 'unknown')
            tool_input = str(item.get('input', ''))[:500]
            return f"[Tool: {tool_name}] {tool_input}"
        elif item_type == 'tool_result':
            result_content = str(item.get('content', ''))[:1000]
            return f"[Result] {result_content}"

        return None

    def _process_tool_entry(self, data: Dict[str, Any], entry_type: str) -> Optional[Dict[str, Any]]:
        """Process a top-level tool entry."""
        text_parts = []

        if entry_type == 'tool_use':
            tool_name = data.get('name', 'unknown')
            tool_input = str(data.get('input', ''))[:500]
            text_parts.append(f"[Tool: {tool_name}] {tool_input}")

        elif entry_type == 'tool_result':
            result_content = self._extract_tool_result(data)
            text_parts.append(f"[Result] {result_content[:1000]}")

        content = "\n".join(text_parts)
        if not content:
            return None

        message_idx = self.current_message_index
        self.current_message_index += 1

        return {
            'role': entry_type,
            'content': content,
            'message_index': message_idx
        }

    def _extract_tool_result(self, data: Dict[str, Any]) -> str:
        """Extract result content from tool result data."""
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


class StreamImportStrategy(ImportStrategy):
    """
    Strategy for streaming import with chunked processing.
    This is the main refactored implementation.
    """

    def __init__(self, client, process_chunk_fn, state_manager, max_chunk_size: int = 50,
                 cleanup_tolerance: int = None):
        self.client = client
        self.process_chunk_fn = process_chunk_fn
        self.state_manager = state_manager
        self.max_chunk_size = max_chunk_size
        # Make cleanup tolerance configurable via environment variable
        self.cleanup_tolerance = cleanup_tolerance or int(os.getenv('CLEANUP_TOLERANCE', '5'))
        self.stream_reader = MessageStreamReader()

    def import_file(self, jsonl_file: Path, collection_name: str, project_path: Path) -> int:
        """Import a JSONL file using streaming strategy."""
        logger.info(f"Streaming import of {jsonl_file.name}")

        conversation_id = jsonl_file.stem

        # Extract metadata first (lightweight)
        from metadata_extractor import MetadataExtractor
        extractor = MetadataExtractor()
        metadata, created_at, total_messages = extractor.extract_metadata_from_file(str(jsonl_file))

        # Initialize chunk processing
        chunk_buffer = ChunkBuffer(self.max_chunk_size)
        chunk_index = 0
        total_chunks = 0

        try:
            # Stream and process messages
            for message in self.stream_reader.read_messages(jsonl_file):
                if chunk_buffer.add(message):
                    # Buffer is full, process chunk
                    chunks = self._process_buffer(
                        chunk_buffer, chunk_index, conversation_id,
                        created_at, metadata, collection_name, project_path, total_messages
                    )
                    total_chunks += chunks
                    chunk_index += 1

                    # Force garbage collection after each chunk
                    gc.collect()

                    # Log progress
                    if chunk_index % 10 == 0:
                        logger.info(f"Processed {chunk_index} chunks from {jsonl_file.name}")

            # Process remaining messages
            if chunk_buffer.has_content():
                chunks = self._process_buffer(
                    chunk_buffer, chunk_index, conversation_id,
                    created_at, metadata, collection_name, project_path, total_messages
                )
                total_chunks += chunks

            # Clean up old points after successful import
            if total_chunks > 0:
                self._cleanup_old_points(conversation_id, collection_name, total_chunks)

            logger.info(f"Imported {total_chunks} chunks from {jsonl_file.name}")
            return total_chunks

        except (IOError, OSError) as e:
            logger.error(f"Failed to read file {jsonl_file}: {e}")
            self._mark_failed(jsonl_file, str(e))
            return 0
        except json.JSONDecodeError as e:
            logger.error(f"Invalid JSON in {jsonl_file}: {e}")
            self._mark_failed(jsonl_file, str(e))
            return 0
        except Exception as e:
            logger.error(f"Unexpected error importing {jsonl_file}: {e}")
            self._mark_failed(jsonl_file, str(e))
            return 0

    def _process_buffer(self, chunk_buffer: ChunkBuffer, chunk_index: int,
                        conversation_id: str, created_at: str, metadata: Dict[str, Any],
                        collection_name: str, project_path: Path, total_messages: int) -> int:
        """Process a buffer of messages and return number of chunks created."""
        messages = chunk_buffer.get_and_clear()
        return self.process_chunk_fn(
            messages, chunk_index, conversation_id,
            created_at, metadata, collection_name, project_path, total_messages
        )

    def _cleanup_old_points(self, conversation_id: str, collection_name: str, total_chunks: int):
        """Clean up old points after successful import."""
        try:
            from qdrant_client.models import Filter, FieldCondition, MatchValue

            # Count old points using count API
            old_count_filter = Filter(
                must=[FieldCondition(key="conversation_id", match=MatchValue(value=conversation_id))]
            )

            # Use count API to get actual count
            old_count = self.client.count(
                collection_name=collection_name,
                count_filter=old_count_filter,
                exact=True
            ).count

            if old_count > total_chunks + self.cleanup_tolerance:
                # Use filter parameter for delete
                self.client.delete(
                    collection_name=collection_name,
                    points_selector=Filter(
                        must=[FieldCondition(key="conversation_id", match=MatchValue(value=conversation_id))]
                    ),
                    wait=True
                )
                logger.info(f"Deleted {old_count - total_chunks} old points for conversation {conversation_id}")

        except ImportError as e:
            logger.debug(f"Qdrant client import error: {e}")
        except Exception as e:
            logger.warning(f"Could not clean up old points for {conversation_id}: {e}")

    def _mark_failed(self, jsonl_file: Path, error: str):
        """Mark a file as failed in state manager."""
        try:
            self.state_manager.mark_file_failed(str(jsonl_file), error)
        except AttributeError as e:
            logger.debug(f"State manager method not available: {e}")
        except Exception as e:
            logger.warning(f"Unexpected error marking file as failed: {e}")