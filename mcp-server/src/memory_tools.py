"""
Memory Tool handler for Claude 2.0.1 Memory & Context Management.

Implements persistent memory storage with:
- Security: Path traversal protection
- CRUD operations: view, create, update, delete, rename
- Search capabilities: text search with preview
- Qdrant integration: Handled externally in server.py via store_to_memory tool

Complexity: Designed following PR #69 patterns (target complexity <5)
"""

import logging
from pathlib import Path
from typing import Optional, List, Dict, Any
from datetime import datetime
import json

logger = logging.getLogger(__name__)


class MemoryToolHandler:
    """
    Handle Memory Tool operations with security and organization.
    Complexity: 3 (simple CRUD operations)
    """

    def __init__(self, base_path: Optional[str] = None):
        """Initialize memory handler with base directory."""
        if base_path is None:
            base_path = str(Path.home() / ".claude" / "memories")

        self.base_path = Path(base_path).expanduser()
        self.base_path.mkdir(parents=True, exist_ok=True)

        # Create standard subdirectories
        self._init_directories()

    def _init_directories(self) -> None:
        """Create standard memory organization directories."""
        standard_dirs = [
            "patterns",
            "insights",
            "quality",
            "projects"
        ]

        for dir_name in standard_dirs:
            (self.base_path / dir_name).mkdir(parents=True, exist_ok=True)

    def _validate_path(self, path: str) -> Path:
        """
        Validate and resolve path to prevent path traversal attacks.
        Complexity: 2 (validation logic)
        """
        # Remove any leading slashes to force relative paths
        path = path.lstrip('/')

        # Resolve the full path (resolve base_path first to ensure consistency)
        resolved_base = self.base_path.resolve()
        full_path = (resolved_base / path).resolve()

        # Security check: ensure path is within base_path
        try:
            full_path.relative_to(resolved_base)
        except ValueError:
            raise ValueError(f"Path traversal detected: {path}")

        return full_path

    async def view(self, path: str) -> str:
        """
        View memory file contents.
        Complexity: 2 (file read with error handling)
        """
        try:
            file_path = self._validate_path(path)

            if not file_path.exists():
                return f"Memory file not found: {path}"

            content = file_path.read_text(encoding='utf-8')
            return content

        except ValueError as e:
            logger.warning(f"Security violation in view: {e}")
            return f"Security error: {str(e)}"
        except Exception as e:
            logger.error(f"Error viewing memory {path}: {e}")
            return f"Error reading memory: {str(e)}"

    async def create(self, path: str, content: str) -> str:
        """
        Create new memory file.
        Complexity: 3 (validation, creation, error handling)
        """
        try:
            file_path = self._validate_path(path)

            if file_path.exists():
                return f"Memory file already exists: {path}"

            # Ensure parent directory exists
            file_path.parent.mkdir(parents=True, exist_ok=True)

            # Write content
            file_path.write_text(content, encoding='utf-8')

            logger.info(f"Created memory: {path}")
            return f"✅ Created memory: {path}"

        except ValueError as e:
            logger.warning(f"Security violation in create: {e}")
            return f"Security error: {str(e)}"
        except Exception as e:
            logger.error(f"Error creating memory {path}: {e}")
            return f"Error creating memory: {str(e)}"

    async def str_replace(self, path: str, old: str, new: str) -> str:
        """
        Replace text in memory file.
        Complexity: 4 (read, search, replace, write)
        """
        try:
            file_path = self._validate_path(path)

            if not file_path.exists():
                return f"Memory file not found: {path}"

            content = file_path.read_text(encoding='utf-8')

            if old not in content:
                return f"Text not found in {path}"

            # Replace first occurrence
            new_content = content.replace(old, new, 1)
            file_path.write_text(new_content, encoding='utf-8')

            logger.info(f"Updated memory: {path}")
            return f"✅ Updated memory: {path}"

        except ValueError as e:
            logger.warning(f"Security violation in str_replace: {e}")
            return f"Security error: {str(e)}"
        except Exception as e:
            logger.error(f"Error updating memory {path}: {e}")
            return f"Error updating memory: {str(e)}"

    async def insert(self, path: str, position: int, content: str) -> str:
        """
        Insert content at position in memory file.
        Complexity: 3 (read, insert, write)
        """
        try:
            file_path = self._validate_path(path)

            if not file_path.exists():
                return f"Memory file not found: {path}"

            existing = file_path.read_text(encoding='utf-8')
            new_content = existing[:position] + content + existing[position:]

            file_path.write_text(new_content, encoding='utf-8')

            logger.info(f"Inserted content in memory: {path}")
            return f"✅ Inserted content in memory: {path}"

        except ValueError as e:
            logger.warning(f"Security violation in insert: {e}")
            return f"Security error: {str(e)}"
        except Exception as e:
            logger.error(f"Error inserting in memory {path}: {e}")
            return f"Error inserting content: {str(e)}"

    async def delete(self, path: str) -> str:
        """
        Delete memory file.
        Complexity: 2 (validation and deletion)
        """
        try:
            file_path = self._validate_path(path)

            if not file_path.exists():
                return f"Memory file not found: {path}"

            file_path.unlink()

            logger.info(f"Deleted memory: {path}")
            return f"✅ Deleted memory: {path}"

        except ValueError as e:
            logger.warning(f"Security violation in delete: {e}")
            return f"Security error: {str(e)}"
        except Exception as e:
            logger.error(f"Error deleting memory {path}: {e}")
            return f"Error deleting memory: {str(e)}"

    async def rename(self, old_path: str, new_path: str) -> str:
        """
        Rename/move memory file.
        Complexity: 3 (validate both paths, rename)
        """
        try:
            old_file = self._validate_path(old_path)
            new_file = self._validate_path(new_path)

            if not old_file.exists():
                return f"Memory file not found: {old_path}"

            if new_file.exists():
                return f"Destination already exists: {new_path}"

            # Ensure destination directory exists
            new_file.parent.mkdir(parents=True, exist_ok=True)

            old_file.rename(new_file)

            logger.info(f"Renamed memory: {old_path} → {new_path}")
            return f"✅ Renamed memory: {old_path} → {new_path}"

        except ValueError as e:
            logger.warning(f"Security violation in rename: {e}")
            return f"Security error: {str(e)}"
        except Exception as e:
            logger.error(f"Error renaming memory {old_path}: {e}")
            return f"Error renaming memory: {str(e)}"

    async def list_memories(self, category: Optional[str] = None) -> List[Dict[str, Any]]:
        """
        List all memory files, optionally filtered by category.
        Complexity: 3 (directory traversal with filtering)
        """
        try:
            search_path = self.base_path
            if category:
                search_path = self.base_path / category
                if not search_path.exists():
                    return []

            memories = []
            for file_path in search_path.rglob("*.md"):
                relative_path = file_path.relative_to(self.base_path)
                memories.append({
                    "path": str(relative_path),
                    "size": file_path.stat().st_size,
                    "modified": datetime.fromtimestamp(file_path.stat().st_mtime).isoformat()
                })

            return memories

        except Exception as e:
            logger.error(f"Error listing memories: {e}")
            return []

    async def search(self, query: str, category: Optional[str] = None) -> List[Dict[str, Any]]:
        """
        Search memory files for query string.
        Complexity: 4 (traversal, search, result building)

        Note: Skips files larger than 1MB to prevent OOM issues.
        """
        # File size limit to prevent memory exhaustion
        MAX_SEARCH_FILE_SIZE = 1_000_000  # 1MB limit

        try:
            search_path = self.base_path
            if category:
                search_path = self.base_path / category
                if not search_path.exists():
                    return []

            results = []
            query_lower = query.lower()

            for file_path in search_path.rglob("*.md"):
                try:
                    # Check file size before loading into memory
                    file_size = file_path.stat().st_size
                    if file_size > MAX_SEARCH_FILE_SIZE:
                        logger.warning(
                            f"Skipping large file {file_path.name}: "
                            f"{file_size:,} bytes (limit: {MAX_SEARCH_FILE_SIZE:,})"
                        )
                        continue

                    content = file_path.read_text(encoding='utf-8')

                    if query_lower in content.lower():
                        relative_path = file_path.relative_to(self.base_path)

                        # Find preview context around match
                        preview = self._get_preview(content, query_lower)

                        results.append({
                            "path": str(relative_path),
                            "preview": preview,
                            "size": file_size
                        })
                except Exception as e:
                    logger.warning(f"Error searching {file_path}: {e}")
                    continue

            return results

        except Exception as e:
            logger.error(f"Error in memory search: {e}")
            return []

    @staticmethod
    def _get_preview(content: str, query: str, context_chars: int = 150) -> str:
        """
        Extract preview text around query match.
        Complexity: 3 (string manipulation)
        """
        content_lower = content.lower()
        index = content_lower.find(query)

        if index == -1:
            return content[:200]

        # Get context around match
        start = max(0, index - context_chars // 2)
        end = min(len(content), index + len(query) + context_chars // 2)

        preview = content[start:end]

        # Add ellipsis if truncated
        if start > 0:
            preview = "..." + preview
        if end < len(content):
            preview = preview + "..."

        return preview


# Module-level instance for convenience
_default_handler: Optional[MemoryToolHandler] = None


def get_memory_handler() -> MemoryToolHandler:
    """
    Get or create default memory handler.
    Complexity: 1 (singleton access)
    """
    global _default_handler
    if _default_handler is None:
        _default_handler = MemoryToolHandler()
    return _default_handler
