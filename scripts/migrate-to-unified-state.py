#!/usr/bin/env python3
"""
Migration script to consolidate multiple state files into unified state format.

This script:
1. Backs up existing state files
2. Reads from imported-files.json, csr-watcher.json, and other state files
3. Merges all data with deduplication (newest wins)
4. Creates unified-state.json with v5.0 format
5. Provides rollback capability
"""

import json
import shutil
import sys
from pathlib import Path
from datetime import datetime, timezone
from typing import Dict, Any, List
import logging

# Add parent directory to path for imports
sys.path.append(str(Path(__file__).parent))
from unified_state_manager import UnifiedStateManager

logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)


class StateMigrator:
    """Migrates multiple state files to unified state format."""

    def __init__(self):
        """Initialize the migrator."""
        self.config_dir = Path.home() / ".claude-self-reflect" / "config"
        self.backup_dir = self.config_dir / f"backup-before-v5-{datetime.now().strftime('%Y%m%d-%H%M%S')}"
        self.state_manager = UnifiedStateManager()

        # State files to migrate
        self.state_files = [
            "imported-files.json",
            "csr-watcher.json",
            "unified-import-state.json",  # May be in archive
            "watcher-state.json",         # May be in archive
            "streaming-state.json"        # May be in archive
        ]

    def backup_existing_states(self) -> List[Path]:
        """
        Backup all existing state files.

        Returns:
            List of backed up file paths
        """
        self.backup_dir.mkdir(exist_ok=True)
        backed_up = []

        logger.info(f"Creating backups in {self.backup_dir}")

        for state_file in self.state_files:
            # Check both main and archive directories
            sources = [
                self.config_dir / state_file,
                self.config_dir / "archive" / state_file
            ]

            for source in sources:
                if source.exists():
                    dest = self.backup_dir / state_file
                    if source.parent.name == "archive":
                        dest = self.backup_dir / f"archive-{state_file}"

                    shutil.copy2(source, dest)
                    backed_up.append(dest)
                    logger.info(f"  Backed up: {state_file} → {dest.name}")

        # Also backup unified-state.json if it exists
        unified_state = self.config_dir / "unified-state.json"
        if unified_state.exists():
            dest = self.backup_dir / "unified-state.json.existing"
            shutil.copy2(unified_state, dest)
            backed_up.append(dest)
            logger.info(f"  Backed up existing unified state")

        return backed_up

    def load_state_file(self, filename: str) -> Dict[str, Any]:
        """
        Safely load a state file from config or archive directory.

        Args:
            filename: Name of the state file

        Returns:
            State dictionary or empty dict if not found
        """
        # Try main directory first
        file_paths = [
            self.config_dir / filename,
            self.config_dir / "archive" / filename
        ]

        for file_path in file_paths:
            if file_path.exists():
                try:
                    with open(file_path, 'r') as f:
                        logger.debug(f"  Loading {filename} from {file_path.parent.name}/")
                        return json.load(f)
                except Exception as e:
                    logger.error(f"  Error loading {filename}: {e}")
                    return {}

        logger.debug(f"  {filename} not found")
        return {}

    def merge_file_data(self, all_files: Dict[str, Any],
                       source_files: Dict[str, Any],
                       importer: str) -> Dict[str, Any]:
        """
        Merge file data from a source into the consolidated dictionary.

        Args:
            all_files: Consolidated file dictionary
            source_files: Files from a specific source
            importer: Name of the importer (batch/streaming)

        Returns:
            Updated consolidated dictionary
        """
        merged_count = 0
        updated_count = 0

        for file_path, metadata in source_files.items():
            normalized = UnifiedStateManager.normalize_path(file_path)

            # Check if this file already exists
            if normalized in all_files:
                # Use newer data (compare timestamps)
                existing_time = all_files[normalized].get("imported_at", "")
                new_time = metadata.get("imported_at", "")

                # Handle None and empty string in comparison
                if (not existing_time) or (new_time and new_time > existing_time):
                    # Update with newer data
                    all_files[normalized] = {
                        "imported_at": metadata.get("imported_at"),
                        "last_modified": metadata.get("last_modified", metadata.get("imported_at")),
                        "chunks": metadata.get("chunks", 0),
                        "importer": importer,
                        "collection": metadata.get("collection"),
                        "embedding_mode": metadata.get("embedding_mode", "local"),
                        "status": "completed",
                        "error": None,
                        "retry_count": 0
                    }
                    updated_count += 1
            else:
                # Add new file
                all_files[normalized] = {
                    "imported_at": metadata.get("imported_at"),
                    "last_modified": metadata.get("last_modified", metadata.get("imported_at")),
                    "chunks": metadata.get("chunks", 0),
                    "importer": importer,
                    "collection": metadata.get("collection"),
                    "embedding_mode": metadata.get("embedding_mode", "local"),
                    "status": "completed",
                    "error": None,
                    "retry_count": 0
                }
                merged_count += 1

        logger.info(f"    {importer}: {merged_count} new, {updated_count} updated")
        return all_files

    def calculate_collection_stats(self, all_files: Dict[str, Any]) -> Dict[str, Any]:
        """
        Calculate statistics for each collection.

        Args:
            all_files: All imported files

        Returns:
            Collection statistics dictionary
        """
        collections = {}

        for file_path, metadata in all_files.items():
            collection = metadata.get("collection")
            if collection:
                if collection not in collections:
                    collections[collection] = {
                        "files": 0,
                        "chunks": 0,
                        "embedding_mode": metadata.get("embedding_mode", "local"),
                        "dimensions": 384 if metadata.get("embedding_mode") == "local" else 1024
                    }
                collections[collection]["files"] += 1
                collections[collection]["chunks"] += metadata.get("chunks", 0)

        return collections

    def migrate(self, dry_run: bool = False) -> bool:
        """
        Perform the migration.

        Args:
            dry_run: If True, only simulate migration without writing

        Returns:
            True if successful, False otherwise
        """
        try:
            print("\n" + "="*60)
            print("Claude Self-Reflect State Migration to v5.0")
            print("="*60)

            # Step 1: Backup
            print("\n1. Creating backups...")
            backed_up = self.backup_existing_states()
            print(f"   ✓ Backed up {len(backed_up)} files")

            # Step 2: Load all state files
            print("\n2. Loading existing state files...")
            imported_files = self.load_state_file("imported-files.json")
            csr_watcher = self.load_state_file("csr-watcher.json")
            unified_import = self.load_state_file("unified-import-state.json")
            watcher_state = self.load_state_file("watcher-state.json")
            streaming_state = self.load_state_file("streaming-state.json")

            # Step 3: Merge data
            print("\n3. Merging state data...")
            all_files = {}

            # Process imported-files.json (batch importer)
            if "imported_files" in imported_files:
                all_files = self.merge_file_data(
                    all_files,
                    imported_files["imported_files"],
                    "batch"
                )
            elif imported_files:  # Might be at root level
                all_files = self.merge_file_data(
                    all_files,
                    imported_files,
                    "batch"
                )

            # Process csr-watcher.json (streaming watcher)
            if "imported_files" in csr_watcher:
                all_files = self.merge_file_data(
                    all_files,
                    csr_watcher["imported_files"],
                    "streaming"
                )

            # Process unified-import-state.json if exists
            if "files" in unified_import:
                all_files = self.merge_file_data(
                    all_files,
                    unified_import["files"],
                    "unified"
                )

            # Process other watcher states
            for state_data, name in [(watcher_state, "watcher"), (streaming_state, "streaming")]:
                if "imported_files" in state_data:
                    all_files = self.merge_file_data(
                        all_files,
                        state_data["imported_files"],
                        name
                    )

            # Step 4: Calculate statistics
            print("\n4. Calculating statistics...")
            total_chunks = sum(f.get("chunks", 0) for f in all_files.values())
            collections = self.calculate_collection_stats(all_files)

            print(f"   - Total files: {len(all_files)}")
            print(f"   - Total chunks: {total_chunks}")
            print(f"   - Collections: {len(collections)}")

            if dry_run:
                print("\n5. DRY RUN - Not writing changes")
                print("\nMigration preview complete!")
                return True

            # Step 5: Create unified state
            print("\n5. Creating unified state...")

            def create_unified_state(state):
                # Replace all file data
                state["files"] = all_files

                # Update metadata
                state["metadata"]["total_files"] = len(all_files)
                state["metadata"]["total_chunks"] = total_chunks
                state["metadata"]["migration_from"] = "v3-v4-multi-file"
                state["metadata"]["migration_date"] = datetime.now(timezone.utc).isoformat()
                state["metadata"]["migration_stats"] = {
                    "imported_files_count": len(imported_files.get("imported_files", {})),
                    "csr_watcher_count": len(csr_watcher.get("imported_files", {})),
                    "unified_count": len(all_files)
                }

                # Update collections
                state["collections"] = collections

                # Update importer stats
                batch_files = [f for f in all_files.values() if f.get("importer") == "batch"]
                streaming_files = [f for f in all_files.values() if f.get("importer") == "streaming"]

                state["importers"]["batch"]["files_processed"] = len(batch_files)
                state["importers"]["batch"]["chunks_imported"] = sum(f.get("chunks", 0) for f in batch_files)

                state["importers"]["streaming"]["files_processed"] = len(streaming_files)
                state["importers"]["streaming"]["chunks_imported"] = sum(f.get("chunks", 0) for f in streaming_files)

                return state

            self.state_manager.update_state(create_unified_state)

            print(f"   ✓ Created unified state at {self.state_manager.state_file}")

            # Step 6: Verification
            print("\n6. Verifying migration...")
            status = self.state_manager.get_status()
            print(f"   - Version: {status['version']}")
            print(f"   - Files: {status['indexed_files']}/{status['total_files']}")
            print(f"   - Chunks: {status['total_chunks']}")
            print(f"   - Collections: {', '.join(status['collections'])}")

            print("\n" + "="*60)
            print("✅ Migration completed successfully!")
            print(f"   - Backups saved to: {self.backup_dir}")
            print(f"   - Unified state: {self.state_manager.state_file}")
            print("\nNext steps:")
            print("   1. Update import scripts to use unified_state_manager")
            print("   2. Test with: python unified_state_manager.py status")
            print("   3. If issues occur, restore from:", self.backup_dir)
            print("="*60 + "\n")

            return True

        except Exception as e:
            logger.error(f"Migration failed: {e}")
            print(f"\n❌ Migration failed: {e}")
            print(f"   Backups available at: {self.backup_dir}")
            return False

    def rollback(self):
        """Rollback to backed up state files."""
        print("\nRolling back migration...")

        if not self.backup_dir.exists():
            print("❌ No backup directory found")
            return False

        # Remove unified state
        unified_state = self.config_dir / "unified-state.json"
        if unified_state.exists():
            unified_state.unlink()
            print(f"   Removed {unified_state}")

        # Restore backed up files
        for backup_file in self.backup_dir.glob("*.json"):
            if backup_file.name == "unified-state.json.existing":
                # Restore previous unified state
                dest = self.config_dir / "unified-state.json"
            elif backup_file.name.startswith("archive-"):
                # Restore to archive directory
                self.config_dir.joinpath("archive").mkdir(exist_ok=True)
                dest = self.config_dir / "archive" / backup_file.name.replace("archive-", "")
            else:
                # Restore to main directory
                dest = self.config_dir / backup_file.name

            shutil.copy2(backup_file, dest)
            print(f"   Restored {backup_file.name} → {dest}")

        print("✅ Rollback complete")
        return True


def main():
    """Main entry point."""
    import argparse

    parser = argparse.ArgumentParser(
        description="Migrate multiple state files to unified state format"
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Preview migration without making changes"
    )
    parser.add_argument(
        "--rollback",
        action="store_true",
        help="Rollback to previous state files"
    )
    parser.add_argument(
        "--verbose",
        "-v",
        action="store_true",
        help="Enable verbose logging"
    )

    args = parser.parse_args()

    if args.verbose:
        logging.getLogger().setLevel(logging.DEBUG)

    migrator = StateMigrator()

    if args.rollback:
        success = migrator.rollback()
    else:
        success = migrator.migrate(dry_run=args.dry_run)

    sys.exit(0 if success else 1)


if __name__ == "__main__":
    main()