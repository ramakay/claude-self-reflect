"""Runtime mode switching tool for embedding models."""

import os
import logging
from typing import Literal
from fastmcp import Context
from pydantic import Field

logger = logging.getLogger(__name__)


class ModeSwitcher:
    """Handles runtime switching between embedding modes."""

    def __init__(self, get_embedding_manager):
        """Initialize with embedding manager getter."""
        self.get_embedding_manager = get_embedding_manager

    async def switch_mode(
        self,
        ctx: Context,
        mode: Literal["local", "cloud"]
    ) -> str:
        """Switch between local and cloud embedding modes at runtime."""

        await ctx.debug(f"Switching to {mode} mode...")

        try:
            # Get the current embedding manager
            manager = self.get_embedding_manager()

            # Store current state
            old_mode = manager.model_type
            old_prefer_local = manager.prefer_local

            # Update configuration based on requested mode
            if mode == "local":
                # Switch to local mode
                manager.prefer_local = True
                # Clear voyage key to force local
                manager.voyage_key = None

                # Reinitialize with local preference
                if not manager.local_model:
                    success = manager.try_initialize_local()
                    if not success:
                        return "❌ Failed to initialize local model"

                # Update default model type
                manager.model_type = 'local'

                await ctx.debug("Switched to LOCAL mode (FastEmbed, 384 dimensions)")

            elif mode == "cloud":
                # Switch to cloud mode
                # First check if we have a Voyage key
                voyage_key = os.getenv('VOYAGE_KEY') or os.getenv('VOYAGE_KEY-2')
                if not voyage_key:
                    # Try to load from .env file
                    from pathlib import Path
                    from dotenv import load_dotenv
                    env_path = Path(__file__).parent.parent.parent / '.env'
                    load_dotenv(env_path, override=True)
                    voyage_key = os.getenv('VOYAGE_KEY') or os.getenv('VOYAGE_KEY-2')

                if not voyage_key:
                    return "❌ Cannot switch to cloud mode: VOYAGE_KEY not found in environment or .env file"

                manager.prefer_local = False
                manager.voyage_key = voyage_key

                # Reinitialize Voyage client
                if not manager.voyage_client:
                    success = manager.try_initialize_voyage()
                    if not success:
                        # Restore previous state
                        manager.prefer_local = old_prefer_local
                        manager.model_type = old_mode
                        return "❌ Failed to initialize Voyage AI client"

                # Update default model type
                manager.model_type = 'voyage'

                await ctx.debug("Switched to CLOUD mode (Voyage AI, 1024 dimensions)")

            # Log the switch
            logger.info(f"Mode switched from {old_mode} to {manager.model_type}")

            # Prepare detailed response
            return f"""✅ Successfully switched to {mode.upper()} mode!

**Previous Configuration:**
- Mode: {old_mode}
- Prefer Local: {old_prefer_local}

**New Configuration:**
- Mode: {manager.model_type}
- Prefer Local: {manager.prefer_local}
- Vector Dimensions: {manager.get_vector_dimension()}
- Has Voyage Key: {bool(manager.voyage_key)}

**Important Notes:**
- New reflections will go to: reflections_{manager.model_type}
- Existing collections remain unchanged
- No restart required! 🎉

**Next Steps:**
- Use `store_reflection` to test the new mode
- Use `reflect_on_past` to search across all collections"""

        except Exception as e:
            logger.error(f"Failed to switch mode: {e}", exc_info=True)
            return f"❌ Failed to switch mode: {str(e)}"

    async def get_current_mode(self, ctx: Context) -> str:
        """Get the current embedding mode and configuration."""

        try:
            manager = self.get_embedding_manager()

            # Check actual model availability
            local_available = manager.local_model is not None
            voyage_available = manager.voyage_client is not None

            return f"""📊 Current Embedding Configuration:

**Active Mode:** {manager.model_type.upper()}
**Vector Dimensions:** {manager.get_vector_dimension()}

**Configuration:**
- Prefer Local: {manager.prefer_local}
- Has Voyage Key: {bool(manager.voyage_key)}

**Available Models:**
- Local (FastEmbed): {'✅ Initialized' if local_available else '❌ Not initialized'}
- Cloud (Voyage AI): {'✅ Initialized' if voyage_available else '❌ Not initialized'}

**Collection Names:**
- Reflections: reflections_{manager.model_type}
- Conversations: [project]_{manager.model_type}

**Environment:**
- PREFER_LOCAL_EMBEDDINGS: {os.getenv('PREFER_LOCAL_EMBEDDINGS', 'not set')}
- VOYAGE_KEY: {'set' if manager.voyage_key else 'not set'}"""

        except Exception as e:
            logger.error(f"Failed to get current mode: {e}", exc_info=True)
            return f"❌ Failed to get current mode: {str(e)}"


def register_mode_switch_tool(mcp, get_embedding_manager):
    """Register the mode switching tool with the MCP server."""

    switcher = ModeSwitcher(get_embedding_manager)

    @mcp.tool()
    async def switch_embedding_mode(
        ctx: Context,
        mode: Literal["local", "cloud"] = Field(
            description="Target embedding mode: 'local' for FastEmbed (384 dim), 'cloud' for Voyage AI (1024 dim)"
        )
    ) -> str:
        """Switch between local and cloud embedding modes at runtime without restarting the MCP server.

        This allows dynamic switching between:
        - LOCAL mode: FastEmbed with 384 dimensions (privacy-first, no API calls)
        - CLOUD mode: Voyage AI with 1024 dimensions (better quality, requires API key)

        No restart required! The change takes effect immediately for all new operations.
        """
        return await switcher.switch_mode(ctx, mode)

    @mcp.tool()
    async def get_embedding_mode(ctx: Context) -> str:
        """Get the current embedding mode configuration and status.

        Shows which mode is active, available models, and collection naming.
        """
        return await switcher.get_current_mode(ctx)

    logger.info("Mode switching tools registered successfully")