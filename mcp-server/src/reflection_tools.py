"""Reflection tools for Claude Self Reflect MCP server."""

import os
import json
import hashlib
import logging
from typing import Optional, List, Dict, Any
from datetime import datetime, timezone
from pathlib import Path
import uuid

from fastmcp import Context
from pydantic import Field
from qdrant_client import AsyncQdrantClient
from qdrant_client.models import PointStruct, VectorParams, Distance

logger = logging.getLogger(__name__)


class ReflectionTools:
    """Handles reflection storage and conversation retrieval operations."""
    
    def __init__(
        self,
        qdrant_client: AsyncQdrantClient,
        qdrant_url: str,
        get_embedding_manager,
        normalize_project_name
    ):
        """Initialize reflection tools with dependencies."""
        self.qdrant_client = qdrant_client
        self.qdrant_url = qdrant_url
        self.get_embedding_manager = get_embedding_manager
        self.normalize_project_name = normalize_project_name

    async def store_reflection(
        self,
        ctx: Context,
        content: str,
        tags: List[str] = []
    ) -> str:
        """Store an important insight or reflection for future reference."""

        await ctx.debug(f"Storing reflection with {len(tags)} tags")

        try:
            # Check runtime preference from environment
            import os
            prefer_local = os.getenv('PREFER_LOCAL_EMBEDDINGS', 'true').lower() == 'true'

            embedding_manager = self.get_embedding_manager()

            # Use embedding_manager's model_type which already respects preferences
            embedding_type = embedding_manager.model_type

            if embedding_type == "local":
                await ctx.debug("Using LOCAL mode (FastEmbed, 384 dimensions)")
            elif embedding_type == "voyage":
                await ctx.debug("Using VOYAGE mode (Voyage AI, 1024 dimensions)")
            else:
                # Shouldn't happen but handle gracefully
                embedding_type = "local" if embedding_manager.local_model else "voyage"
                await ctx.debug(f"Using {embedding_type} mode (fallback)")

            collection_name = f"reflections_{embedding_type}"

            # Ensure reflections collection exists
            try:
                await self.qdrant_client.get_collection(collection_name)
                await ctx.debug(f"Using existing {collection_name} collection")
            except Exception:
                # Collection doesn't exist, create it
                await ctx.debug(f"Creating {collection_name} collection")

                # Get embedding dimensions for the specific type
                embedding_dim = embedding_manager.get_vector_dimension(force_type=embedding_type)

                await self.qdrant_client.create_collection(
                    collection_name=collection_name,
                    vectors_config=VectorParams(
                        size=embedding_dim,
                        distance=Distance.COSINE
                    )
                )

            # Generate embedding with the same forced type for consistency
            embedding = await embedding_manager.generate_embedding(content, force_type=embedding_type)

            # Guard against failed embeddings
            if not embedding:
                await ctx.debug("Failed to generate embedding for reflection")
                return "Failed to store reflection: embedding generation failed"
            
            # Create unique ID
            reflection_id = hashlib.md5(f"{content}{datetime.now().isoformat()}".encode()).hexdigest()
            
            # Prepare metadata
            metadata = {
                "content": content,
                "tags": tags,
                "timestamp": datetime.now(timezone.utc).isoformat(),
                "type": "reflection"
            }
            
            # Store in Qdrant
            await self.qdrant_client.upsert(
                collection_name=collection_name,
                points=[
                    PointStruct(
                        id=reflection_id,
                        vector=embedding,
                        payload=metadata
                    )
                ]
            )
            
            await ctx.debug(f"Stored reflection with ID {reflection_id}")
            
            return f"""Reflection stored successfully.
ID: {reflection_id}
Collection: {collection_name}
Tags: {', '.join(tags) if tags else 'none'}
Timestamp: {metadata['timestamp']}"""
            
        except Exception as e:
            logger.error(f"Failed to store reflection: {e}", exc_info=True)
            return f"Failed to store reflection: {str(e)}"

    async def get_full_conversation(
        self,
        ctx: Context,
        conversation_id: str,
        project: Optional[str] = None
    ) -> str:
        """Get the full JSONL conversation file path for a conversation ID.
        This allows agents to read complete conversations instead of truncated excerpts."""
        
        await ctx.debug(f"Getting full conversation for ID: {conversation_id}, project: {project}")
        
        try:
            # Base path for conversations
            base_path = Path.home() / '.claude' / 'projects'
            
            # If project is specified, try to find it in that project
            if project:
                # Normalize project name for path matching
                project_normalized = self.normalize_project_name(project)
                
                # Look for project directories that match
                for project_dir in base_path.glob('*'):
                    if project_normalized in project_dir.name.lower():
                        # Look for JSONL files in this project
                        for jsonl_file in project_dir.glob('*.jsonl'):
                            # Check if filename matches conversation_id (with or without .jsonl)
                            if conversation_id in jsonl_file.stem or conversation_id == jsonl_file.stem:
                                await ctx.debug(f"Found conversation by filename in {jsonl_file}")
                                return f"""<conversation_file>
<conversation_id>{conversation_id}</conversation_id>
<file_path>{str(jsonl_file)}</file_path>
<project>{project_dir.name}</project>
<message>Use the Read tool with this file path to read the complete conversation.</message>
</conversation_file>"""
            
            # If not found in specific project or no project specified, search all
            await ctx.debug("Searching all projects for conversation")
            for project_dir in base_path.glob('*'):
                for jsonl_file in project_dir.glob('*.jsonl'):
                    # Check if filename matches conversation_id (with or without .jsonl)
                    if conversation_id in jsonl_file.stem or conversation_id == jsonl_file.stem:
                        await ctx.debug(f"Found conversation by filename in {jsonl_file}")
                        return f"""<conversation_file>
<conversation_id>{conversation_id}</conversation_id>
<file_path>{str(jsonl_file)}</file_path>
<project>{project_dir.name}</project>
<message>Use the Read tool with this file path to read the complete conversation.</message>
</conversation_file>"""
            
            # Not found
            return f"""<conversation_file>
<error>Conversation ID '{conversation_id}' not found in any project.</error>
<suggestion>The conversation may not have been imported yet, or the ID may be incorrect.</suggestion>
</conversation_file>"""
            
        except Exception as e:
            logger.error(f"Failed to get conversation file: {e}", exc_info=True)
            return f"""<conversation_file>
<error>Failed to locate conversation: {str(e)}</error>
</conversation_file>"""


def register_reflection_tools(
    mcp,
    qdrant_client: AsyncQdrantClient,
    qdrant_url: str,
    get_embedding_manager,
    normalize_project_name
):
    """Register reflection tools with the MCP server."""
    
    tools = ReflectionTools(
        qdrant_client,
        qdrant_url,
        get_embedding_manager,
        normalize_project_name
    )
    
    @mcp.tool()
    async def store_reflection(
        ctx: Context,
        content: str = Field(description="The insight or reflection to store"),
        tags: List[str] = Field(default=[], description="Tags to categorize this reflection")
    ) -> str:
        """Store an important insight or reflection for future reference."""
        return await tools.store_reflection(ctx, content, tags)
    
    @mcp.tool()
    async def get_full_conversation(
        ctx: Context,
        conversation_id: str = Field(description="The conversation ID from search results (cid)"),
        project: Optional[str] = Field(default=None, description="Optional project name to help locate the file")
    ) -> str:
        """Get the full JSONL conversation file path for a conversation ID.
        This allows agents to read complete conversations instead of truncated excerpts."""
        return await tools.get_full_conversation(ctx, conversation_id, project)
    
    logger.info("Reflection tools registered successfully")