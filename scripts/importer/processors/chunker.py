"""Intelligent chunking for conversations."""

import logging
from typing import List, Optional
from pathlib import Path

from ..core import Message, ConversationChunk

logger = logging.getLogger(__name__)


class Chunker:
    """
    Create optimized chunks from conversation messages.
    
    Implements intelligent chunking with overlap for context preservation.
    """
    
    def __init__(self, chunk_size: int = 3000, chunk_overlap: int = 200):
        self.chunk_size = chunk_size
        self.chunk_overlap = chunk_overlap
    
    def create_chunks(
        self, 
        messages: List[Message],
        file_path: str
    ) -> List[ConversationChunk]:
        """
        Create chunks from messages.
        
        Args:
            messages: List of messages to chunk
            file_path: Source file path for metadata
            
        Returns:
            List of conversation chunks
        """
        if not messages:
            return []
        
        # Generate conversation ID from file path
        conversation_id = Path(file_path).stem
        
        chunks = []
        current_chunk_text = []
        current_chunk_size = 0
        current_message_indices = []
        
        for msg in messages:
            # Format message for chunk
            formatted = self._format_message(msg)
            msg_size = len(formatted)
            
            # Check if adding this message would exceed chunk size
            if current_chunk_size + msg_size > self.chunk_size and current_chunk_text:
                # Create chunk with current messages
                chunk = self._create_chunk(
                    current_chunk_text,
                    current_message_indices,
                    len(chunks),
                    conversation_id,
                    file_path
                )
                chunks.append(chunk)
                
                # Start new chunk with overlap
                overlap_text, overlap_indices = self._get_overlap(
                    current_chunk_text,
                    current_message_indices
                )
                current_chunk_text = overlap_text
                current_message_indices = overlap_indices
                current_chunk_size = sum(len(t) for t in current_chunk_text)
            
            # Add message to current chunk
            current_chunk_text.append(formatted)
            # Fix: Check for None instead of truthiness to include index 0
            if msg.message_index is not None:
                current_message_indices.append(msg.message_index)
            current_chunk_size += msg_size
        
        # Create final chunk
        if current_chunk_text:
            chunk = self._create_chunk(
                current_chunk_text,
                current_message_indices,
                len(chunks),
                conversation_id,
                file_path
            )
            chunks.append(chunk)
        
        # Update total chunks count
        for chunk in chunks:
            chunk.total_chunks = len(chunks)
        
        logger.debug(f"Created {len(chunks)} chunks from {len(messages)} messages")
        return chunks
    
    def _format_message(self, message: Message) -> str:
        """Format a message for inclusion in chunk."""
        # Include role for context
        role_prefix = f"[{message.role.upper()}]: "
        return role_prefix + message.content
    
    def _get_overlap(
        self,
        chunk_text: List[str],
        message_indices: List[int]
    ) -> tuple[List[str], List[int]]:
        """Get overlap text and indices for next chunk."""
        if not chunk_text:
            return [], []
        
        # Calculate how many messages to include in overlap
        overlap_size = 0
        overlap_messages = []
        overlap_indices = []
        
        # Work backwards to get overlap
        for i in range(len(chunk_text) - 1, -1, -1):
            msg_size = len(chunk_text[i])
            if overlap_size + msg_size <= self.chunk_overlap:
                overlap_messages.insert(0, chunk_text[i])
                if i < len(message_indices):
                    overlap_indices.insert(0, message_indices[i])
                overlap_size += msg_size
            else:
                break
        
        return overlap_messages, overlap_indices
    
    def _create_chunk(
        self,
        text_parts: List[str],
        message_indices: List[int],
        chunk_index: int,
        conversation_id: str,
        file_path: str
    ) -> ConversationChunk:
        """Create a conversation chunk."""
        chunk_text = "\n".join(text_parts)
        
        chunk = ConversationChunk(
            text=chunk_text,
            message_indices=message_indices,
            chunk_index=chunk_index,
            total_chunks=0,  # Will be updated later
            conversation_id=conversation_id
        )
        
        # Add file metadata
        chunk.add_metadata("file_path", file_path)
        chunk.add_metadata("chunk_method", "overlap")
        chunk.add_metadata("chunk_size_chars", len(chunk_text))
        
        return chunk