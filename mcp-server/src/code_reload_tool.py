"""Runtime code reloading tool for MCP server development."""

import os
import sys
import importlib
import logging
from pathlib import Path
from typing import Dict, List, Optional, Literal
from fastmcp import Context
from pydantic import Field
import hashlib
import json

logger = logging.getLogger(__name__)


class CodeReloader:
    """Handles runtime code reloading for the MCP server."""

    def __init__(self):
        """Initialize the code reloader."""
        self.module_hashes: Dict[str, str] = {}
        self.reload_history: List[Dict] = []
        self.cache_dir = Path.home() / '.claude-self-reflect' / 'reload_cache'
        self.cache_dir.mkdir(parents=True, exist_ok=True)
        # Test comment: Hot reload test at 2025-09-15
        logger.info("CodeReloader initialized with hot reload support")

    def _get_file_hash(self, filepath: Path) -> str:
        """Get SHA256 hash of a file."""
        with open(filepath, 'rb') as f:
            return hashlib.sha256(f.read()).hexdigest()

    def _get_changed_modules(self) -> List[str]:
        """Detect which modules have changed since last check."""
        changed = []
        src_dir = Path(__file__).parent

        for py_file in src_dir.glob("*.py"):
            if py_file.name == "__pycache__":
                continue

            module_name = f"src.{py_file.stem}"
            current_hash = self._get_file_hash(py_file)

            if module_name in self.module_hashes:
                if self.module_hashes[module_name] != current_hash:
                    changed.append(module_name)

            self.module_hashes[module_name] = current_hash

        return changed

    async def reload_modules(
        self,
        ctx: Context,
        modules: Optional[List[str]] = None,
        auto_detect: bool = True
    ) -> str:
        """Reload Python modules at runtime without restarting the MCP server."""

        await ctx.debug("Starting code reload process...")

        try:
            # Track what we're reloading
            reload_targets = []

            if auto_detect:
                # Detect changed modules
                changed = self._get_changed_modules()
                if changed:
                    reload_targets.extend(changed)
                    await ctx.debug(f"Auto-detected changes in: {changed}")

            if modules:
                # Add explicitly requested modules
                reload_targets.extend(modules)

            if not reload_targets:
                return "📊 No modules to reload. All code is up to date!"

            # Perform the reload
            reloaded = []
            failed = []

            for module_name in reload_targets:
                try:
                    if module_name in sys.modules:
                        # Store old module reference for rollback
                        old_module = sys.modules[module_name]

                        # Reload the module
                        logger.info(f"Reloading module: {module_name}")
                        reloaded_module = importlib.reload(sys.modules[module_name])

                        # Update any global references if needed
                        self._update_global_references(module_name, reloaded_module)

                        reloaded.append(module_name)
                        await ctx.debug(f"✅ Reloaded: {module_name}")
                    else:
                        # Module not loaded yet, import it
                        importlib.import_module(module_name)
                        reloaded.append(module_name)
                        await ctx.debug(f"✅ Imported: {module_name}")

                except Exception as e:
                    logger.error(f"Failed to reload {module_name}: {e}", exc_info=True)
                    failed.append((module_name, str(e)))
                    await ctx.debug(f"❌ Failed: {module_name} - {e}")

            # Record reload history
            self.reload_history.append({
                "timestamp": os.environ.get('MCP_REQUEST_ID', 'unknown'),
                "reloaded": reloaded,
                "failed": failed
            })

            # Build response
            response = "🔄 **Code Reload Results**\n\n"

            if reloaded:
                response += f"**Successfully Reloaded ({len(reloaded)}):**\n"
                for module in reloaded:
                    response += f"- ✅ {module}\n"
                response += "\n"

            if failed:
                response += f"**Failed to Reload ({len(failed)}):**\n"
                for module, error in failed:
                    response += f"- ❌ {module}: {error}\n"
                response += "\n"

            response += "**Important Notes:**\n"
            response += "- Class instances created before reload keep old code\n"
            response += "- New requests will use the reloaded code\n"
            response += "- Some changes may require full restart (e.g., new tools)\n"

            return response

        except Exception as e:
            logger.error(f"Code reload failed: {e}", exc_info=True)
            return f"❌ Code reload failed: {str(e)}"

    def _update_global_references(self, module_name: str, new_module):
        """Update global references after module reload."""
        # This is where we'd update any global singleton references
        # For example, if we reload embedding_manager, we might need to
        # update the global embedding manager instance

        if module_name == "src.embedding_manager":
            # Update the global embedding manager if it exists
            if hasattr(new_module, 'get_embedding_manager'):
                # The singleton pattern should handle this automatically
                pass

        elif module_name == "src.search_tools":
            # Search tools might need to refresh their references
            pass

        # Add more specific updates as needed

    async def get_reload_status(self, ctx: Context) -> str:
        """Get the current reload status and history."""

        try:
            # Check for changed files
            changed = self._get_changed_modules()

            response = "📊 **Code Reload Status**\n\n"

            response += "**Module Status:**\n"
            if changed:
                response += f"⚠️ {len(changed)} modules have pending changes:\n"
                for module in changed:
                    response += f"  - {module}\n"
            else:
                response += "✅ All modules are up to date\n"

            response += f"\n**Tracked Modules:** {len(self.module_hashes)}\n"

            if self.reload_history:
                response += f"\n**Recent Reloads:**\n"
                for entry in self.reload_history[-5:]:  # Last 5 reloads
                    response += f"- {entry['timestamp']}: "
                    response += f"{len(entry['reloaded'])} success, "
                    response += f"{len(entry['failed'])} failed\n"

            return response

        except Exception as e:
            logger.error(f"Failed to get reload status: {e}", exc_info=True)
            return f"❌ Failed to get reload status: {str(e)}"

    async def clear_python_cache(self, ctx: Context) -> str:
        """Clear Python's module cache and bytecode."""

        try:
            await ctx.debug("Clearing Python cache...")

            # Clear __pycache__ directories
            src_dir = Path(__file__).parent
            pycache_dirs = list(src_dir.rglob("__pycache__"))

            for pycache in pycache_dirs:
                if pycache.is_dir():
                    import shutil
                    shutil.rmtree(pycache)
                    await ctx.debug(f"Removed: {pycache}")

            # Clear import cache
            importlib.invalidate_caches()

            return f"✅ Cleared {len(pycache_dirs)} __pycache__ directories and invalidated import caches"

        except Exception as e:
            logger.error(f"Failed to clear cache: {e}", exc_info=True)
            return f"❌ Failed to clear cache: {str(e)}"


