#!/usr/bin/env python3
"""
Migration script for MD5 to SHA-256 ID conversion
Maintains backward compatibility for existing conversations
"""

import hashlib
import uuid
import asyncio
import json
import logging
from pathlib import Path
from typing import Dict, List, Set
from datetime import datetime
from qdrant_client import AsyncQdrantClient

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)


class IdMigrationTool:
    """Tool for migrating conversation IDs from MD5 to SHA-256"""

    def __init__(self, qdrant_url: str, api_key: str = None):
        self.qdrant_url = qdrant_url
        self.api_key = api_key
        self.client = None
        self.migration_log = []
        self.backup_path = Path.home() / '.claude-self-reflect' / 'backups'

    async def connect(self):
        """Connect to Qdrant"""
        self.client = AsyncQdrantClient(
            url=self.qdrant_url,
            api_key=self.api_key,
            timeout=30
        )

    async def backup_collections(self) -> Path:
        """Create backup of all collections before migration"""
        self.backup_path.mkdir(parents=True, exist_ok=True)
        backup_file = self.backup_path / f"backup_{datetime.now().isoformat()}.json"

        logger.info(f"Creating backup at {backup_file}")

        backup_data = {
            'timestamp': datetime.now().isoformat(),
            'collections': {}
        }

        collections = await self.client.get_collections()

        for collection in collections.collections:
            logger.info(f"Backing up collection: {collection.name}")

            # Get all points from collection
            offset = None
            all_points = []

            while True:
                response = await self.client.scroll(
                    collection_name=collection.name,
                    offset=offset,
                    limit=100,
                    with_payload=True,
                    with_vector=True
                )

                points, next_offset = response
                all_points.extend(points)

                # Use next_offset to determine if there are more points
                if next_offset is None:
                    break
                offset = next_offset

            backup_data['collections'][collection.name] = {
                'points': len(all_points),
                'data': [
                    {
                        'id': str(point.id),
                        'payload': point.payload,
                        'vector': point.vector
                    }
                    for point in all_points
                ]
            }

        with open(backup_file, 'w') as f:
            json.dump(backup_data, f, indent=2, default=str)

        logger.info(f"Backup completed: {backup_file}")
        return backup_file

    def generate_new_id(self, content: str) -> str:
        """Generate SHA-256 based ID"""
        sha256_hash = hashlib.sha256(content.encode()).hexdigest()
        unique_suffix = str(uuid.uuid4())[:8]
        return f"{sha256_hash}_{unique_suffix}"

    def is_md5_id(self, id_str: str) -> bool:
        """Check if an ID is using the legacy MD5 format"""
        return len(str(id_str)) == 32 and '_' not in str(id_str)

    async def migrate_collection(self, collection_name: str) -> Dict:
        """Migrate IDs in a single collection"""
        logger.info(f"Migrating collection: {collection_name}")

        stats = {
            'collection': collection_name,
            'total_points': 0,
            'md5_ids': 0,
            'migrated': 0,
            'errors': 0,
            'id_mapping': {}
        }

        # Get all points
        offset = None
        while True:
            response = await self.client.scroll(
                collection_name=collection_name,
                offset=offset,
                limit=100,
                with_payload=True,
                with_vector=True
            )

            points, next_offset = response
            stats['total_points'] += len(points)

            for point in points:
                old_id = str(point.id)

                # Check if this is an MD5 ID
                if self.is_md5_id(old_id):
                    stats['md5_ids'] += 1

                    # Generate new ID based on content
                    content = point.payload.get('content', '')
                    if not content:
                        # Try to reconstruct content from other fields
                        content = json.dumps(point.payload)

                    new_id = self.generate_new_id(content)

                    try:
                        # Handle both vector and vectors (named vectors)
                        vec = getattr(point, 'vectors', None) or getattr(point, 'vector', None)

                        # Prepare upsert point
                        upsert_point = {
                            'id': new_id,
                            'payload': {
                                **point.payload,
                                'original_md5_id': old_id,
                                'migrated_at': datetime.now().isoformat()
                            }
                        }

                        # Add vector or vectors based on what's present
                        if isinstance(vec, dict):
                            upsert_point['vectors'] = vec
                        else:
                            upsert_point['vector'] = vec

                        # Create new point with new ID
                        await self.client.upsert(
                            collection_name=collection_name,
                            points=[upsert_point]
                        )

                        # Store mapping
                        stats['id_mapping'][old_id] = new_id
                        stats['migrated'] += 1

                        logger.debug(f"Migrated {old_id} -> {new_id}")

                        # Delete old ID to prevent duplicates (only after successful creation)
                        await self.client.delete(
                            collection_name=collection_name,
                            points_selector={'ids': [old_id]}
                        )

                    except Exception as e:
                        logger.error(f"Failed to migrate {old_id}: {e}")
                        stats['errors'] += 1

            # Use next_offset to determine if there are more points
            if next_offset is None:
                break
            offset = next_offset

        return stats

    async def create_id_mapping_file(self, mappings: Dict[str, Dict]) -> Path:
        """Create a mapping file for reference"""
        mapping_file = self.backup_path / f"id_mapping_{datetime.now().isoformat()}.json"

        with open(mapping_file, 'w') as f:
            json.dump({
                'timestamp': datetime.now().isoformat(),
                'mappings': mappings
            }, f, indent=2)

        logger.info(f"ID mapping saved to: {mapping_file}")
        return mapping_file

    async def verify_migration(self, stats: List[Dict]) -> bool:
        """Verify that migration was successful"""
        logger.info("Verifying migration...")

        for collection_stats in stats:
            collection_name = collection_stats['collection']

            # Check that we can still find old conversations
            for old_id, new_id in collection_stats['id_mapping'].items():
                # Try to find by new ID
                try:
                    result = await self.client.retrieve(
                        collection_name=collection_name,
                        ids=[new_id]
                    )

                    if not result:
                        logger.error(f"Could not find migrated point {new_id} (was {old_id})")
                        return False

                    # Verify it has the original ID reference
                    if result[0].payload.get('original_md5_id') != old_id:
                        logger.error(f"Missing original_md5_id reference for {new_id}")
                        return False

                except Exception as e:
                    logger.error(f"Verification failed for {new_id}: {e}")
                    return False

        logger.info("Migration verification successful!")
        return True

    async def run_migration(self, dry_run: bool = False) -> bool:
        """Run the complete migration process"""
        try:
            await self.connect()

            # Step 1: Create backup
            if not dry_run:
                backup_file = await self.backup_collections()
                logger.info(f"Backup created: {backup_file}")
            else:
                logger.info("Dry run - skipping backup")

            # Step 2: Get all collections
            collections = await self.client.get_collections()
            logger.info(f"Found {len(collections.collections)} collections")

            # Step 3: Migrate each collection
            all_stats = []
            all_mappings = {}

            for collection in collections.collections:
                if collection.name.startswith('csr_'):
                    logger.info(f"Processing collection: {collection.name}")

                    if not dry_run:
                        stats = await self.migrate_collection(collection.name)
                        all_stats.append(stats)
                        all_mappings[collection.name] = stats['id_mapping']
                    else:
                        logger.info(f"Dry run - would migrate {collection.name}")

            # Step 4: Save ID mappings
            if not dry_run and all_mappings:
                mapping_file = await self.create_id_mapping_file(all_mappings)
                logger.info(f"ID mappings saved: {mapping_file}")

            # Step 5: Verify migration
            if not dry_run and all_stats:
                success = await self.verify_migration(all_stats)
                if not success:
                    logger.error("Migration verification failed!")
                    return False

            # Print summary
            logger.info("\n=== Migration Summary ===")
            total_md5 = sum(s['md5_ids'] for s in all_stats)
            total_migrated = sum(s['migrated'] for s in all_stats)
            total_errors = sum(s['errors'] for s in all_stats)

            logger.info(f"Total MD5 IDs found: {total_md5}")
            logger.info(f"Successfully migrated: {total_migrated}")
            logger.info(f"Errors: {total_errors}")

            return total_errors == 0

        except Exception as e:
            logger.error(f"Migration failed: {e}", exc_info=True)
            return False


async def main():
    """Main entry point"""
    import argparse
    import os

    parser = argparse.ArgumentParser(description="Migrate conversation IDs from MD5 to SHA-256")
    parser.add_argument('--url', default='http://localhost:6333', help='Qdrant URL')
    parser.add_argument('--api-key', help='Qdrant API key')
    parser.add_argument('--dry-run', action='store_true', help='Perform a dry run without making changes')

    args = parser.parse_args()

    # Get API key from environment if not provided
    api_key = args.api_key or os.getenv('QDRANT_API_KEY')

    # Create migration tool
    migrator = IdMigrationTool(args.url, api_key)

    # Run migration
    success = await migrator.run_migration(dry_run=args.dry_run)

    if success:
        logger.info("✅ Migration completed successfully!")
    else:
        logger.error("❌ Migration failed! Check logs for details.")
        exit(1)


if __name__ == "__main__":
    asyncio.run(main())