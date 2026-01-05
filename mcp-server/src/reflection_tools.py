"""Reflection tools for Claude Self Reflect MCP server."""

import os
import json
import hashlib
import logging
from typing import Optional, List, Dict, Any
from datetime import datetime, timezone
from pathlib import Path
import uuid
from xml.sax.saxutils import escape as xml_escape

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
            
            # SECURITY FIX: Use SHA-256 instead of MD5
            from .security_patches import SecureHashGenerator
            reflection_id = SecureHashGenerator.generate_id(f"{content}{datetime.now().isoformat()}")
            
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

            # SECURITY FIX: Validate paths to prevent traversal
            from .security_patches import PathValidator
            if not PathValidator.is_safe_path(base_path):
                logger.error(f"Unsafe base path detected: {base_path}")
                return "<conversation_file><error>Security validation failed</error></conversation_file>"

            # If project is specified, try to find it in that project
            if project:
                # Normalize project name for path matching
                from .security_patches import InputValidator
                project_normalized = InputValidator.validate_project_name(
                    self.normalize_project_name(project)
                )

                # Look for project directories that match
                for project_dir in base_path.glob('*'):
                    # Validate each path before accessing
                    if not PathValidator.is_safe_path(project_dir):
                        continue

                    if project_normalized in project_dir.name.lower():
                        # Look for JSONL files in this project
                        for jsonl_file in project_dir.glob('*.jsonl'):
                            # Validate file path
                            if not PathValidator.is_safe_path(jsonl_file):
                                continue

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
            from .security_patches import PathValidator
            for project_dir in base_path.glob('*'):
                # SECURITY FIX: Validate each path before accessing
                if not PathValidator.is_safe_path(project_dir):
                    continue

                for jsonl_file in project_dir.glob('*.jsonl'):
                    # Validate file path
                    if not PathValidator.is_safe_path(jsonl_file):
                        continue

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

    async def get_session_learnings(
        self,
        ctx: Context,
        session_id: str,
        limit: int = 50
    ) -> str:
        """Get all learnings from a specific Ralph session.

        This enables iteration-level memory: retrieve what was learned
        in previous iterations of the SAME Ralph loop session.
        """
        from qdrant_client import models

        await ctx.debug(f"Getting learnings for session: {session_id}")

        try:
            # Check runtime preference from environment
            prefer_local = os.getenv('PREFER_LOCAL_EMBEDDINGS', 'true').lower() == 'true'
            embedding_type = "local" if prefer_local else "voyage"
            collection_name = f"reflections_{embedding_type}"

            # Filter by session tag - matches reflections stored with session_{id} tag
            session_filter = models.Filter(
                must=[
                    models.FieldCondition(
                        key="tags",
                        match=models.MatchAny(any=[f"session_{session_id}"])
                    )
                ]
            )

            results, _ = await self.qdrant_client.scroll(
                collection_name=collection_name,
                scroll_filter=session_filter,
                limit=limit,
                with_payload=True,
            )

            if not results:
                await ctx.debug(f"No learnings found for session {session_id}")
                return f"""<session_learnings>
<session_id>{session_id}</session_id>
<count>0</count>
<message>No learnings stored yet for this session. Use store_reflection() with tags=['session_{session_id}', 'iteration_N', 'ralph_iteration'] to store iteration learnings.</message>
</session_learnings>"""

            # Format results
            learnings = []
            for point in results:
                payload = point.payload or {}
                tags = payload.get("tags", [])
                # Extract iteration number from tags if present
                iteration = "unknown"
                for tag in tags:
                    if tag.startswith("iteration_"):
                        iteration = tag.replace("iteration_", "")
                        break

                learnings.append({
                    "content": payload.get("content", ""),
                    "iteration": iteration,
                    "timestamp": payload.get("timestamp", ""),
                    "tags": tags
                })

            # Sort by timestamp (oldest first for chronological order)
            learnings.sort(key=lambda x: x.get("timestamp", ""))

            await ctx.debug(f"Found {len(learnings)} learnings for session {session_id}")

            # Format as XML for structured output (escape special chars for safety)
            learnings_xml = "\n".join([
                f"""<learning iteration="{xml_escape(str(l['iteration']))}">
<timestamp>{xml_escape(str(l['timestamp']))}</timestamp>
<content>{xml_escape(str(l['content']))}</content>
<tags>{xml_escape(', '.join(str(t) for t in l['tags']))}</tags>
</learning>"""
                for l in learnings
            ])

            return f"""<session_learnings>
<session_id>{session_id}</session_id>
<count>{len(learnings)}</count>
<learnings>
{learnings_xml}
</learnings>
</session_learnings>"""

        except Exception as e:
            logger.error(f"Failed to get session learnings: {e}", exc_info=True)
            return f"""<session_learnings>
<session_id>{session_id}</session_id>
<error>Failed to retrieve learnings: {str(e)}</error>
</session_learnings>"""


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

    @mcp.tool()
    async def get_session_learnings(
        ctx: Context,
        session_id: str = Field(description="Ralph session ID to get learnings from (e.g., 'ralph_20260104_224757_iter1')"),
        limit: int = Field(default=50, description="Maximum number of learnings to return")
    ) -> str:
        """Get all learnings from a specific Ralph session.

        This enables iteration-level memory: retrieve what was learned
        in previous iterations of the SAME Ralph loop session.

        Use this at the START of each Ralph iteration to see what previous
        iterations learned. Then use store_reflection() with session tags
        to save learnings at the END of each iteration."""
        return await tools.get_session_learnings(ctx, session_id, limit)

    logger.info("Reflection tools registered successfully")