def register_code_reload_tool(mcp, get_embedding_manager):
    """Register the code reloading tool with the MCP server."""

    reloader = CodeReloader()

    @mcp.tool()
    async def reload_code(
        ctx: Context,
        modules: Optional[List[str]] = Field(
            default=None,
            description="Specific modules to reload (e.g., ['src.search_tools', 'src.embedding_manager'])"
        ),
        auto_detect: bool = Field(
            default=True,
            description="Automatically detect and reload changed modules"
        )
    ) -> str:
        """Reload Python code at runtime without restarting the MCP server.

        This allows hot-reloading of code changes during development, similar to
        the mode switching capability. Changes take effect for new requests.

        Note: Some changes (new tools, startup configuration) still require restart.
        """
        return await reloader.reload_modules(ctx, modules, auto_detect)

    @mcp.tool()
    async def reload_status(ctx: Context) -> str:
        """Check which modules have pending changes and reload history.

        Shows which files have been modified since last reload and
        the history of recent reload operations.
        """
        return await reloader.get_reload_status(ctx)

    @mcp.tool()
    async def clear_module_cache(ctx: Context) -> str:
        """Clear Python's module cache and __pycache__ directories.

        Useful when reload isn't working due to cached bytecode.
        """
        return await reloader.clear_python_cache(ctx)

    logger.info("Code reload tools registered successfully")