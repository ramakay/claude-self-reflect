"""Parser for JSONL conversation files."""

import json
import logging
from pathlib import Path
from typing import List, Dict, Any, Optional
from datetime import datetime

from ..core import Message
from ..core.exceptions import ParseError

logger = logging.getLogger(__name__)


class ConversationParser:
    """
    Parse JSONL conversation files into Message objects.
    
    Handles various conversation formats from Claude.
    """
    
    def parse_file(self, file_path: Path) -> List[Message]:
        """
        Parse a JSONL file into messages.
        
        Args:
            file_path: Path to JSONL file
            
        Returns:
            List of Message objects
            
        Raises:
            ParseError: If parsing fails
        """
        messages = []
        
        try:
            with open(file_path, 'r', encoding='utf-8') as f:
                for line_num, line in enumerate(f, 1):
                    line = line.strip()
                    if not line:
                        continue
                    
                    try:
                        data = json.loads(line)
                        message = self._parse_message(data, line_num)
                        if message:
                            messages.append(message)
                    except json.JSONDecodeError as e:
                        logger.warning(f"Skipping invalid JSON at line {line_num}: {e}")
                        # Don't fail entire file for one bad line
                        continue
            
            if not messages:
                raise ParseError(
                    str(file_path),
                    reason="No valid messages found in file"
                )
            
            # Add message indices
            for i, msg in enumerate(messages):
                msg.message_index = i
            
            logger.debug(f"Parsed {len(messages)} messages from {file_path}")
            return messages
            
        except FileNotFoundError:
            raise ParseError(str(file_path), reason="File not found")
        except Exception as e:
            if isinstance(e, ParseError):
                raise
            raise ParseError(str(file_path), reason=str(e))
    
    def _parse_message(self, data: Dict[str, Any], line_num: int) -> Optional[Message]:
        """
        Parse a single message from JSON data.
        
        Handles multiple conversation formats.
        """
        # Format 1: Direct message format
        if "role" in data and "content" in data:
            return Message(
                role=data["role"],
                content=self._extract_content(data["content"]),
                timestamp=self._parse_timestamp(data.get("timestamp")),
                metadata=self._extract_metadata(data)
            )
        
        # Format 2: Nested messages array
        if "messages" in data and isinstance(data["messages"], list):
            # Return first message or aggregate
            messages = []
            for msg_data in data["messages"]:
                if isinstance(msg_data, dict) and "role" in msg_data:
                    msg = Message(
                        role=msg_data["role"],
                        content=self._extract_content(msg_data.get("content", "")),
                        timestamp=self._parse_timestamp(msg_data.get("timestamp")),
                        metadata=self._extract_metadata(msg_data)
                    )
                    messages.append(msg)
            
            # For now, return first message
            # In future, might want to handle differently
            return messages[0] if messages else None
        
        # Format 3: Event-based format
        if "event" in data and data["event"] == "message":
            return Message(
                role=data.get("role", "unknown"),
                content=self._extract_content(data.get("text", "")),
                timestamp=self._parse_timestamp(data.get("timestamp")),
                metadata=self._extract_metadata(data)
            )
        
        # Unknown format
        logger.debug(f"Unknown message format at line {line_num}")
        return None
    
    def _extract_content(self, content: Any) -> str:
        """Extract text content from various formats."""
        if isinstance(content, str):
            return content
        
        if isinstance(content, list):
            # Handle content array format
            text_parts = []
            for item in content:
                if isinstance(item, dict):
                    if "text" in item:
                        text_parts.append(item["text"])
                    elif "content" in item:
                        text_parts.append(str(item["content"]))
                else:
                    text_parts.append(str(item))
            return "\n".join(text_parts)
        
        if isinstance(content, dict):
            if "text" in content:
                return content["text"]
            elif "content" in content:
                return str(content["content"])
        
        return str(content) if content else ""
    
    def _parse_timestamp(self, timestamp: Any) -> Optional[datetime]:
        """Parse timestamp from various formats."""
        if not timestamp:
            return None
        
        if isinstance(timestamp, datetime):
            return timestamp
        
        if isinstance(timestamp, (int, float)):
            # Unix timestamp
            try:
                return datetime.fromtimestamp(timestamp)
            except Exception:
                return None
        
        if isinstance(timestamp, str):
            # ISO format or other string formats
            try:
                return datetime.fromisoformat(timestamp)
            except Exception:
                # Try other formats if needed
                return None
        
        return None
    
    def _extract_metadata(self, data: Dict[str, Any]) -> Dict[str, Any]:
        """Extract additional metadata from message data."""
        # Skip known fields
        skip_fields = {"role", "content", "text", "timestamp", "message_index"}
        
        metadata = {}
        for key, value in data.items():
            if key not in skip_fields:
                metadata[key] = value
        
        return metadata